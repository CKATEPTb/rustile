mod appearance;
mod capture;
mod conflict;
mod header;
mod keycaps;
mod slots;
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
use crate::settings::{Assignment, BindingConflicts, Chord, Command, Settings, Shortcut};

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
const EDIT_ROW: i32 = 1200;
const ADD_ROW: i32 = 1300;
const REMOVE_ROW: i32 = 1400;
const ALT_ROW: i32 = 2000;
const ALT_EDIT: i32 = 2100;
const ALT_REMOVE: i32 = 2200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Row,
    Edit,
    Remove,
    Add,
    Reset,
}

fn row_control(id: i32) -> Option<(Control, Shortcut)> {
    let kind = match id / 100 * 100 {
        ROW | ALT_ROW => Control::Row,
        EDIT_ROW | ALT_EDIT => Control::Edit,
        REMOVE_ROW | ALT_REMOVE => Control::Remove,
        ADD_ROW => Control::Add,
        RESET_ROW => Control::Reset,
        _ => return None,
    };
    let action = *Shortcut::ALL.get((id % 100) as usize)?;
    action.is_editable().then_some((kind, action))
}

#[derive(Clone, Copy)]
struct Slot {
    row: HWND,
    edit: HWND,
    remove: HWND,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn child(hwnd: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(hwnd, id) }
}

fn caption(hwnd: HWND, s: &str) {
    unsafe { SetWindowTextW(hwnd, wide(s).as_ptr()) };
}

pub(super) struct ShortcutWindow {
    hwnd: Cell<HWND>,
    owner: Cell<HWND>,
    dpi: Cell<u32>,
    settings: RefCell<Settings>,
    slots: RefCell<[Vec<Slot>; Shortcut::ALL.len()]>,
    tooltip: Cell<HWND>,
    selected_slot: Cell<usize>,
    after_save: Cell<Option<(Shortcut, Chord)>>,
    selected: Cell<Shortcut>,
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
            settings: RefCell::default(),
            slots: RefCell::new(std::array::from_fn(|_| Vec::new())),
            tooltip: Cell::default(),
            selected_slot: Cell::new(0),
            after_save: Cell::default(),
            selected: Cell::new(view::GROUPS[0].actions[0]),
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
                let mut size = RECT {
                    right: self.p(1408),
                    bottom: self.p(852),
                    ..Default::default()
                };
                AdjustWindowRectExForDpi(&mut size, STYLE, 0, WS_EX_CONTROLPARENT, self.dpi.get());
                let area = info.rcWork;
                let width = (size.right - size.left).min(area.right - area.left);
                let height = (size.bottom - size.top).min(area.bottom - area.top);
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
                self.update_rows();
                self.layout(hwnd);
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
            SetFocus(self.selected_row());
        }
        Ok(())
    }

    pub(super) fn set_settings(&self, settings: Settings) {
        if *self.settings.borrow() == settings {
            return;
        }
        self.settings.replace(settings);
        let hwnd = self.hwnd.get();
        if !hwnd.is_null() {
            self.end_capture();
            if let Some((action, chord)) = self.after_save.take()
                && action == self.selected.get()
                && let Some(index) = self
                    .settings
                    .borrow()
                    .bindings(action)
                    .iter()
                    .position(|&c| c == chord)
            {
                self.selected_slot.set(index);
            }
            self.update_rows();
            self.layout(hwnd);
            let foreground = unsafe { GetForegroundWindow() };
            if foreground == hwnd || foreground.is_null() {
                let focus = unsafe { GetFocus() };
                if unsafe { IsChild(hwnd, focus) } != 0
                    && row_control(unsafe { GetDlgCtrlID(focus) })
                        .is_some_and(|(kind, _)| kind == Control::Row)
                {
                    unsafe { SetFocus(self.selected_row()) };
                }
            }
        }
    }

    pub(super) fn take_command(&self) -> Option<Command> {
        self.command.take()
    }

    pub(super) fn result(&self, error: Option<&str>) {
        self.saving.set(false);
        self.after_save.set(None);
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
            if let Some((control, action)) = row_control(id) {
                let slot = unsafe { GetWindowLongPtrW(msg.hwnd, GWLP_USERDATA) as usize };
                if matches!(control, Control::Edit | Control::Row)
                    && matches!(key, VK_RETURN | VK_F2)
                {
                    if !down {
                        self.start_capture(action, slot, false);
                    }
                    return true;
                }
            }
            if key == VK_RETURN && (row_control(id).is_some() || id == RESET_ALL) {
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

    fn recording(&self, action: Shortcut) -> bool {
        self.selected.get() == action && self.capture.borrow().is_some()
    }

    fn update_row(&self, action: Shortcut) {
        let bindings = self.settings.borrow().bindings(action);
        let selected = self.selected.get() == action;
        let recording = selected && self.capture.borrow().is_some();
        let pos = self.selected_slot.get();
        let count = if recording {
            bindings.len().max(pos + 1)
        } else {
            bindings.len()
        }
        .max(1);
        if selected && pos >= count {
            self.selected_slot.set(count - 1);
        }
        if let Err(error) = self.sync_slots(action, count) {
            self.set_status(&error);
            return;
        }
        let mut legends = bindings
            .iter()
            .map(|chord| chord.label())
            .collect::<Vec<_>>();
        legends.resize(count, "Не назначено".into());
        if recording && let Some(rec) = self.capture.borrow().as_ref() {
            legends[pos] = rec.keys.label();
        }
        let slots = self.slots.borrow()[action.index()].clone();
        let category = view::group_title(action);
        let base = format!("{}, {}", action.label(), legends.join(" или "));
        for (i, (&slot, value)) in slots.iter().zip(&legends).enumerate() {
            let detail = format!("{}, {}", action.label(), value);
            caption(slot.row, if i == 0 { &base } else { &detail });
            caption(
                slot.edit,
                &format!("Изменить {value} для «{}» — {category}", action.label()),
            );
            caption(
                slot.remove,
                &format!("Удалить {value} для «{}» — {category}", action.label()),
            );
            unsafe {
                EnableWindow(slot.edit, i32::from(!self.saving.get()));
                EnableWindow(
                    slot.remove,
                    i32::from(!self.saving.get() && i < bindings.len()),
                );
                InvalidateRect(slot.row, null(), 0);
            }
            if selected && i == self.selected_slot.get() {
                let preview = child(self.hwnd.get(), PREVIEW);
                caption(preview, &detail);
                unsafe { InvalidateRect(preview, null(), 0) };
            }
        }
        let view = child(self.hwnd.get(), VIEW);
        unsafe {
            EnableWindow(
                child(view, ADD_ROW + action.index() as i32),
                i32::from(!self.saving.get()),
            );
            EnableWindow(
                child(view, RESET_ROW + action.index() as i32),
                i32::from(!self.saving.get() && bindings != action.factory_bindings()),
            );
        }
    }

    fn update_rows(&self) {
        for group in &view::GROUPS {
            for &action in group.actions {
                self.update_row(action);
            }
        }
        let settings = self.settings.borrow();
        let can_reset = settings
            .changed(Command::ResetShortcuts)
            .is_ok_and(|next| next != *settings);
        unsafe {
            EnableWindow(
                child(self.hwnd.get(), RESET_ALL),
                i32::from(can_reset && !self.saving.get()),
            );
        }
    }

    fn select(&self, action: Shortcut, slot: usize) {
        let old = self.selected.get();
        if old != action || self.selected_slot.get() != slot {
            self.end_capture();
            self.selected.set(action);
            self.selected_slot.set(slot);
            self.update_row(action);
            if old != action {
                for slot in &self.slots.borrow()[old.index()] {
                    unsafe { InvalidateRect(slot.row, null(), 0) };
                }
            }
            self.set_status("");
        }
    }

    fn start_capture(&self, action: Shortcut, index: usize, append: bool) {
        if self.saving.get() || !self.conflict.get().is_null() {
            return;
        }
        if self.recording(action) && self.selected_slot.get() == index {
            return;
        }
        self.end_capture();
        let bindings = self.settings.borrow().bindings(action);
        let slot = if append {
            bindings.len()
        } else {
            index.min(bindings.len())
        };
        let original = bindings.get(slot).copied();
        self.select(action, slot.min(bindings.len().saturating_sub(1)));
        match capture::Capture::start(self.hwnd.get(), original) {
            Ok(capture) => {
                self.selected_slot.set(slot);
                self.capture.replace(Some(capture));
                self.update_row(action);
                self.layout(self.hwnd.get());
                unsafe { SetFocus(self.selected_row()) };
                self.reveal_row(self.hwnd.get(), self.selected_row());
                self.set_status("");
            }
            Err(e) => self.result(Some(&e)),
        }
    }

    fn end_capture(&self) {
        if self.capture.take().is_some() {
            self.update_row(self.selected.get());
            self.layout(self.hwnd.get());
            self.set_status("");
        }
    }

    fn capture_key(&self, key: u16, down: bool) {
        use capture::Event;
        let event = self
            .capture
            .borrow_mut()
            .as_mut()
            .map(|c| c.keys.key(key, down));
        let action = self.selected.get();
        match event {
            None | Some(Event::Unchanged) => {}
            Some(Event::Changed) => self.update_row(action),
            Some(Event::Cancel) => self.end_capture(),
            Some(Event::Ambiguous) => {
                self.end_capture();
                self.result(Some(
                    "Используйте одну основную клавишу вместе с модификаторами.",
                ));
            }
            Some(Event::Complete(chord)) => {
                let original = self.capture.borrow().as_ref().and_then(|rec| rec.original);
                self.end_capture();
                let assignment = match original {
                    Some(old) => Assignment::Edit(action, old, chord),
                    None => Assignment::Add(action, chord),
                };
                self.submit(Command::Assign(assignment));
            }
        }
    }

    fn submit(&self, command: Command) {
        if self.saving.get() || !self.conflict.get().is_null() {
            return;
        }
        self.end_capture();
        let settings = self.settings.borrow().clone();
        if let Command::Assign(assignment) = command {
            match settings.conflicts(assignment) {
                Ok(owners) if !owners.is_empty() => {
                    if let Err(error) = self.ask_transfer(assignment, owners) {
                        self.result(Some(&error));
                    }
                    return;
                }
                Err(error) => {
                    self.result(Some(&error));
                    return;
                }
                _ => {}
            }
        }
        match settings.changed(command) {
            Err(e) => self.result(Some(&e)),
            Ok(next) if next == settings => {}
            Ok(_) => {
                let target = match command {
                    Command::Assign(assignment) | Command::Transfer(assignment, _) => {
                        match assignment {
                            Assignment::Add(action, chord) | Assignment::Edit(action, _, chord) => {
                                Some((action, chord))
                            }
                            Assignment::Reset(_) => None,
                        }
                    }
                    _ => None,
                };
                self.after_save.set(target);
                self.saving.set(true);
                self.command.set(Some(command));
                self.update_rows();
                unsafe { PostMessageW(self.owner.get(), WM_NULL, 0, 0) };
            }
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
                if let Some((control, action)) = row_control(id) {
                    let pos = GetWindowLongPtrW(lp as HWND, GWLP_USERDATA) as usize;
                    if event == BN_SETFOCUS {
                        if control != Control::Row {
                            ui.end_capture();
                        }
                        ui.select(action, pos);
                        ui.reveal_row(hwnd, lp as HWND);
                    } else if event == BN_CLICKED {
                        match control {
                            Control::Row => ui.select(action, pos),
                            Control::Edit => ui.start_capture(action, pos, false),
                            Control::Add => ui.start_capture(action, pos, true),
                            Control::Remove | Control::Reset => {
                                ui.select(action, pos);
                                SetFocus(ui.selected_row());
                                let old = ui.settings.borrow().bindings(action).get(pos).copied();
                                if control == Control::Reset {
                                    ui.submit(Command::Assign(Assignment::Reset(action)));
                                } else if let Some(chord) = old {
                                    ui.submit(Command::RemoveShortcut(action, chord));
                                }
                            }
                        }
                    }
                } else if id == RESET_ALL {
                    if event == BN_CLICKED {
                        SetFocus(ui.selected_row());
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
                ui.slots.borrow_mut().iter_mut().for_each(Vec::clear);
                ui.tooltip.set(null_mut());
                ui.after_save.set(None);
                ui.selected_slot.set(0);
                ui.hwnd.set(null_mut());
                ui.scroll.set(0);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
