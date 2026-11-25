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
use crate::settings::{Chord, Command, Settings, Shortcut};

const CLASS: *const u16 = w!("Rustile.Shortcuts");
const TITLE: *const u16 = w!("Горячие клавиши — Rustile");
const STYLE: u32 = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
const MODS: [(u32, &str); 4] = [
    (MOD_WIN, "Win"),
    (MOD_CONTROL, "Ctrl"),
    (MOD_ALT, "Alt"),
    (MOD_SHIFT, "Shift"),
];
const MOD_BASE: i32 = 500;
const KEY: i32 = 504;
const APPLY: i32 = 505;
const STATUS: i32 = 506;
const EDIT_TITLE: i32 = 507;
const KEY_LABEL: i32 = 508;
const VIEW: i32 = 510;
const SECTION: i32 = 520;
const ROW: i32 = 1000;

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
    ROW + Shortcut::ALL
        .iter()
        .position(|&s| s == action)
        .expect("listed shortcut") as i32
}

fn row_action(id: i32) -> Option<Shortcut> {
    Shortcut::ALL.get(id.checked_sub(ROW)? as usize).copied()
}

pub(super) struct ShortcutWindow {
    hwnd: Cell<HWND>,
    owner: Cell<HWND>,
    dpi: Cell<u32>,
    settings: Cell<Settings>,
    selected: Cell<Shortcut>,
    command: Cell<Option<Command>>,
    saving: Cell<bool>,
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
            command: Cell::default(),
            saving: Cell::new(false),
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
                    right: self.p(850),
                    bottom: self.p(570),
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
                self.layout(hwnd);
                self.update_rows();
                self.select(Shortcut::MoveLeft);
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
            SetFocus(self.row(self.selected.get()));
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
            self.update_apply();
            caption(child(hwnd, STATUS), error.unwrap_or("Сочетание сохранено"));
        }
    }

    pub(super) fn pre_translate(&self, msg: &MSG) -> bool {
        let hwnd = self.hwnd.get();
        if hwnd.is_null() || (msg.hwnd != hwnd && unsafe { IsChild(hwnd, msg.hwnd) } == 0) {
            return false;
        }
        // Let a dropped-down native combo handle Escape and Enter itself.
        let combo = child(hwnd, KEY);
        unsafe {
            if msg.hwnd == combo && SendMessageW(combo, CB_GETDROPPEDSTATE, 0, 0) != 0 {
                return false;
            }
            let id = GetDlgCtrlID(msg.hwnd);
            if msg.message == WM_KEYDOWN
                && msg.wParam == VK_RETURN as usize
                && (row_action(id).is_some() || id == APPLY)
            {
                if msg.lParam & (1 << 30) == 0 {
                    SendMessageW(msg.hwnd, BM_CLICK, 0, 0);
                }
                return true;
            }
            IsDialogMessageW(hwnd, msg) != 0
        }
    }

    fn p(&self, dip: i32) -> i32 {
        scale(dip, self.dpi.get())
    }
    fn row(&self, action: Shortcut) -> HWND {
        child(child(self.hwnd.get(), VIEW), row_id(action))
    }

    fn update_rows(&self) {
        for action in Shortcut::ALL {
            let row = self.row(action);
            caption(
                row,
                &format!(
                    "{}, {}",
                    action.label(),
                    self.settings.get().chord(action).label()
                ),
            );
            unsafe { InvalidateRect(row, null(), 0) };
        }
    }

    fn select(&self, action: Shortcut) {
        let hwnd = self.hwnd.get();
        let previous = self.selected.replace(action);
        let chord = self.settings.get().chord(action);
        caption(child(hwnd, EDIT_TITLE), action.label());
        caption(child(hwnd, STATUS), "");
        unsafe {
            for (i, (flag, _)) in MODS.iter().enumerate() {
                SendMessageW(
                    child(hwnd, MOD_BASE + i as i32),
                    BM_SETCHECK,
                    usize::from(chord.mods & flag != 0),
                    0,
                );
            }
            if let Some(i) = Chord::keys().iter().position(|&k| k == chord.key) {
                SendMessageW(child(hwnd, KEY), CB_SETCURSEL, i, 0);
            }
            InvalidateRect(self.row(previous), null(), 0);
            InvalidateRect(self.row(action), null(), 0);
        }
        self.update_apply();
    }

    fn draft(&self) -> Option<Chord> {
        let hwnd = self.hwnd.get();
        let mut mods = 0;
        for (i, (flag, _)) in MODS.iter().enumerate() {
            if unsafe { SendMessageW(child(hwnd, MOD_BASE + i as i32), BM_GETCHECK, 0, 0) }
                == BST_CHECKED as isize
            {
                mods |= flag;
            }
        }
        let index = unsafe { SendMessageW(child(hwnd, KEY), CB_GETCURSEL, 0, 0) };
        Some(Chord {
            mods,
            key: *Chord::keys().get(index as usize)?,
        })
    }

    fn update_apply(&self) {
        let changed = self
            .draft()
            .is_some_and(|c| c != self.settings.get().chord(self.selected.get()));
        unsafe {
            EnableWindow(
                child(self.hwnd.get(), APPLY),
                i32::from(changed && !self.saving.get()),
            )
        };
    }

    fn apply(&self) {
        let Some(chord) = self.draft() else { return };
        let action = self.selected.get();
        if self.saving.get() || chord == self.settings.get().chord(action) {
            return;
        }
        let command = Command::Shortcut(action, chord);
        if let Err(e) = self.settings.get().changed(command) {
            self.result(Some(&e));
            return;
        }
        self.command.set(Some(command));
        self.saving.set(true);
        self.update_apply();
        unsafe { PostMessageW(self.owner.get(), WM_NULL, 0, 0) };
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
            WM_COMMAND => {
                let id = (wp & 0xffff) as i32;
                let event = (wp >> 16) as u32;
                if let Some(action) = row_action(id) {
                    if event == BN_CLICKED {
                        ui.select(action);
                        SetFocus(child(hwnd, MOD_BASE));
                    } else if event == BN_SETFOCUS {
                        ui.reveal_row(hwnd, lp as HWND);
                    }
                } else if id == APPLY && event == BN_CLICKED {
                    ui.apply();
                } else if ((MOD_BASE..KEY).contains(&id) && event == BN_CLICKED)
                    || (id == KEY && event == CBN_SELCHANGE)
                {
                    caption(child(hwnd, STATUS), "");
                    ui.update_apply();
                } else if id == IDCANCEL && lp == 0 {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_MEASUREITEM if wp == KEY as usize && lp != 0 => {
                (*(lp as *mut MEASUREITEMSTRUCT)).itemHeight = ui.p(26) as u32;
                1
            }
            WM_DRAWITEM if lp != 0 => {
                ui.draw(&*(lp as *const DRAWITEMSTRUCT));
                1
            }
            WM_ERASEBKGND => {
                ui.background(hwnd, wp as HDC);
                1
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => {
                let colors = ui.colors.get();
                let bg = if msg == WM_CTLCOLORLISTBOX {
                    colors.control
                } else {
                    colors.background
                };
                SetBkColor(wp as HDC, bg);
                SetTextColor(wp as HDC, colors.text);
                SetDCBrushColor(wp as HDC, bg);
                GetStockObject(DC_BRUSH) as isize
            }
            WM_MOUSEWHEEL => {
                ui.wheel(hwnd, (wp >> 16) as i16 as i32);
                0
            }
            WM_NCDESTROY => {
                ui.hwnd.set(null_mut());
                ui.scroll.set(0);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
