mod exclusion_window;
mod menu;
mod paint;
mod shortcut_window;
mod stepper;

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    io,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    },
    Graphics::{Dwm::*, Gdi::*},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{DRAWITEMSTRUCT, WM_MOUSELEAVE},
        HiDpi::{GetDpiForMonitor, GetDpiForSystem, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI},
        Input::KeyboardAndMouse::*,
        Shell::*,
        WindowsAndMessaging::*,
    },
};
use windows_sys::core::w;

use crate::i18n::{Msg, render, tr};
use crate::settings::{Command, Settings, exclusions::ExclusionRule};
use crate::startup::Startup;
use exclusion_window::ExclusionWindow;
use menu::{Action, Row};
use paint::Painter;
pub(crate) use paint::{Colors, Fonts};
use shortcut_window::ShortcutWindow;
use stepper::{EDIT_ID, EndEdit, Inline, arrow_info};

const CALLBACK: u32 = WM_APP + 20;
const ICON_ID: u32 = 1;
const HEADER_ICON_DIP: i32 = 20;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const RETRY: usize = 1;
const DISMISS_AFTER_ICON_PRESS: usize = 2;
const CLASS: *const u16 = w!("Rustile.Tray");

fn scale(dip: i32, dpi: u32) -> i32 {
    (dip * dpi as i32 + 48) / 96
}

pub(crate) fn load_app_icon(width: i32, height: i32) -> HICON {
    unsafe {
        LoadImageW(
            GetModuleHandleW(null()),
            101usize as _,
            IMAGE_ICON,
            width,
            height,
            0,
        )
    }
}

struct State {
    settings: RefCell<Settings>,
    startup: Cell<Option<Startup>>,
    startup_pending: Cell<bool>,
    commands: RefCell<VecDeque<Command>>,
    applying: Cell<Option<Command>>,
    tray_icon: HICON,
    header_icon: Cell<Option<(i32, HICON)>>,
    taskbar_created: u32,
    work_area_changed: Cell<bool>,
    registered: Cell<bool>,
    attempts: Cell<u8>,
    shortcuts: ShortcutWindow,
    exclusions: ExclusionWindow,
    anchor: Cell<POINT>,
    icon_bounds: Cell<Option<RECT>>,
    icon_pressed: Cell<bool>,
    dismiss_pending: Cell<bool>,
    dpi: Cell<u32>,
    colors: Cell<Colors>,
    fonts: RefCell<Option<Fonts>>,
    entries: RefCell<Vec<Row>>,
    buttons: RefCell<Vec<HWND>>,
    hot: Cell<HWND>,
    keyboard: Cell<bool>,
    inline: Inline,
}

pub struct Tray {
    hwnd: HWND,
    // GWLP_USERDATA uses this stable address until DestroyWindow returns.
    state: Box<State>,
}

impl Tray {
    pub fn new(settings: Settings) -> Result<Self, String> {
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = WNDCLASSW {
                hInstance: instance,
                lpszClassName: CLASS,
                lpfnWndProc: Some(tray_proc),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(render(
                    Msg::TrayMenuRegister,
                    &[("error", &io::Error::last_os_error().to_string())],
                ));
            }
            let dpi = GetDpiForSystem().max(96);
            let icon = load_app_icon(
                GetSystemMetricsForDpi(SM_CXSMICON, dpi),
                GetSystemMetricsForDpi(SM_CYSMICON, dpi),
            );
            if icon.is_null() {
                return Err(render(
                    Msg::TrayIconLoad,
                    &[("error", &io::Error::last_os_error().to_string())],
                ));
            }
            let state = Box::new(State {
                settings: RefCell::new(settings),
                startup: Cell::new(None),
                startup_pending: Cell::new(false),
                commands: RefCell::default(),
                applying: Cell::new(None),
                tray_icon: icon,
                header_icon: Cell::default(),
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                work_area_changed: Cell::new(false),
                registered: Cell::new(false),
                attempts: Cell::new(0),
                shortcuts: ShortcutWindow::default(),
                exclusions: ExclusionWindow::default(),
                anchor: Cell::default(),
                icon_bounds: Cell::default(),
                icon_pressed: Cell::new(false),
                dismiss_pending: Cell::new(false),
                dpi: Cell::new(dpi),
                colors: Cell::new(Colors::current()),
                fonts: RefCell::default(),
                entries: RefCell::default(),
                buttons: RefCell::default(),
                hot: Cell::default(),
                keyboard: Cell::new(false),
                inline: Inline::default(),
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                w!("Rustile"),
                WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                (&raw const *state).cast(),
            );
            if hwnd.is_null() {
                return Err(render(
                    Msg::TrayMenuCreate,
                    &[("error", &io::Error::last_os_error().to_string())],
                ));
            }
            state.register_icon(hwnd);
            Ok(Self { hwnd, state })
        }
    }

    pub fn set_settings(&self, settings: Settings) {
        self.state.applying.set(None);
        self.state.shortcuts.set_settings(settings.clone());
        let enabled = settings.enabled;
        let previous = self.state.settings.replace(settings);
        if previous.enabled != enabled && self.state.registered.get() {
            let mut data = self.state.icon_data(self.hwnd);
            data.uFlags = NIF_TIP | NIF_SHOWTIP;
            if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } == 0 {
                self.state.registered.set(false);
                self.state.register_icon(self.hwnd);
            }
        }
        if unsafe { IsWindowVisible(self.hwnd) } != 0 {
            self.state.refresh_rows(self.hwnd);
        }
    }

    pub fn shortcut_result(&self, error: Option<&str>) {
        self.state.shortcuts.result(error);
    }

    pub fn begin_startup_change(&self) -> bool {
        if self.state.startup_pending.replace(true) {
            return false;
        }
        if unsafe { IsWindowVisible(self.hwnd) } != 0 {
            self.state.refresh_rows(self.hwnd);
        }
        true
    }

    pub fn set_startup(&self, value: Option<Startup>) {
        let was_pending = self.state.startup_pending.replace(false);
        self.state.applying.set(None);
        let previous = self.state.startup.replace(value);
        if (!was_pending && previous == value) || unsafe { IsWindowVisible(self.hwnd) } == 0 {
            return;
        }
        let has_admin_row = |mode| matches!(mode, Some(Startup::Normal | Startup::Elevated));
        if has_admin_row(previous) == has_admin_row(value) {
            self.state.refresh_rows(self.hwnd);
            return;
        }
        // The buttons are reused by index. Inserting the admin row moves Exit
        // to a different HWND, so restore keyboard focus by its visible label.
        let focus = unsafe { GetFocus() };
        let focused = self
            .state
            .buttons
            .borrow()
            .iter()
            .position(|&btn| btn == focus)
            .and_then(|index| {
                self.state
                    .entries
                    .borrow()
                    .get(index)
                    .map(|r| r.label.clone())
            });
        self.state.layout(self.hwnd);
        if let Some(label) = focused {
            let entries = self.state.entries.borrow();
            let index = entries.iter().position(|r| r.label == label).or_else(|| {
                entries
                    .iter()
                    .position(|r| matches!(r.action, Action::Apply(Command::SetStartup(_))))
            });
            if let Some(index) = index {
                self.state.focus(self.hwnd, index);
            }
        }
    }

    pub fn pre_translate(&self, message: &MSG) -> bool {
        self.state.exclusions.pre_translate(message) || self.state.shortcuts.pre_translate(message)
    }

    pub fn take_exclusions(&self) -> Option<Vec<ExclusionRule>> {
        self.state.exclusions.take_rules()
    }

    pub fn exclusions_result(&self, result: Result<(), String>) {
        self.state.exclusions.finish_save(result);
    }

    pub fn take_work_area_change(&self) -> bool {
        self.state.work_area_changed.take()
    }

    pub fn take_command(&self) -> Option<Command> {
        let command = self
            .state
            .commands
            .borrow_mut()
            .pop_front()
            .or_else(|| self.state.shortcuts.take_command());
        self.state.applying.set(command);
        command
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            KillTimer(self.hwnd, RETRY);
            if self.state.registered.get() {
                Shell_NotifyIconW(NIM_DELETE, &self.state.icon_data(self.hwnd));
            }
            DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
pub(crate) fn check_work_area_notifications() {
    // Called only after the native smoke harness verifies its private desktop.
    let tray = Tray::new(Settings::default()).unwrap();
    let hwnd = tray.hwnd;
    let foreground = unsafe { GetForegroundWindow() };
    let drain_wakes = || unsafe {
        let mut msg = MSG::default();
        let mut wakes = 0;
        while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE) != 0 {
            if msg.message == WM_NULL {
                wakes += 1;
            } else {
                DispatchMessageW(&msg);
            }
        }
        wakes
    };
    drain_wakes();
    tray.take_work_area_change();
    let mut rect = RECT::default();
    assert_ne!(unsafe { GetWindowRect(hwnd, &mut rect) }, 0);
    for (msg, wp, lp) in [
        (WM_SETTINGCHANGE, SPI_SETWORKAREA as usize, 0),
        (WM_SETTINGCHANGE, 0, 0),
        (WM_DISPLAYCHANGE, 32, (1080 << 16) | 1920),
        (WM_DPICHANGED, (96 << 16) | 96, (&raw const rect) as isize),
        (tray.state.taskbar_created, 0, 0),
    ] {
        for _ in 0..3 {
            unsafe { SendMessageW(hwnd, msg, wp, lp) };
        }
        assert!(tray.take_work_area_change(), "missed notification {msg:#x}");
        assert!(!tray.take_work_area_change());
        assert_eq!(drain_wakes(), 1, "one wake-up per notification burst");
    }
    assert_eq!(unsafe { IsWindowVisible(hwnd) }, 0);
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            if let Some((_, icon)) = self.header_icon.get() {
                DestroyIcon(icon);
            }
            DestroyIcon(self.tray_icon);
        }
    }
}

impl State {
    fn menu_rows(&self) -> Vec<Row> {
        let mut rows = menu::rows(&self.settings.borrow(), self.startup.get());
        if self.startup_pending.get() {
            for row in &mut rows {
                if matches!(row.action, Action::Apply(Command::SetStartup(_))) {
                    row.detail = tr(Msg::TrayUpdating).into();
                }
            }
        }
        rows
    }

    fn icon_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_TIP | NIF_SHOWTIP | NIF_MESSAGE,
            hIcon: self.tray_icon,
            uCallbackMessage: CALLBACK,
            ..Default::default()
        };
        let tip = if self.settings.borrow().enabled {
            tr(Msg::TrayTipOn)
        } else {
            tr(Msg::TrayTipOff)
        };
        for (out, c) in data.szTip.iter_mut().zip(tip.encode_utf16()) {
            *out = c;
        }
        data
    }

    fn register_icon(&self, hwnd: HWND) {
        // FindWindow is confined to this Win32 desktop. A background test or
        // isolated desktop must never add its icon to the user's taskbar.
        unsafe {
            if FindWindowW(w!("Shell_TrayWnd"), null()).is_null() {
                KillTimer(hwnd, RETRY);
                return; // Explorer broadcasts TaskbarCreated when it starts.
            }
            let mut data = self.icon_data(hwnd);
            // The shell may retain an existing icon across a DPI-change broadcast.
            if Shell_NotifyIconW(NIM_ADD, &data) != 0 || Shell_NotifyIconW(NIM_MODIFY, &data) != 0 {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                if Shell_NotifyIconW(NIM_SETVERSION, &data) != 0 {
                    self.registered.set(true);
                    self.attempts.set(0);
                    KillTimer(hwnd, RETRY);
                    return;
                }
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
            let attempt = self.attempts.get().saturating_add(1);
            self.attempts.set(attempt);
            SetTimer(hwnd, RETRY, 1000 * u32::from(attempt).min(30), None);
            if attempt == 8 {
                eprintln!("{}", tr(Msg::TrayShellRetry));
            }
        }
    }

    fn show(&self, hwnd: HWND, point: POINT, keyboard: bool) {
        self.anchor.set(point);
        self.keyboard.set(keyboard);
        if !self.startup_pending.get() {
            self.startup.set(
                Startup::read()
                    .inspect_err(|e| eprintln!("Rustile: {e}"))
                    .ok(),
            );
        }
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
        let (mut x, mut y) = (96, 96);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
        let dpi = x.max(96);
        self.dpi.set(dpi);
        self.prepare_header_icon(dpi);
        *self.fonts.borrow_mut() = Some(Fonts::new(dpi));
        self.apply_theme(hwnd, Colors::current());
        self.layout(hwnd);
        unsafe {
            // Win32 paints visible child controls. Let DWM compose the popup
            // without presenting it until the owner-drawn rows are ready.
            let cloaked = set_popup_cloak(hwnd, true);
            // A launcher may pass STARTF_USESHOWWINDOW with SW_HIDE. The first
            // ShowWindow would then ignore its requested state.
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
            RedrawWindow(
                hwnd,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
            GdiFlush();
            if cloaked {
                DwmFlush();
                if !set_popup_cloak(hwnd, false) {
                    ShowWindow(hwnd, SW_HIDE);
                    return;
                }
            }
            SetForegroundWindow(hwnd);
        }
        self.focus(hwnd, 1);
    }

    fn prepare_header_icon(&self, dpi: u32) {
        let size = scale(HEADER_ICON_DIP, dpi);
        if self
            .header_icon
            .get()
            .is_some_and(|(cached, _)| cached == size)
        {
            return;
        }
        // The resource contains a separate SVG export for each menu size.
        let icon = load_app_icon(size, size);
        if !icon.is_null()
            && let Some((_, previous)) = self.header_icon.replace(Some((size, icon)))
        {
            unsafe { DestroyIcon(previous) };
        }
    }

    fn update_icon_bounds(&self, hwnd: HWND) -> Option<RECT> {
        let id = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            ..Default::default()
        };
        let mut rect = RECT::default();
        let bounds = (self.registered.get()
            && unsafe { Shell_NotifyIconGetRect(&id, &mut rect) } >= 0)
            .then_some(rect);
        self.icon_bounds.set(bounds);
        bounds
    }

    fn deactivate(&self, hwnd: HWND) {
        // Explorer can take focus on button-down and send the icon selection
        // on button-up. Keep the flyout visible so that selection closes it.
        let mut cursor = POINT::default();
        let over_icon = self.icon_bounds.get().is_some_and(|rect| unsafe {
            GetCursorPos(&mut cursor) != 0 && PtInRect(&rect, cursor) != 0
        });
        if (self.icon_pressed.get() || over_icon)
            && unsafe { SetTimer(hwnd, DISMISS_AFTER_ICON_PRESS, GetDoubleClickTime(), None) } != 0
        {
            self.dismiss_pending.set(true);
        } else {
            self.hide(hwnd, false);
        }
    }

    fn cancel_deferred_dismissal(&self, hwnd: HWND) {
        if self.dismiss_pending.replace(false) {
            unsafe { KillTimer(hwnd, DISMISS_AFTER_ICON_PRESS) };
        }
    }

    fn apply_theme(&self, hwnd: HWND, colors: Colors) {
        self.colors.set(colors);
        unsafe {
            for (attribute, value) in [
                (DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32),
                (DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(colors.dark)),
                (DWMWA_BORDER_COLOR, colors.border),
            ] {
                DwmSetWindowAttribute(
                    hwnd,
                    attribute as u32,
                    (&raw const value).cast(),
                    size_of::<u32>() as u32,
                );
            }
        }
    }

    fn layout(&self, hwnd: HWND) {
        self.hot.set(null_mut());
        let rows = self.menu_rows();
        let count = rows.len();
        *self.entries.borrow_mut() = rows;
        let point = self.anchor.get();
        let mut monitor = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            GetMonitorInfoW(
                MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            );
        }
        let work = monitor.rcWork;
        let p = |n| scale(n, self.dpi.get());
        // Measure with the same font and DPI used for the owner-drawn rows.
        // The stepper and switch reserve different amounts of space on the right.
        let mut width = p(340);
        if let Some(fonts) = self.fonts.borrow().as_ref() {
            let dc = unsafe { GetDC(hwnd) };
            if !dc.is_null() {
                let measure = |font, value: &str| {
                    paint::text(dc, RECT::default(), value, font, 0, DT_CALCRECT)
                };
                for row in self.entries.borrow().iter().skip(1) {
                    let mut label = measure(fonts.regular, &row.label);
                    let trailing = if let Action::Adjust(setting) = row.action {
                        let range = render(Msg::TrayRange, &[("max", &setting.max().to_string())]);
                        label = label.max(measure(fonts.regular, &range));
                        p(118)
                    } else if row.checked.is_some() {
                        p(52)
                    } else if !row.detail.is_empty() {
                        measure(fonts.small, &row.detail) + p(24)
                    } else {
                        p(12)
                    };
                    width = width.max(p(50) + label + trailing + 2);
                }
                unsafe { ReleaseDC(hwnd, dc) };
            }
        }
        let width = width.min((work.right - work.left).max(1));
        // Keep all choices reachable on a small high-DPI monitor.
        let ideal = self
            .entries
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, r)| p(if i == 0 { 43 } else { 34 }) + if r.separator { p(9) } else { 0 })
            .sum::<i32>();
        let height = (ideal + p(12) + 2).min((work.bottom - work.top).max(1));
        let x = (point.x - width / 2).clamp(work.left, (work.right - width).max(work.left));
        let y = if point.y - height - p(5) >= work.top {
            point.y - height - p(5)
        } else {
            point.y + p(5)
        }
        .clamp(work.top, (work.bottom - height).max(work.top));
        unsafe {
            SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
        }
        let mut client = RECT::default();
        unsafe {
            GetClientRect(hwnd, &mut client);
        }
        let mut y = p(5);
        let available = (client.bottom - 2 * y).max(1).min(ideal);
        for i in 0..count {
            let row = self.entries.borrow()[i].clone();
            let name = row.accessible_name();
            let mut btn = self.buttons.borrow().get(i).copied().unwrap_or(null_mut());
            if btn.is_null() {
                unsafe {
                    btn = CreateWindowExW(
                        0,
                        w!("BUTTON"),
                        name.as_ptr(),
                        WS_CHILD | WS_CLIPSIBLINGS | WS_TABSTOP | BS_OWNERDRAW as u32,
                        0,
                        0,
                        0,
                        0,
                        hwnd,
                        (100 + i) as _,
                        GetModuleHandleW(null()),
                        null(),
                    );
                    if btn.is_null() {
                        return;
                    }
                    SetWindowSubclass(btn, Some(button_proc), 1, 0);
                }
                self.buttons.borrow_mut().push(btn);
            }
            let h = (p(if i == 0 { 43 } else { 34 }) + if row.separator { p(9) } else { 0 })
                * available
                / ideal;
            unsafe {
                SetWindowTextW(btn, name.as_ptr());
                EnableWindow(btn, i32::from(row.enabled));
                SetWindowPos(
                    btn,
                    null_mut(),
                    p(5),
                    y,
                    client.right - 2 * p(5),
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                InvalidateRect(btn, null(), 0);
            }
            if let Action::Adjust(measure) = row.action {
                let rect = RECT {
                    left: p(5),
                    top: y,
                    right: client.right - p(5),
                    bottom: y + h,
                };
                self.layout_arrows(hwnd, i, measure, rect, row.separator);
            }
            y += h;
        }
        let extra = self.buttons.borrow()[count..].to_vec();
        for btn in extra {
            unsafe {
                ShowWindow(btn, SW_HIDE);
            }
        }
        unsafe {
            InvalidateRect(hwnd, null(), 1);
        }
    }

    fn hide(&self, hwnd: HWND, return_to_tray: bool) {
        self.icon_pressed.set(false);
        self.finish_edit(hwnd, EndEdit::Blur);
        self.inline.wheel.set(None);
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
            if return_to_tray && self.registered.get() {
                Shell_NotifyIconW(NIM_SETFOCUS, &self.icon_data(hwnd));
            }
        }
        self.cancel_deferred_dismissal(hwnd);
        self.hot.set(null_mut());
    }

    fn focus(&self, hwnd: HWND, index: usize) {
        let btn = unsafe { GetDlgItem(hwnd, 100 + index as i32) };
        if !btn.is_null() {
            unsafe {
                SetFocus(btn);
            }
        }
    }

    fn activate(&self, hwnd: HWND, index: usize) {
        let action = self
            .entries
            .borrow()
            .get(index)
            .filter(|r| r.enabled)
            .map(|r| r.action);
        match action {
            Some(Action::Adjust(_)) => self.start_edit(hwnd, index),
            Some(action @ (Action::EditShortcuts | Action::EditExclusions)) => {
                self.hide(hwnd, false);
                let settings = self.settings.borrow().clone();
                let opened = if matches!(action, Action::EditExclusions) {
                    self.exclusions.open(hwnd, &settings.exclusions)
                } else {
                    self.shortcuts.show(hwnd, self.anchor.get(), settings)
                };
                if let Err(detail) = opened {
                    let text: Vec<_> = detail.encode_utf16().chain([0]).collect();
                    unsafe {
                        MessageBoxW(hwnd, text.as_ptr(), w!("Rustile"), MB_OK | MB_ICONWARNING)
                    };
                }
            }
            Some(Action::Apply(command)) => {
                if matches!(command, Command::SetStartup(_)) && self.startup_pending.get() {
                    return;
                }
                self.commands.borrow_mut().push_back(command);
                if !matches!(command, Command::SetStartup(_)) {
                    self.hide(hwnd, true);
                }
                unsafe {
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                }
            }
            _ => {}
        }
    }

    fn key(&self, hwnd: HWND, index: usize, key: u16) -> bool {
        self.keyboard.set(true);
        if matches!(key, VK_LEFT | VK_RIGHT)
            && let Some(m) = self.measure(index)
        {
            self.step(hwnd, m, if key == VK_RIGHT { 1 } else { -1 });
            return true;
        }
        match key {
            VK_ESCAPE | VK_LEFT | VK_BACK => self.hide(hwnd, true),
            VK_RIGHT => {
                let opens = self.entries.borrow().get(index).is_some_and(|r| {
                    matches!(r.action, Action::EditShortcuts | Action::EditExclusions)
                });
                if opens {
                    self.activate(hwnd, index);
                }
            }
            VK_RETURN | VK_SPACE => self.activate(hwnd, index),
            VK_UP | VK_DOWN | VK_TAB | VK_HOME | VK_END => {
                let selectable: Vec<_> = self
                    .entries
                    .borrow()
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.enabled)
                    .map(|(i, _)| i)
                    .collect();
                if selectable.is_empty() {
                    return true;
                }
                let at = selectable.iter().position(|&i| i == index).unwrap_or(0);
                let backward =
                    key == VK_UP || (key == VK_TAB && unsafe { GetKeyState(VK_SHIFT as i32) } < 0);
                let n = selectable.len();
                let next = match key {
                    VK_HOME => 0,
                    VK_END => n - 1,
                    _ if backward => (at + n - 1) % n,
                    _ => (at + 1) % n,
                };
                self.focus(hwnd, selectable[next]);
            }
            _ => return false,
        }
        true
    }
}

fn set_popup_cloak(hwnd: HWND, cloak: bool) -> bool {
    let value = i32::from(cloak);
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK as u32,
            (&raw const value).cast(),
            size_of_val(&value) as u32,
        ) >= 0
    }
}

unsafe extern "system" fn button_proc(
    btn: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        let parent = GetParent(btn);
        let ptr = GetWindowLongPtrW(parent, GWLP_USERDATA) as *const State;
        if let Some(state) = ptr.as_ref() {
            let id = GetDlgCtrlID(btn) as usize;
            let arrow = arrow_info(id);
            let Some(index) = arrow
                .map(|(index, _)| index)
                .or_else(|| id.checked_sub(100))
            else {
                return DefSubclassProc(btn, msg, wp, lp);
            };
            match msg {
                WM_ERASEBKGND => {
                    let mut rect = RECT::default();
                    GetClientRect(btn, &mut rect);
                    paint::background(wp as HDC, &rect, state.colors.get());
                    return 1;
                }
                WM_MOUSEWHEEL => {
                    state.wheel(parent, wp, lp);
                    return 0;
                }
                WM_KEYDOWN if arrow.is_some() && matches!(wp as u16, VK_SPACE | VK_RETURN) => {
                    if lp & (1 << 30) == 0 {
                        let (_, increasing) = arrow.unwrap();
                        state.arrow_click(parent, index, increasing);
                    }
                    return 0;
                }
                WM_KEYDOWN if matches!(wp as u16, VK_SPACE | VK_RETURN) && lp & (1 << 30) != 0 => {
                    return 0;
                }
                WM_KEYUP | WM_CHAR if matches!(wp as u16, VK_SPACE | VK_RETURN) => {
                    return 0;
                }
                WM_KEYDOWN if state.key(parent, index, wp as u16) => {
                    return 0;
                }
                WM_CHAR
                    if (b'0' as usize..=b'9' as usize).contains(&wp)
                        && state.measure(index).is_some() =>
                {
                    state.start_edit(parent, index);
                    if let Some(edit) = state.editor(parent) {
                        SendMessageW(edit, WM_CHAR, wp, lp);
                    }
                    return 0;
                }
                WM_MOUSEMOVE if state.hot.get() != btn => {
                    let previous = state.hot.replace(btn);
                    state.keyboard.set(false);
                    if !previous.is_null() {
                        InvalidateRect(previous, null(), 0);
                    }
                    let mut tracking = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: btn,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tracking);
                    InvalidateRect(btn, null(), 0);
                }
                WM_MOUSELEAVE if state.hot.get() == btn => {
                    state.hot.set(null_mut());
                    InvalidateRect(btn, null(), 0);
                }
                WM_NCDESTROY => {
                    RemoveWindowSubclass(btn, Some(button_proc), 1);
                }
                _ => {}
            }
        }
        DefSubclassProc(btn, msg, wp, lp)
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lp as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
        let Some(state) = ptr.as_ref() else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        // This top-level window receives broadcasts even when its flyout is
        // closed. A sent message alone does not return control to GetMessage.
        if (matches!(msg, WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_DPICHANGED)
            || (msg != 0 && msg == state.taskbar_created))
            && !state.work_area_changed.replace(true)
        {
            PostMessageW(hwnd, WM_NULL, 0, 0);
        }
        match msg {
            CALLBACK if (lp as u32 >> 16) == ICON_ID => {
                let event = (lp as u32) & 0xffff;
                if matches!(
                    event,
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK
                ) {
                    state.icon_pressed.set(IsWindowVisible(hwnd) != 0);
                } else if matches!(event, WM_LBUTTONUP | WM_RBUTTONUP) {
                    state.icon_pressed.set(false);
                } else if matches!(event, NIN_SELECT | NIN_KEYSELECT | WM_CONTEXTMENU) {
                    if IsWindowVisible(hwnd) != 0 {
                        state.hide(hwnd, true);
                        return 0;
                    }
                    let bounds = state.update_icon_bounds(hwnd);
                    let mut point = POINT {
                        x: wp as i16 as i32,
                        y: (wp >> 16) as i16 as i32,
                    };
                    let keyboard = event == NIN_KEYSELECT || (point.x == -1 && point.y == -1);
                    if keyboard {
                        if let Some(rect) = bounds {
                            point = POINT {
                                x: (rect.left + rect.right) / 2,
                                y: rect.top,
                            };
                        } else {
                            GetCursorPos(&mut point);
                        }
                    }
                    state.show(hwnd, point, keyboard);
                }
                0
            }
            m if m != 0 && m == state.taskbar_created => {
                state.registered.set(false);
                state.attempts.set(0);
                state.register_icon(hwnd);
                0
            }
            WM_TIMER if wp == RETRY => {
                state.register_icon(hwnd);
                0
            }
            WM_TIMER if wp == DISMISS_AFTER_ICON_PRESS => {
                // A drag away or a cancelled click can end without a selection.
                if state.dismiss_pending.get()
                    && GetAsyncKeyState(VK_LBUTTON as i32) >= 0
                    && GetAsyncKeyState(VK_RBUTTON as i32) >= 0
                {
                    state.hide(hwnd, false);
                }
                0
            }
            WM_ACTIVATE if wp & 0xffff == WA_INACTIVE as usize && IsWindowVisible(hwnd) != 0 => {
                state.deactivate(hwnd);
                0
            }
            WM_ACTIVATE if wp & 0xffff != WA_INACTIVE as usize => {
                state.cancel_deferred_dismissal(hwnd);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_MOUSEWHEEL => {
                state.wheel(hwnd, wp, lp);
                0
            }
            WM_CTLCOLOREDIT if GetDlgCtrlID(lp as HWND) == EDIT_ID as i32 => {
                let (text, background) = state.colors.get().edit_colors();
                let dc = wp as HDC;
                SetTextColor(dc, text);
                SetBkColor(dc, background);
                SetDCBrushColor(dc, background);
                GetStockObject(DC_BRUSH) as LRESULT
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED if IsWindowVisible(hwnd) != 0 => {
                let updated = Colors::current();
                if updated != state.colors.get() {
                    state.apply_theme(hwnd, updated);
                    RedrawWindow(
                        hwnd,
                        null(),
                        null_mut(),
                        RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                    );
                }
                0
            }
            WM_COMMAND if lp != 0 && wp & 0xffff == EDIT_ID && wp >> 16 == EN_CHANGE as usize => {
                if state.inline.invalid.replace(false)
                    && let Some(index) = state.inline.editing.get()
                {
                    InvalidateRect(GetDlgItem(hwnd, 100 + index as i32), null(), 0);
                }
                0
            }
            WM_COMMAND
                if lp != 0 && wp >> 16 == BN_CLICKED as usize && IsWindowVisible(hwnd) != 0 =>
            {
                if let Some((index, increasing)) = arrow_info(wp & 0xffff) {
                    state.arrow_click(hwnd, index, increasing);
                } else if let Some(i) = (wp & 0xffff).checked_sub(100) {
                    state.activate(hwnd, i);
                }
                0
            }
            WM_DRAWITEM if lp != 0 => {
                let draw = &*(lp as *const DRAWITEMSTRUCT);
                if let Some(fonts) = state.fonts.borrow().as_ref() {
                    let painter = Painter {
                        dpi: state.dpi.get(),
                        colors: state.colors.get(),
                        fonts,
                        header_icon: state
                            .header_icon
                            .get()
                            .map_or(state.tray_icon, |(_, icon)| icon),
                        keyboard: state.keyboard.get(),
                    };
                    let hover = state.hot.get() == draw.hwndItem;
                    let focus = GetFocus() == draw.hwndItem;
                    if let Some((_, increasing)) = arrow_info(draw.CtlID as usize) {
                        painter.arrow(draw, increasing, hover, focus);
                    } else if let Some(i) = draw.CtlID.checked_sub(100)
                        && let Some(mut row) = state.entries.borrow().get(i as usize).cloned()
                    {
                        if state.inline.invalid.get()
                            && state.inline.editing.get() == Some(i as usize)
                            && let Action::Adjust(measure) = row.action
                        {
                            row.label =
                                render(Msg::TrayRange, &[("max", &measure.max().to_string())]);
                        }
                        painter.row(draw, &row, i == 0, hover, focus);
                    }
                }
                1
            }
            WM_ERASEBKGND => {
                let mut rc = RECT::default();
                GetClientRect(hwnd, &mut rc);
                paint::background(wp as HDC, &rc, state.colors.get());
                1
            }
            WM_CLOSE => {
                state.hide(hwnd, true);
                0
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
