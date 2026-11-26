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
        System::{LibraryLoader::GetModuleHandleW, SystemServices::SS_NOPREFIX},
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::w,
};

use super::{Colors, Fonts, paint, scale};
use crate::i18n::{Msg, tr, wide as wide_message};
use crate::settings::exclusions::{ExclusionRule, MatchMode};

const CLASS: *const u16 = w!("Rustile.Exclusions");
const STYLE: u32 = WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
const LIST: i32 = 601;
const NEW: i32 = 602;
const REMOVE: i32 = 603;
const PROCESS: i32 = 604;
const TITLE_FIELD: i32 = 605;
const CLASS_FIELD: i32 = 606;
const MODE: i32 = 607;
const SAVE: i32 = 608;
const STATUS: i32 = 609;
const PICK: i32 = 610;
const LIST_LABEL: i32 = 611;
const FORM_LABEL: i32 = 612;
const HINT: i32 = 613;
const PROCESS_LABEL: i32 = 614;
const TITLE_LABEL: i32 = 615;
const CLASS_LABEL: i32 = 616;
const MODE_LABEL: i32 = 617;
const MODES: [(MatchMode, Msg); 3] = [
    (MatchMode::Equals, Msg::RuleExact),
    (MatchMode::Contains, Msg::RuleContains),
    (MatchMode::Regex, Msg::RuleRegex),
];
const FIELDS: [i32; 3] = [PROCESS, TITLE_FIELD, CLASS_FIELD];

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn set_text(hwnd: HWND, value: &str) {
    unsafe { SetWindowTextW(hwnd, wide(value).as_ptr()) };
}

fn text_of(hwnd: HWND) -> String {
    unsafe {
        let mut buffer = vec![0; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let len = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        String::from_utf16_lossy(&buffer[..len.max(0) as usize])
    }
}

struct Change {
    rules: Vec<ExclusionRule>,
    selected: Option<usize>,
}

#[derive(Default)]
pub(super) struct ExclusionWindow(Box<Editor>);

struct Editor {
    hwnd: Cell<HWND>,
    owner: Cell<HWND>,
    dpi: Cell<u32>,
    colors: Cell<Colors>,
    fonts: RefCell<Option<Fonts>>,
    rules: RefCell<Vec<ExclusionRule>>,
    selected: Cell<Option<usize>>,
    loading: Cell<bool>,
    picked_cues: Cell<bool>,
    pending: RefCell<Option<Change>>,
    queued: Cell<bool>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            hwnd: Cell::default(),
            owner: Cell::default(),
            dpi: Cell::new(96),
            colors: Cell::new(Colors::current()),
            fonts: RefCell::default(),
            rules: RefCell::default(),
            selected: Cell::new(None),
            loading: Cell::new(false),
            picked_cues: Cell::new(false),
            pending: RefCell::default(),
            queued: Cell::new(false),
        }
    }
}

impl ExclusionWindow {
    pub(super) fn language_changed(&self) {
        let ui = &self.0;
        let hwnd = ui.hwnd.get();
        if hwnd.is_null() {
            return;
        }
        set_text(hwnd, tr(Msg::RuleWindowTitle));
        ui.localize_controls();
        if !ui.picked_cues.get() {
            ui.example_cues();
        }
        ui.status("");
        ui.layout();
        unsafe {
            RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    pub(super) fn open(&self, owner: HWND, rules: &[ExclusionRule]) -> Result<(), String> {
        let ui = &*self.0;
        if ui.hwnd.get().is_null() {
            ui.owner.set(owner);
            ui.rules.replace(rules.to_vec());
            ui.create()?;
            ui.populate();
            ui.load_row((!rules.is_empty()).then_some(0));
        }
        unsafe {
            let hwnd = ui.hwnd.get();
            // The tray process can be launched with SW_HIDE. A first
            // ShowWindow call would inherit that startup preference.
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
            if IsChild(hwnd, GetFocus()) == 0 {
                SetFocus(ui.child(if ui.selected.get().is_some() {
                    LIST
                } else {
                    PROCESS
                }));
            }
        }
        Ok(())
    }

    pub(super) fn take_rules(&self) -> Option<Vec<ExclusionRule>> {
        if !self.0.queued.take() {
            return None;
        }
        self.0
            .pending
            .borrow()
            .as_ref()
            .map(|change| change.rules.clone())
    }

    pub(super) fn finish_save(&self, result: Result<(), String>) {
        let ui = &*self.0;
        let Some(change) = ui.pending.take() else {
            return;
        };
        if ui.hwnd.get().is_null() {
            return;
        }
        match result {
            Ok(()) => {
                ui.rules.replace(change.rules);
                ui.populate();
                ui.load_row(change.selected);
            }
            Err(error) => {
                ui.status(&error);
                ui.update_buttons();
            }
        }
        unsafe {
            let hwnd = ui.hwnd.get();
            if GetForegroundWindow() == hwnd && IsChild(hwnd, GetFocus()) == 0 {
                SetFocus(ui.child(if ui.selected.get().is_some() {
                    LIST
                } else {
                    PROCESS
                }));
            }
        }
    }

    pub(super) fn pre_translate(&self, msg: &MSG) -> bool {
        let ui = &*self.0;
        let hwnd = ui.hwnd.get();
        unsafe {
            if hwnd.is_null() || (msg.hwnd != hwnd && IsChild(hwnd, msg.hwnd) == 0) {
                return false;
            }
            if msg.message == WM_KEYDOWN
                && SendMessageW(ui.child(MODE), CB_GETDROPPEDSTATE, 0, 0) == 0
            {
                match msg.wParam as u16 {
                    VK_ESCAPE => {
                        SendMessageW(hwnd, WM_CLOSE, 0, 0);
                        return true;
                    }
                    VK_RETURN => {
                        let id = GetDlgCtrlID(msg.hwnd);
                        if matches!(id, NEW | REMOVE | PICK | SAVE) {
                            SendMessageW(msg.hwnd, BM_CLICK, 0, 0);
                        } else if IsWindowEnabled(ui.child(SAVE)) != 0 {
                            ui.save();
                        }
                        return true;
                    }
                    _ => {}
                }
            }
            IsDialogMessageW(hwnd, msg) != 0
        }
    }
}

impl Drop for ExclusionWindow {
    fn drop(&mut self) {
        if !self.0.hwnd.get().is_null() {
            unsafe { DestroyWindow(self.0.hwnd.get()) };
        }
    }
}

impl Editor {
    fn child(&self, id: i32) -> HWND {
        unsafe { GetDlgItem(self.hwnd.get(), id) }
    }

    fn create(&self) -> Result<(), String> {
        unsafe {
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
            let mut cursor = POINT::default();
            GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
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
            let area = info.rcWork;
            let mut r = RECT {
                right: self.p(686),
                bottom: self.p(379),
                ..Default::default()
            };
            AdjustWindowRectExForDpi(&mut r, STYLE, 0, WS_EX_CONTROLPARENT, self.dpi.get());
            let width = (r.right - r.left).min(area.right - area.left);
            let height = (r.bottom - r.top).min(area.bottom - area.top);
            let hwnd = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                CLASS,
                wide_message(Msg::RuleWindowTitle),
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
                return Err(io::Error::last_os_error().to_string());
            }
            if let Err(error) = self.create_controls() {
                DestroyWindow(hwnd);
                return Err(error);
            }
            self.refresh_fonts();
            self.theme();
            self.layout();
        }
        Ok(())
    }

    fn form(&self) -> ExclusionRule {
        let index = unsafe { SendMessageW(self.child(MODE), CB_GETCURSEL, 0, 0) } as usize;
        ExclusionRule {
            mode: MODES.get(index).unwrap_or(&MODES[0]).0,
            process: text_of(self.child(PROCESS)),
            title: text_of(self.child(TITLE_FIELD)),
            class: text_of(self.child(CLASS_FIELD)),
        }
    }

    fn dirty(&self) -> bool {
        let form = self.form();
        let rules = self.rules.borrow();
        match self.selected.get().and_then(|index| rules.get(index)) {
            Some(rule) => form != *rule,
            None => !form.process.is_empty() || !form.title.is_empty() || !form.class.is_empty(),
        }
    }

    fn may_discard(&self) -> bool {
        !self.dirty()
            || unsafe {
                MessageBoxW(
                    self.hwnd.get(),
                    wide_message(Msg::RuleDiscard),
                    wide_message(Msg::RuleWindowTitle),
                    MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
                ) == IDYES
            }
    }

    fn load_row(&self, row: Option<usize>) {
        self.loading.set(true);
        let rule = row
            .and_then(|i| self.rules.borrow().get(i).cloned())
            .unwrap_or_default();
        self.selected.set(row);
        unsafe {
            SendMessageW(self.child(LIST), LB_SETCURSEL, row.unwrap_or(usize::MAX), 0);
            let mode = MODES
                .iter()
                .position(|(mode, _)| *mode == rule.mode)
                .unwrap_or(0);
            SendMessageW(self.child(MODE), CB_SETCURSEL, mode, 0);
        }
        for (id, value) in FIELDS
            .into_iter()
            .zip([&rule.process, &rule.title, &rule.class])
        {
            set_text(self.child(id), value);
        }
        self.example_cues();
        self.loading.set(false);
        self.status("");
        self.update_buttons();
    }

    fn navigate(&self, row: Option<usize>) {
        if row != self.selected.get() || row.is_none() {
            if self.may_discard() {
                self.load_row(row);
            } else {
                unsafe {
                    SendMessageW(
                        self.child(LIST),
                        LB_SETCURSEL,
                        self.selected.get().unwrap_or(usize::MAX),
                        0,
                    )
                };
            }
        }
    }

    fn status(&self, message: &str) {
        set_text(self.child(STATUS), message);
    }

    fn update_buttons(&self) {
        let busy = self.pending.borrow().is_some();
        unsafe {
            EnableWindow(self.child(SAVE), (!busy && self.dirty()).into());
            EnableWindow(
                self.child(REMOVE),
                (!busy && self.selected.get().is_some()).into(),
            );
            // A save is dispatched asynchronously through the tray. Keep its
            // acknowledgement from replacing a newer selection or typed edit.
            for id in [LIST, NEW, PICK, PROCESS, TITLE_FIELD, CLASS_FIELD, MODE] {
                EnableWindow(self.child(id), (!busy).into());
            }
        }
    }

    fn queue(&self, rules: Vec<ExclusionRule>, selected: Option<usize>) {
        if self.pending.borrow().is_some() {
            return;
        }
        self.pending.replace(Some(Change { rules, selected }));
        self.queued.set(true);
        self.update_buttons();
        if unsafe { PostMessageW(self.owner.get(), WM_NULL, 0, 0) } == 0 {
            self.queued.set(false);
            self.pending.take();
            self.status(tr(Msg::RuleSendError));
            self.update_buttons();
        }
    }

    fn save(&self) {
        let mut rules = self.rules.borrow().clone();
        let index = self.selected.get().unwrap_or(rules.len());
        let draft = self.form();
        if let Some(rule) = rules.get_mut(index) {
            *rule = draft;
        } else {
            rules.push(draft);
        }
        self.queue(rules, Some(index));
    }

    fn remove(&self) {
        let Some(index) = self.selected.get() else {
            return;
        };
        let mut rules = self.rules.borrow().clone();
        rules.remove(index);
        let next = rules.len().checked_sub(1).map(|last| index.min(last));
        self.queue(rules, next);
    }

    fn example_cues(&self) {
        self.picked_cues.set(false);
        self.cues(
            tr(Msg::RuleExampleProcess),
            tr(Msg::RuleExampleTitle),
            tr(Msg::RuleExampleClass),
        );
    }

    fn cues(&self, process: &str, title: &str, class: &str) {
        for (id, cue) in FIELDS.into_iter().zip([process, title, class]) {
            unsafe { SendMessageW(self.child(id), EM_SETCUEBANNER, 0, wide(cue).as_ptr() as _) };
        }
    }

    fn pick_window(&self) {
        let candidates = crate::native::exclusion_candidates();
        if candidates.is_empty() {
            self.status(tr(Msg::RuleNoWindows));
            return;
        }
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                self.status(tr(Msg::RuleWindowListError));
                return;
            }
            for (index, (process, title, class)) in candidates.iter().enumerate() {
                let caption = [title.as_str(), process.as_str()]
                    .into_iter()
                    .filter(|v| !v.is_empty())
                    .collect::<Vec<_>>()
                    .join("  —  ");
                let caption = if caption.is_empty() {
                    class.as_str()
                } else {
                    &caption
                };
                AppendMenuW(
                    menu,
                    MF_STRING,
                    index + 1,
                    wide(&caption.replace('&', "&&")).as_ptr(),
                );
            }
            let mut r = RECT::default();
            GetWindowRect(self.child(PICK), &mut r);
            let picked = TrackPopupMenuEx(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTALIGN,
                r.right,
                r.bottom,
                self.hwnd.get(),
                null_mut(),
            );
            DestroyMenu(menu);
            if let Some((process, title, class)) = picked
                .checked_sub(1)
                .and_then(|i| candidates.get(i as usize))
            {
                self.loading.set(true);
                SendMessageW(self.child(MODE), CB_SETCURSEL, 0, 0);
                set_text(self.child(PROCESS), process);
                // Prefer a single stable identifier. For protected processes
                // the title is still a usable starting point.
                set_text(
                    self.child(TITLE_FIELD),
                    if process.is_empty() { title } else { "" },
                );
                set_text(
                    self.child(CLASS_FIELD),
                    if process.is_empty() && title.is_empty() {
                        class
                    } else {
                        ""
                    },
                );
                self.picked_cues.set(true);
                self.cues(process, title, class);
                self.loading.set(false);
                self.status("");
                self.update_buttons();
                SetFocus(self.child(PROCESS));
            }
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let editor = (*(lp as *const CREATESTRUCTW)).lpCreateParams as *const Editor;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, editor as isize);
            (*editor).hwnd.set(hwnd);
        }
        let Some(ui) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Editor).as_ref() else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        match msg {
            WM_COMMAND => {
                let id = (wp & 0xffff) as i32;
                let event = (wp >> 16) as u32;
                match (id, event) {
                    (LIST, LBN_SELCHANGE) => {
                        let index = SendMessageW(ui.child(LIST), LB_GETCURSEL, 0, 0);
                        if index >= 0 {
                            ui.navigate(Some(index as usize));
                        }
                    }
                    (NEW, BN_CLICKED) => {
                        ui.navigate(None);
                        SetFocus(ui.child(PROCESS));
                    }
                    (REMOVE, BN_CLICKED) => ui.remove(),
                    (SAVE, BN_CLICKED) => ui.save(),
                    (PICK, BN_CLICKED) => ui.pick_window(),
                    (PROCESS | TITLE_FIELD | CLASS_FIELD, EN_CHANGE) | (MODE, CBN_SELCHANGE)
                        if !ui.loading.get() =>
                    {
                        ui.status("");
                        ui.update_buttons();
                    }
                    (PROCESS | TITLE_FIELD | CLASS_FIELD, EN_SETFOCUS | EN_KILLFOCUS) => {
                        InvalidateRect(hwnd, null(), 0);
                    }
                    (IDCANCEL, _) if lp == 0 => {
                        SendMessageW(hwnd, WM_CLOSE, 0, 0);
                    }
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                if ui.pending.borrow().is_none() && ui.may_discard() {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_DPICHANGED => {
                ui.dpi.set((wp as u32 & 0xffff).max(96));
                let r = &*(lp as *const RECT);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                ui.refresh_fonts();
                ui.layout();
                0
            }
            WM_SIZE if !ui.child(SAVE).is_null() => {
                ui.layout();
                0
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                ui.theme();
                0
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                ui.paint(dc);
                EndPaint(hwnd, &ps);
                0
            }
            WM_PRINTCLIENT => {
                ui.paint(wp as HDC);
                0
            }
            WM_ERASEBKGND => 1,
            WM_MEASUREITEM if lp != 0 => {
                let item = &mut *(lp as *mut MEASUREITEMSTRUCT);
                item.itemHeight = ui.p(if item.CtlID as i32 == LIST { 46 } else { 26 }) as u32;
                1
            }
            WM_DRAWITEM if lp != 0 => {
                ui.draw(&*(lp as *const DRAWITEMSTRUCT));
                1
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
                ui.control_color(wp as HDC, lp as HWND, msg)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                ui.hwnd.set(null_mut());
                ui.fonts.take();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
