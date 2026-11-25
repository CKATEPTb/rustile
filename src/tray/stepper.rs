use super::*;
use menu::Measure;
use windows_sys::Win32::UI::Controls::{EM_SETLIMITTEXT, EM_SETSEL};

const ARROW_BASE: usize = 200;
pub(super) const EDIT_ID: usize = 300;

#[derive(Default)]
pub(super) struct Inline {
    pub editing: Cell<Option<usize>>,
    pub invalid: Cell<bool>,
    pub wheel: Cell<Option<(Measure, i32)>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EndEdit {
    Accept,
    Cancel,
    Blur,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

pub(super) fn arrow_info(id: usize) -> Option<(usize, bool)> {
    (ARROW_BASE..EDIT_ID)
        .contains(&id)
        .then(|| ((id - ARROW_BASE) / 2, !id.is_multiple_of(2)))
}

fn row_hwnd(hwnd: HWND, index: usize) -> HWND {
    unsafe { GetDlgItem(hwnd, (100 + index) as i32) }
}

impl State {
    pub(super) fn measure(&self, index: usize) -> Option<Measure> {
        match self.entries.borrow().get(index)?.action {
            Action::Adjust(measure) => Some(measure),
            _ => None,
        }
    }

    fn pending_value(&self, measure: Measure) -> u8 {
        // Sent mouse messages can arrive before the message pump drains the
        // command queue. Consecutive steps must start at the last proposal.
        let queued = self.commands.borrow();
        let settings = self
            .applying
            .get()
            .into_iter()
            .chain(queued.iter().copied())
            .fold(self.settings.get(), |s, cmd| s.changed(cmd).unwrap_or(s));
        measure.value(settings)
    }

    fn propose(&self, hwnd: HWND, measure: Measure, value: u8) {
        if self.pending_value(measure) == value {
            return;
        }
        let command = measure.command(value);
        let mut queue = self.commands.borrow_mut();
        if let Some(last) = queue.back_mut()
            && std::mem::discriminant(last) == std::mem::discriminant(&command)
        {
            *last = command;
        } else {
            queue.push_back(command);
        }
        drop(queue);
        unsafe { PostMessageW(hwnd, WM_NULL, 0, 0) };
    }

    pub(super) fn step(&self, hwnd: HWND, measure: Measure, delta: i32) {
        let next = (i32::from(self.pending_value(measure)) + delta)
            .clamp(0, i32::from(measure.max())) as u8;
        self.propose(hwnd, measure, next);
    }

    pub(super) fn refresh_rows(&self, hwnd: HWND) {
        let rows = menu::rows(self.settings.get(), self.startup.get());
        for (index, row) in rows.iter().enumerate() {
            let btn = row_hwnd(hwnd, index);
            if btn.is_null() {
                continue;
            }
            let name = row.accessible_name();
            unsafe {
                SetWindowTextW(btn, name.as_ptr());
                EnableWindow(btn, i32::from(row.enabled));
                InvalidateRect(btn, null(), 0);
            }
            if let Action::Adjust(m) = row.action {
                self.enable_arrows(hwnd, index, m);
            }
        }
        *self.entries.borrow_mut() = rows;
    }

    fn enable_arrows(&self, hwnd: HWND, index: usize, m: Measure) {
        let value = m.value(self.settings.get());
        for (offset, enabled) in [value > 0, value < m.max()].into_iter().enumerate() {
            let arrow = unsafe { GetDlgItem(hwnd, (ARROW_BASE + index * 2 + offset) as i32) };
            if !arrow.is_null() {
                unsafe {
                    EnableWindow(arrow, i32::from(enabled));
                    InvalidateRect(arrow, null(), 0);
                }
            }
        }
    }

    pub(super) fn layout_arrows(
        &self,
        hwnd: HWND,
        index: usize,
        m: Measure,
        row: RECT,
        separator: bool,
    ) {
        let [left, _, right] = paint::stepper_rects(row, self.dpi.get(), separator);
        for (offset, rect) in [left, right].into_iter().enumerate() {
            let id = ARROW_BASE + index * 2 + offset;
            unsafe {
                let mut arrow = GetDlgItem(hwnd, id as i32);
                if arrow.is_null() {
                    let verb = if offset == 0 {
                        "Уменьшить"
                    } else {
                        "Увеличить"
                    };
                    let name = wide(&format!("{verb}: {}", m.label()));
                    arrow = CreateWindowExW(
                        0,
                        w!("BUTTON"),
                        name.as_ptr(),
                        WS_CHILD | WS_CLIPSIBLINGS | BS_OWNERDRAW as u32,
                        0,
                        0,
                        0,
                        0,
                        hwnd,
                        id as _,
                        GetModuleHandleW(null()),
                        null(),
                    );
                    if arrow.is_null() {
                        continue;
                    }
                    SetWindowSubclass(arrow, Some(button_proc), 1, 0);
                }
                SetWindowPos(
                    arrow,
                    HWND_TOP,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                );
            }
        }
        self.enable_arrows(hwnd, index, m);
    }

    pub(super) fn arrow_click(&self, hwnd: HWND, index: usize, increasing: bool) {
        if let Some(m) = self.measure(index) {
            self.finish_edit(hwnd, EndEdit::Blur);
            self.focus(hwnd, index);
            self.step(hwnd, m, if increasing { 1 } else { -1 });
        }
    }

    pub(super) fn wheel(&self, hwnd: HWND, wp: WPARAM, lp: LPARAM) {
        // Windows can deliver the wheel to the focused child or directly to the
        // hovered child. The screen coordinates always identify the intended row.
        let mut pt = POINT {
            x: lp as i16 as i32,
            y: (lp >> 16) as i16 as i32,
        };
        unsafe { ScreenToClient(hwnd, &mut pt) };
        let child = unsafe { ChildWindowFromPointEx(hwnd, pt, CWP_SKIPINVISIBLE) };
        let id = unsafe { GetDlgCtrlID(child) } as usize;
        let index = if id == EDIT_ID {
            self.inline.editing.get()
        } else if let Some((i, _)) = arrow_info(id) {
            Some(i)
        } else {
            id.checked_sub(100)
        };
        let Some((index, m)) = index.and_then(|i| self.measure(i).map(|m| (i, m))) else {
            self.inline.wheel.set(None);
            return;
        };
        let rest = self
            .inline
            .wheel
            .get()
            .filter(|(last, _)| *last == m)
            .map_or(0, |(_, n)| n);
        let delta = rest + i32::from((wp >> 16) as i16);
        let notch = WHEEL_DELTA as i32;
        self.inline.wheel.set(Some((m, delta % notch)));
        if delta / notch != 0 {
            if self.inline.editing.get() == Some(index) {
                self.finish_edit(hwnd, EndEdit::Blur);
                self.focus(hwnd, index);
            }
            self.step(hwnd, m, delta / notch);
        }
    }

    pub(super) fn editor(&self, hwnd: HWND) -> Option<HWND> {
        let edit = unsafe { GetDlgItem(hwnd, EDIT_ID as i32) };
        (!edit.is_null()).then_some(edit)
    }

    pub(super) fn start_edit(&self, hwnd: HWND, index: usize) {
        let Some(m) = self.measure(index) else { return };
        self.finish_edit(hwnd, EndEdit::Blur);
        unsafe {
            let edit = if let Some(edit) = self.editor(hwnd) {
                edit
            } else {
                let edit = CreateWindowExW(
                    0,
                    w!("EDIT"),
                    w!(""),
                    WS_CHILD
                        | WS_CLIPSIBLINGS
                        | ES_CENTER as u32
                        | ES_AUTOHSCROLL as u32
                        | ES_NUMBER as u32,
                    0,
                    0,
                    0,
                    0,
                    hwnd,
                    EDIT_ID as _,
                    GetModuleHandleW(null()),
                    null(),
                );
                if edit.is_null() {
                    return;
                }
                SetWindowSubclass(edit, Some(edit_proc), 1, 0);
                SendMessageW(edit, EM_SETLIMITTEXT, 32, 0);
                edit
            };
            let mut rect = RECT::default();
            GetWindowRect(row_hwnd(hwnd, index), &mut rect);
            MapWindowPoints(null_mut(), hwnd, (&raw mut rect).cast(), 2);
            let separator = self.entries.borrow()[index].separator;
            let [_, value, _] = paint::stepper_rects(rect, self.dpi.get(), separator);
            if let Some(fonts) = self.fonts.borrow().as_ref() {
                SendMessageW(edit, WM_SETFONT, fonts.value_font() as usize, 0);
            }
            let h = scale(18, self.dpi.get()).min(value.bottom - value.top);
            // Use a single-line native EDIT centred in the same footprint as
            // the display value so editing doesn't resize the flyout.
            SetWindowTextW(edit, wide(&self.pending_value(m).to_string()).as_ptr());
            self.inline.editing.set(Some(index));
            self.inline.invalid.set(false);
            SetWindowPos(
                edit,
                HWND_TOP,
                value.left,
                (value.top + value.bottom - h) / 2,
                value.right - value.left,
                h,
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
            SetFocus(edit);
            SendMessageW(edit, EM_SETSEL, 0, -1);
        }
    }

    pub(super) fn finish_edit(&self, hwnd: HWND, how: EndEdit) -> bool {
        let Some(index) = self.inline.editing.get() else {
            return true;
        };
        let Some(edit) = self.editor(hwnd) else {
            return false;
        };
        if how != EndEdit::Cancel
            && let Some(m) = self.measure(index)
        {
            let mut buf = [0u16; 34];
            let len = unsafe { GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32) } as usize;
            let text = String::from_utf16_lossy(&buf[..len]);
            let value = text.trim().parse::<u8>().ok().filter(|n| *n <= m.max());
            if let Some(n) = value {
                self.propose(hwnd, m, n);
            } else if how == EndEdit::Accept {
                self.inline.invalid.set(true);
                unsafe {
                    InvalidateRect(row_hwnd(hwnd, index), null(), 0);
                    SendMessageW(edit, EM_SETSEL, 0, -1);
                }
                return false;
            }
        }
        self.inline.editing.set(None);
        self.inline.invalid.set(false);
        unsafe {
            ShowWindow(edit, SW_HIDE);
            InvalidateRect(row_hwnd(hwnd, index), null(), 0);
        }
        if how != EndEdit::Blur {
            self.focus(hwnd, index);
        }
        true
    }
}

unsafe extern "system" fn edit_proc(
    edit: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        let hwnd = GetParent(edit);
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
        if let Some(state) = ptr.as_ref() {
            match msg {
                WM_KEYDOWN if wp as u16 == VK_A && GetKeyState(VK_CONTROL as i32) < 0 => {
                    SendMessageW(edit, EM_SETSEL, 0, -1);
                    return 0;
                }
                WM_KEYDOWN if matches!(wp as u16, VK_RETURN | VK_ESCAPE) => {
                    state.finish_edit(
                        hwnd,
                        if wp as u16 == VK_RETURN {
                            EndEdit::Accept
                        } else {
                            EndEdit::Cancel
                        },
                    );
                    return 0;
                }
                WM_KEYDOWN if matches!(wp as u16, VK_UP | VK_DOWN | VK_TAB) => {
                    if let Some(index) = state.inline.editing.get()
                        && state.finish_edit(hwnd, EndEdit::Accept)
                    {
                        state.key(hwnd, index, wp as u16);
                    }
                    return 0;
                }
                WM_CHAR if matches!(wp as u16, VK_RETURN | VK_ESCAPE | VK_TAB | 1) => return 0,
                WM_KILLFOCUS => {
                    state.finish_edit(hwnd, EndEdit::Blur);
                }
                WM_MOUSEWHEEL => {
                    state.wheel(hwnd, wp, lp);
                    return 0;
                }
                WM_NCDESTROY => {
                    RemoveWindowSubclass(edit, Some(edit_proc), 1);
                }
                _ => {}
            }
        }
        DefSubclassProc(edit, msg, wp, lp)
    }
}
