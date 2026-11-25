mod menu;
mod paint;

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

use crate::settings::{Command, Settings};
use menu::{Action, Page, Row};
use paint::{Colors, Fonts, Painter};

const CALLBACK: u32 = WM_APP + 20;
const ICON_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const RETRY: usize = 1;
const CLASS: *const u16 = w!("Rustile.Tray");

fn scale(dip: i32, dpi: u32) -> i32 {
    (dip * dpi as i32 + 48) / 96
}

struct State {
    settings: Cell<Settings>,
    commands: RefCell<VecDeque<Command>>,
    icon: HICON,
    taskbar_created: u32,
    registered: Cell<bool>,
    attempts: Cell<u8>,
    page: Cell<Page>,
    anchor: Cell<POINT>,
    dpi: Cell<u32>,
    colors: Cell<Colors>,
    fonts: RefCell<Option<Fonts>>,
    entries: RefCell<Vec<Row>>,
    buttons: RefCell<Vec<HWND>>,
    hot: Cell<HWND>,
    keyboard: Cell<bool>,
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
                return Err(format!(
                    "Регистрация меню Rustile: {}",
                    io::Error::last_os_error()
                ));
            }
            let dpi = GetDpiForSystem().max(96);
            let icon = LoadImageW(
                instance,
                101usize as _,
                IMAGE_ICON,
                GetSystemMetricsForDpi(SM_CXSMICON, dpi),
                GetSystemMetricsForDpi(SM_CYSMICON, dpi),
                0,
            );
            if icon.is_null() {
                return Err(format!("Значок Rustile: {}", io::Error::last_os_error()));
            }
            let state = Box::new(State {
                settings: Cell::new(settings),
                commands: RefCell::default(),
                icon,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                registered: Cell::new(false),
                attempts: Cell::new(0),
                page: Cell::new(Page::Main),
                anchor: Cell::default(),
                dpi: Cell::new(dpi),
                colors: Cell::new(Colors::current()),
                fonts: RefCell::default(),
                entries: RefCell::default(),
                buttons: RefCell::default(),
                hot: Cell::default(),
                keyboard: Cell::new(false),
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
                return Err(format!(
                    "Создание меню Rustile: {}",
                    io::Error::last_os_error()
                ));
            }
            state.register_icon(hwnd);
            Ok(Self { hwnd, state })
        }
    }

    pub fn set_settings(&self, settings: Settings) {
        self.state.settings.set(settings);
        if self.state.registered.get() {
            let mut data = self.state.icon_data(self.hwnd);
            data.uFlags = NIF_TIP | NIF_SHOWTIP;
            if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } == 0 {
                self.state.registered.set(false);
                self.state.register_icon(self.hwnd);
            }
        }
        if unsafe { IsWindowVisible(self.hwnd) } != 0 {
            self.state.layout(self.hwnd, self.state.page.get());
        }
    }

    pub fn take_command(&self) -> Option<Command> {
        self.state.commands.borrow_mut().pop_front()
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

impl Drop for State {
    fn drop(&mut self) {
        unsafe { DestroyIcon(self.icon) };
    }
}

impl State {
    fn icon_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_TIP | NIF_SHOWTIP | NIF_MESSAGE,
            hIcon: self.icon,
            uCallbackMessage: CALLBACK,
            ..Default::default()
        };
        let tip = if self.settings.get().enabled {
            "Rustile — мозаика включена"
        } else {
            "Rustile — мозаика выключена"
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
                eprintln!(
                    "Проводник Windows пока не принял значок Rustile. Установка будет повторена."
                );
            }
        }
    }

    fn show(&self, hwnd: HWND, point: POINT, keyboard: bool) {
        self.anchor.set(point);
        self.keyboard.set(keyboard);
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
        let (mut x, mut y) = (96, 96);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
        self.dpi.set(x.max(96));
        *self.fonts.borrow_mut() = Some(Fonts::new(x.max(96)));
        self.apply_theme(hwnd, Colors::current());
        self.layout(hwnd, Page::Main);
        unsafe {
            // A launcher may pass STARTF_USESHOWWINDOW with SW_HIDE. The first
            // ShowWindow would then ignore its requested state. Reveal the
            // popup explicitly after its rows have been positioned.
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
            SetForegroundWindow(hwnd);
        }
        self.focus(hwnd, 1);
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

    fn layout(&self, hwnd: HWND, page: Page) {
        self.page.set(page);
        self.hot.set(null_mut());
        let rows = menu::rows(page, self.settings.get());
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
        let width = p(325).min((work.right - work.left).max(1));
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
                        WS_CHILD | WS_TABSTOP | BS_OWNERDRAW as u32,
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
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
            if return_to_tray && self.registered.get() {
                Shell_NotifyIconW(NIM_SETFOCUS, &self.icon_data(hwnd));
            }
        }
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
            Some(Action::Open(page)) => {
                self.layout(hwnd, page);
                let selected = self
                    .entries
                    .borrow()
                    .iter()
                    .position(|row| row.checked)
                    .unwrap_or(1);
                self.focus(hwnd, selected);
            }
            Some(Action::Back) => self.back(hwnd),
            Some(Action::Apply(command)) => {
                self.commands.borrow_mut().push_back(command);
                self.hide(hwnd, true);
                unsafe {
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                }
            }
            _ => {}
        }
    }

    fn back(&self, hwnd: HWND) {
        let old = self.page.get();
        if let Some(parent) = old.parent() {
            self.layout(hwnd, parent);
            let i = self
                .entries
                .borrow()
                .iter()
                .position(|row| matches!(row.action, Action::Open(p) if p == old))
                .unwrap_or(1);
            self.focus(hwnd, i);
        } else {
            self.hide(hwnd, true);
        }
    }

    fn key(&self, hwnd: HWND, index: usize, key: u16) -> bool {
        self.keyboard.set(true);
        match key {
            VK_ESCAPE => self.hide(hwnd, true),
            VK_LEFT | VK_BACK => self.back(hwnd),
            VK_RIGHT => {
                let opens = self
                    .entries
                    .borrow()
                    .get(index)
                    .is_some_and(|r| matches!(r.action, Action::Open(_)));
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
            match msg {
                WM_KEYDOWN if matches!(wp as u16, VK_SPACE | VK_RETURN) && lp & (1 << 30) != 0 => {
                    return 0;
                }
                WM_KEYUP | WM_CHAR if matches!(wp as u16, VK_SPACE | VK_RETURN) => {
                    return 0;
                }
                WM_KEYDOWN if state.key(parent, (GetDlgCtrlID(btn) - 100) as usize, wp as u16) => {
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
        match msg {
            CALLBACK if (lp as u32 >> 16) == ICON_ID => {
                let event = (lp as u32) & 0xffff;
                if matches!(event, NIN_SELECT | NIN_KEYSELECT | WM_CONTEXTMENU) {
                    let mut point = POINT {
                        x: wp as i16 as i32,
                        y: (wp >> 16) as i16 as i32,
                    };
                    let keyboard = event == NIN_KEYSELECT || (point.x == -1 && point.y == -1);
                    if keyboard {
                        let id = NOTIFYICONIDENTIFIER {
                            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
                            hWnd: hwnd,
                            uID: ICON_ID,
                            ..Default::default()
                        };
                        let mut rect = RECT::default();
                        if Shell_NotifyIconGetRect(&id, &mut rect) >= 0 {
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
            WM_ACTIVATE if wp & 0xffff == WA_INACTIVE as usize && IsWindowVisible(hwnd) != 0 => {
                state.hide(hwnd, false);
                0
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
            WM_COMMAND
                if lp != 0 && wp >> 16 == BN_CLICKED as usize && IsWindowVisible(hwnd) != 0 =>
            {
                if let Some(i) = (wp & 0xffff).checked_sub(100) {
                    state.activate(hwnd, i);
                }
                0
            }
            WM_DRAWITEM if lp != 0 => {
                let draw = &*(lp as *const DRAWITEMSTRUCT);
                let Some(i) = draw.CtlID.checked_sub(100) else {
                    return 0;
                };
                let row = state.entries.borrow().get(i as usize).cloned();
                if let (Some(row), Some(fonts)) = (row, state.fonts.borrow().as_ref()) {
                    Painter {
                        dpi: state.dpi.get(),
                        colors: state.colors.get(),
                        fonts,
                        icon: state.icon,
                        keyboard: state.keyboard.get(),
                        enabled: state.settings.get().enabled,
                    }
                    .row(
                        draw,
                        &row,
                        i == 0,
                        state.hot.get() == draw.hwndItem,
                        GetFocus() == draw.hwndItem,
                    );
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
