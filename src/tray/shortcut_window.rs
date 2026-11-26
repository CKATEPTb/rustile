mod appearance;
mod capture;
mod conflict;
mod header;
mod scheme;
mod view;

use std::{
    cell::{Cell, RefCell},
    io,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::w,
};

use super::{
    paint::{self, Colors, Fonts},
    scale,
};
use crate::settings::{BindingOwners, Chord, Command, SchemeKey, Settings, Shortcut};

const CLASS: *const u16 = w!("Rustile.Shortcuts");
const TITLE: *const u16 = w!("Горячие клавиши — Rustile");
const STYLE: u32 = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
const RESET_ALL: i32 = 505;
const STATUS: i32 = 506;
const VIEW: i32 = 510;
const PREVIEW: i32 = 511;
const SECTION: i32 = 520;
const ROW: i32 = 1000;
const RESET_ROW: i32 = 1100;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn child(hwnd: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(hwnd, id) }
}

fn caption(hwnd: HWND, s: &str) {
    unsafe { SetWindowTextW(hwnd, wide(s).as_ptr()) };
}

fn row_id(action: Shortcut) -> i32 {
    ROW + action.index() as i32
}

fn row_action(id: i32) -> Option<Shortcut> {
    Shortcut::ALL.get(id.checked_sub(ROW)? as usize).copied()
}

fn reset_action(id: i32) -> Option<Shortcut> {
    Shortcut::ALL
        .get(id.checked_sub(RESET_ROW)? as usize)
        .copied()
}

pub(super) struct ShortcutWindow {
    hwnd: Cell<HWND>,
    owner: Cell<HWND>,
    dpi: Cell<u32>,
    settings: Cell<Settings>,
    selected: Cell<Shortcut>,
    role: Cell<SchemeKey>,
    advanced: Cell<bool>,
    command: Cell<Option<Command>>,
    saving: Cell<bool>,
    capture: RefCell<Option<capture::Capture>>,
    conflict: Cell<HWND>,
    transfer: Cell<Option<Command>>,
    scroll: Cell<i32>,
    colors: Cell<Colors>,
    fonts: RefCell<Option<Fonts>>,
}

impl Default for ShortcutWindow {
    fn default() -> Self {
        Self {
            hwnd: Cell::default(),
            owner: Cell::default(),
            dpi: Cell::new(96),
            settings: Cell::new(Settings::default()),
            selected: Cell::new(Shortcut::MoveLeft),
            role: Cell::new(SchemeKey::Base),
            advanced: Cell::new(false),
            command: Cell::default(),
            saving: Cell::new(false),
            capture: RefCell::default(),
            conflict: Cell::default(),
            transfer: Cell::default(),
            scroll: Cell::new(0),
            colors: Cell::new(Colors::current()),
            fonts: RefCell::default(),
        }
    }
}

impl Drop for ShortcutWindow {
    fn drop(&mut self) {
        if !self.hwnd.get().is_null() {
            unsafe { DestroyWindow(self.hwnd.get()) };
        }
    }
}

impl ShortcutWindow {
    pub(super) fn show(
        &self,
        owner: HWND,
        anchor: POINT,
        settings: Settings,
    ) -> Result<(), String> {
        if !self.conflict.get().is_null() {
            unsafe { SetForegroundWindow(self.conflict.get()) };
            return Ok(());
        }
        self.owner.set(owner);
        self.set_settings(settings);
        let mut hwnd = self.hwnd.get();
        unsafe {
            if hwnd.is_null() {
                let instance = GetModuleHandleW(null());
                let class = WNDCLASSW {
                    hInstance: instance,
                    lpszClassName: CLASS,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                    hIcon: LoadIconW(instance, 101usize as _),
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                    return Err(io::Error::last_os_error().to_string());
                }
                let monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(monitor, &mut info) == 0 {
                    return Err(io::Error::last_os_error().to_string());
                }
                let (mut dpi, mut y_dpi) = (96, 96);
                GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut y_dpi);
                self.dpi.set(dpi.max(96));
                self.advanced.set(false);
                let area = info.rcWork;
                let (width, height) = self.preferred_size(false, area);
                hwnd = CreateWindowExW(
                    WS_EX_CONTROLPARENT,
                    CLASS,
                    TITLE,
                    STYLE,
                    area.left + (area.right - area.left - width) / 2,
                    area.top + (area.bottom - area.top - height) / 2,
                    width,
                    height,
                    null_mut(),
                    null_mut(),
                    instance,
                    (self as *const Self).cast(),
                );
                if hwnd.is_null() {
                    return Err(format!(
                        "Не удалось открыть горячие клавиши. {}",
                        io::Error::last_os_error()
                    ));
                }
                if let Err(e) = self.create_controls(hwnd) {
                    DestroyWindow(hwnd);
                    return Err(e);
                }
                self.update_fonts(hwnd);
                self.apply_theme(hwnd);
                self.scheme_visibility();
                self.update_rows();
            }
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetWindowPos(
                hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            SetForegroundWindow(hwnd);
            SetFocus(self.selected_control());
        }
        Ok(())
    }

    pub(super) fn set_settings(&self, settings: Settings) {
        if self.settings.replace(settings) != settings && !self.hwnd.get().is_null() {
            self.update_rows();
        }
    }

    pub(super) fn take_command(&self) -> Option<Command> {
        self.command.take()
    }

    pub(super) fn result(&self, error: Option<&str>) {
        self.saving.set(false);
        let hwnd = self.hwnd.get();
        if !hwnd.is_null() {
            self.update_rows();
            self.set_status(error.unwrap_or_default());
        }
    }

    pub(super) fn pre_translate(&self, msg: &MSG) -> bool {
        if capture::Capture::take_delivery_failure() {
            self.end_capture();
        }
        let dialog = self.conflict.get();
        if !dialog.is_null() && (msg.hwnd == dialog || unsafe { IsChild(dialog, msg.hwnd) } != 0) {
            if msg.wParam == usize::from(VK_RETURN)
                && let Some(down) = capture::is_key_down(msg.message)
            {
                if !down {
                    let accept = unsafe { GetFocus() } == child(dialog, IDOK);
                    self.finish_transfer(dialog, accept);
                }
                return true;
            }
            return unsafe { IsDialogMessageW(dialog, msg) } != 0;
        }
        let hwnd = self.hwnd.get();
        if hwnd.is_null() || (msg.hwnd != hwnd && unsafe { IsChild(hwnd, msg.hwnd) } == 0) {
            return false;
        }
        if msg.message == capture::KEY_EVENT {
            return false; // The editor handles its hook notifications directly.
        }
        if let Some(down) = capture::is_key_down(msg.message) {
            let key = msg.wParam as u16;
            if self.capture.borrow().is_some() {
                self.capture_key(key, down);
                return true;
            }
            let id = unsafe { GetDlgCtrlID(msg.hwnd) };
            if (row_action(id).is_some() || scheme::role_from_id(id).is_some())
                && matches!(key, VK_RETURN | VK_F2)
            {
                // Start on release so the activation key cannot bind itself.
                if !down {
                    self.start_capture();
                }
                return true;
            }
            if key == VK_RETURN
                && (reset_action(id).is_some()
                    || scheme::reset_from_id(id).is_some()
                    || matches!(id, RESET_ALL | scheme::PAGE_SWITCH))
            {
                // Clicking can move focus to an editable row. Consume Enter's
                // release here so it cannot immediately start recording there.
                if !down {
                    unsafe { SendMessageW(msg.hwnd, BM_CLICK, 0, 0) };
                }
                return true;
            }
            if key == VK_ESCAPE && down && msg.lParam & (1 << 30) != 0 {
                return true;
            }
        }
        unsafe { IsDialogMessageW(hwnd, msg) != 0 }
    }

    fn p(&self, dip: i32) -> i32 {
        scale(dip, self.dpi.get())
    }

    fn row(&self, action: Shortcut) -> HWND {
        child(child(self.hwnd.get(), VIEW), row_id(action))
    }

    fn selected_control(&self) -> HWND {
        if self.advanced.get() {
            self.row(self.selected.get())
        } else {
            child(
                child(self.hwnd.get(), VIEW),
                scheme::role_id(self.role.get()),
            )
        }
    }

    fn preferred_size(&self, advanced: bool, area: RECT) -> (i32, i32) {
        let (width, height) = if advanced { (1232, 852) } else { (864, 566) };
        let mut size = RECT {
            right: self.p(width),
            bottom: self.p(height),
            ..Default::default()
        };
        unsafe {
            AdjustWindowRectExForDpi(&mut size, STYLE, 0, WS_EX_CONTROLPARENT, self.dpi.get())
        };
        (
            (size.right - size.left).min(area.right - area.left),
            (size.bottom - size.top).min(area.bottom - area.top),
        )
    }

    fn switch_page(&self) {
        let hwnd = self.hwnd.get();
        self.end_capture();
        self.set_status("");
        let was_advanced = self.advanced.get();
        self.advanced.set(!was_advanced);
        self.scroll.set(0);
        unsafe {
            let mut monitor = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let mut rect = RECT::default();
            if IsZoomed(hwnd) == 0
                && GetWindowRect(hwnd, &mut rect) != 0
                && GetMonitorInfoW(
                    MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                    &mut monitor,
                ) != 0
            {
                let area = monitor.rcWork;
                let size = (rect.right - rect.left, rect.bottom - rect.top);
                // Respect a user-sized window. Only switch between our own preferred sizes.
                if size == self.preferred_size(was_advanced, area) {
                    let (w, h) = self.preferred_size(!was_advanced, area);
                    let x = (rect.left + (size.0 - w) / 2).clamp(area.left, area.right - w);
                    let y = (rect.top + (size.1 - h) / 2).clamp(area.top, area.bottom - h);
                    SetWindowPos(hwnd, null_mut(), x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
                }
            }
        }
        self.scheme_visibility();
        self.update_rows();
        unsafe { SetFocus(self.selected_control()) };
    }

    fn recording(&self, action: Shortcut) -> bool {
        self.advanced.get() && self.selected.get() == action && self.capture.borrow().is_some()
    }

    fn recording_role(&self) -> bool {
        !self.advanced.get() && self.capture.borrow().is_some()
    }

    fn role_caption(&self, role: SchemeKey) -> String {
        if !self.advanced.get()
            && self.role.get() == role
            && let Some(capture) = self.capture.borrow().as_ref()
        {
            return capture.keys.single_label();
        }
        Chord::key_label(self.settings.get().scheme_key(role))
    }

    fn binding_text(&self, action: Shortcut) -> String {
        if self.advanced.get()
            && self.selected.get() == action
            && let Some(capture) = self.capture.borrow().as_ref()
        {
            return capture.keys.label();
        }
        self.settings
            .get()
            .binding(action)
            .map(|chord| chord.label())
            .unwrap_or_else(|| "Не назначено".into())
    }

    fn update_row(&self, action: Shortcut) {
        let row = self.row(action);
        let name = format!("{}, {}", action.label(), self.binding_text(action));
        caption(row, &name);
        unsafe { InvalidateRect(row, null(), 0) };
        if self.advanced.get() && action == self.selected.get() {
            let preview = child(self.hwnd.get(), PREVIEW);
            caption(preview, &name);
            unsafe { InvalidateRect(preview, null(), 0) };
        }
    }

    fn update_rows(&self) {
        let hwnd = self.hwnd.get();
        let view = child(hwnd, VIEW);
        let settings = self.settings.get();
        for action in Shortcut::ALL {
            self.update_row(action);
            let changed = settings.has_shortcut_override(action);
            unsafe {
                EnableWindow(
                    child(view, RESET_ROW + action.index() as i32),
                    (changed && !self.saving.get()) as i32,
                )
            };
        }
        // Profiles in older files can differ even when individual overrides
        // happen to reproduce all factory chords.
        let can_reset = settings
            .changed(Command::ResetShortcuts)
            .is_ok_and(|s| s != settings);
        unsafe {
            EnableWindow(
                child(hwnd, RESET_ALL),
                (can_reset && !self.saving.get()) as i32,
            )
        };
        self.update_scheme();
    }

    fn select(&self, action: Shortcut) {
        let old = self.selected.get();
        if old != action {
            self.end_capture();
            self.selected.set(action);
            self.update_row(action);
            unsafe {
                InvalidateRect(self.row(old), null(), 0);
            }
            self.set_status("");
        }
    }

    fn select_role(&self, role: SchemeKey) {
        if self.role.get() != role {
            self.end_capture();
            self.role.set(role);
            self.update_scheme();
            self.set_status("");
        }
    }

    fn refresh_capture(&self) {
        if self.advanced.get() {
            self.update_row(self.selected.get());
        } else {
            self.update_scheme();
        }
    }

    fn start_capture(&self) {
        if self.saving.get() || !self.conflict.get().is_null() || self.capture.borrow().is_some() {
            return;
        }
        unsafe { SetFocus(self.selected_control()) };
        match capture::Capture::start(self.hwnd.get(), !self.advanced.get()) {
            Ok(capture) => {
                self.capture.replace(Some(capture));
                self.refresh_capture();
                self.set_status("");
            }
            Err(e) => self.result(Some(&e)),
        }
    }

    fn end_capture(&self) {
        if self.capture.take().is_some() {
            self.refresh_capture();
            self.set_status("");
        }
    }

    fn capture_key(&self, key: u16, down: bool) {
        use capture::Event;
        let event = self.capture.borrow_mut().as_mut().map(|c| {
            if self.advanced.get() {
                c.keys.key(key, down)
            } else {
                c.keys.single(key, down)
            }
        });
        let action = self.selected.get();
        match event {
            None | Some(Event::Unchanged) => {}
            Some(Event::Changed) => self.refresh_capture(),
            Some(Event::Cancel) => self.end_capture(),
            Some(Event::Ambiguous) => {
                self.end_capture();
                self.result(Some(if self.advanced.get() {
                    "Используйте одну основную клавишу вместе с модификаторами."
                } else {
                    "Для этой роли нажмите одну клавишу."
                }));
            }
            Some(Event::Complete(chord)) => {
                self.end_capture();
                self.submit(Command::Shortcut(action, chord));
            }
            Some(Event::Single(key)) => {
                self.end_capture();
                self.change_role(self.role.get(), key);
            }
        }
    }

    fn submit(&self, command: Command) {
        if self.saving.get() || !self.conflict.get().is_null() {
            return;
        }
        self.end_capture();
        let settings = self.settings.get();
        let assignment = match command {
            Command::Shortcut(action, chord) => Some((action, chord)),
            Command::ResetShortcut(action) => Some((action, settings.inherited_chord(action))),
            _ => None,
        };
        if let Some((action, chord)) = assignment
            && settings.binding(action) != Some(chord)
            && let Some(previous) = settings.conflict(action, chord)
        {
            if let Err(error) = self.ask_transfer(action, chord, previous) {
                self.result(Some(&error));
            }
            return;
        }
        match settings.changed(command) {
            Err(e) => self.result(Some(&e)),
            Ok(next) if next == settings => {}
            Ok(_) => {
                self.saving.set(true);
                self.command.set(Some(command));
                self.update_rows();
                unsafe { PostMessageW(self.owner.get(), WM_NULL, 0, 0) };
            }
        }
    }

    fn change_role(&self, role: SchemeKey, key: u16) {
        let settings = self.settings.get();
        let command = Command::SchemeKey(role, key);
        if let Some(partner) = settings.scheme_partner(role, key) {
            let old_key = Chord::key_label(settings.scheme_key(role));
            let new_key = Chord::key_label(settings.scheme_key(partner));
            let chord = format!("{old_key} ↔ {new_key}");
            let message = format!(
                "{} — {new_key}\r\n{} — {old_key}",
                role.label(),
                partner.label()
            );
            if let Err(e) = self.confirm_change(command, &chord, &message) {
                self.result(Some(&e));
            }
        } else {
            self.submit(command);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lp as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            (*(create.lpCreateParams as *const ShortcutWindow))
                .hwnd
                .set(hwnd);
        }
        let Some(ui) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ShortcutWindow).as_ref()
        else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        match msg {
            WM_GETMINMAXINFO if lp != 0 => {
                let limits = &mut *(lp as *mut MINMAXINFO);
                let mut display = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                if GetMonitorInfoW(monitor, &mut display) != 0 {
                    limits.ptMinTrackSize = POINT {
                        x: ui.p(340).min(display.rcWork.right - display.rcWork.left),
                        y: ui.p(320).min(display.rcWork.bottom - display.rcWork.top),
                    };
                }
                0
            }
            WM_SIZE if !child(hwnd, VIEW).is_null() => {
                ui.layout(hwnd);
                0
            }
            WM_DPICHANGED => {
                let rect = &*(lp as *const RECT);
                ui.dpi.set((wp as u32 & 0xffff).max(96));
                ui.update_fonts(hwnd);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                ui.layout(hwnd);
                0
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                ui.apply_theme(hwnd);
                0
            }
            capture::KEY_EVENT => {
                let current = ui
                    .capture
                    .borrow()
                    .as_ref()
                    .is_some_and(|c| c.generation == wp);
                if current {
                    ui.capture_key(lp as u16, lp & (1 << 16) != 0);
                }
                0
            }
            WM_ACTIVATE if (wp & 0xffff) == WA_INACTIVE as usize => {
                ui.end_capture();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_COMMAND => {
                let id = (wp & 0xffff) as i32;
                let event = (wp >> 16) as u32;
                if id == scheme::PAGE_SWITCH && event == BN_CLICKED {
                    ui.switch_page();
                } else if let Some(role) = scheme::role_from_id(id) {
                    if matches!(event, BN_SETFOCUS | BN_CLICKED | BN_DOUBLECLICKED) {
                        ui.select_role(role);
                        if event == BN_SETFOCUS {
                            ui.reveal_row(hwnd, lp as HWND);
                        } else if event == BN_DOUBLECLICKED {
                            ui.start_capture();
                        }
                    }
                } else if let Some(role) = scheme::reset_from_id(id) {
                    if event == BN_CLICKED {
                        ui.select_role(role);
                        SetFocus(ui.selected_control());
                        ui.change_role(role, role.default_key());
                    } else if event == BN_SETFOCUS {
                        ui.end_capture();
                        ui.reveal_row(hwnd, lp as HWND);
                    }
                } else if let Some(action) = row_action(id) {
                    if event == BN_DOUBLECLICKED {
                        ui.select(action);
                        ui.start_capture();
                    } else if event == BN_CLICKED {
                        ui.select(action);
                    } else if event == BN_SETFOCUS {
                        ui.select(action);
                        ui.reveal_row(hwnd, lp as HWND);
                    }
                } else if let Some(action) = reset_action(id) {
                    if event == BN_CLICKED {
                        SetFocus(ui.row(action));
                        ui.submit(Command::ResetShortcut(action));
                    } else if event == BN_SETFOCUS {
                        ui.end_capture();
                        ui.reveal_row(hwnd, lp as HWND);
                    }
                } else if id == PREVIEW && event == STN_DBLCLK {
                    ui.start_capture();
                } else if id == RESET_ALL {
                    if event == BN_CLICKED {
                        SetFocus(ui.selected_control());
                        ui.submit(Command::ResetShortcuts);
                    } else if event == BN_SETFOCUS {
                        ui.end_capture();
                    }
                } else if id == IDCANCEL && lp == 0 {
                    if ui.capture.borrow().is_some() {
                        ui.end_capture();
                    } else {
                        DestroyWindow(hwnd);
                    }
                }
                0
            }
            WM_DRAWITEM if lp != 0 => {
                let item = &*(lp as *const DRAWITEMSTRUCT);
                if ui.draw_scheme(item) {
                    return 1;
                }
                match item.CtlID as i32 {
                    PREVIEW => ui.draw_preview(item),
                    RESET_ALL => ui.draw_reset_all(item),
                    _ => ui.draw(item),
                }
                1
            }
            WM_ERASEBKGND => {
                ui.background(hwnd, wp as HDC);
                1
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let colors = ui.colors.get();
                let status = GetDlgCtrlID(lp as HWND) == STATUS;
                let surface = if status {
                    ui.header_color()
                } else {
                    colors.background
                };
                SetBkColor(wp as HDC, surface);
                SetTextColor(
                    wp as HDC,
                    if status {
                        colors.secondary
                    } else {
                        colors.text
                    },
                );
                SetDCBrushColor(wp as HDC, surface);
                GetStockObject(DC_BRUSH) as isize
            }
            WM_MOUSEWHEEL => {
                ui.wheel(hwnd, (wp >> 16) as i16 as i32);
                0
            }
            WM_NCDESTROY => {
                ui.capture.take();
                ui.hwnd.set(null_mut());
                ui.scroll.set(0);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
