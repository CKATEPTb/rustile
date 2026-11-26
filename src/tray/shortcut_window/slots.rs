use super::*;
use windows_sys::Win32::UI::Shell::SetWindowSubclass;

pub(super) fn create(
    parent: HWND,
    class: *const u16,
    label: &str,
    id: i32,
    style: u32,
    ex: u32,
) -> Result<HWND, String> {
    let hwnd = unsafe {
        CreateWindowExW(
            ex,
            class,
            wide(label).as_ptr(),
            WS_VISIBLE | WS_CHILD | style,
            0,
            0,
            1,
            1,
            parent,
            id as _,
            GetModuleHandleW(null()),
            null(),
        )
    };
    if hwnd.is_null() {
        Err(io::Error::last_os_error().to_string())
    } else {
        Ok(hwnd)
    }
}

pub(super) fn button(parent: HWND, label: &str, id: i32) -> Result<HWND, String> {
    let hwnd = create(
        parent,
        w!("BUTTON"),
        label,
        id,
        WS_TABSTOP | BS_OWNERDRAW as u32 | BS_NOTIFY as u32,
        0,
    )?;
    unsafe { SetWindowSubclass(hwnd, Some(view::button_proc), 1, 0) };
    Ok(hwnd)
}

impl ShortcutWindow {
    pub(super) fn hint(&self, parent: HWND, hwnd: HWND, text: Msg, message: u32) {
        let tooltip = self.tooltip.get();
        if !tooltip.is_null() {
            let tool = TTTOOLINFOW {
                cbSize: size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd: parent,
                uId: hwnd as usize,
                lpszText: localized_wide(text).cast_mut(),
                ..Default::default()
            };
            unsafe { SendMessageW(tooltip, message, 0, (&raw const tool) as isize) };
        }
    }

    pub(super) fn selected_row(&self) -> HWND {
        let slots = self.slots.borrow();
        let slots = &slots[self.selected.get().index()];
        slots
            .get(self.selected_slot.get())
            .or_else(|| slots.first())
            .map_or(null_mut(), |slot| slot.row)
    }

    pub(super) fn sync_slots(&self, action: Shortcut, count: usize) -> Result<(), String> {
        let index = action.index();
        let view = child(self.hwnd.get(), VIEW);
        let existing = self.slots.borrow()[index].len();
        for pos in existing..count {
            let ids = if pos == 0 {
                [ROW, REMOVE_ROW]
            } else {
                [ALT_ROW, ALT_REMOVE]
            };
            let mut handles = [null_mut(); 2];
            for (i, base) in ids.into_iter().enumerate() {
                let hwnd = match button(view, "", base + index as i32) {
                    Ok(hwnd) => hwnd,
                    Err(error) => {
                        for &created in &handles[..i] {
                            unsafe { DestroyWindow(created) };
                        }
                        return Err(error);
                    }
                };
                handles[i] = hwnd;
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, pos as isize) };
            }
            let [row, remove] = handles;
            unsafe { ShowWindow(remove, SW_HIDE) };
            let previous = self.slots.borrow()[index].last().copied();
            if let Some(slot) = previous {
                // Insert new editors in the native tab order next to their action.
                let mut after = slot.remove;
                for hwnd in handles {
                    unsafe {
                        SetWindowPos(
                            hwnd,
                            after,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                    };
                    after = hwnd;
                }
            }
            self.hint(view, row, Msg::UiEditHint, TTM_ADDTOOLW);
            self.hint(view, remove, Msg::UiRemoveShortcut, TTM_ADDTOOLW);
            self.slots.borrow_mut()[index].push(Slot { row, remove });
        }
        let obsolete = {
            let mut slots = self.slots.borrow_mut();
            slots[index].split_off(count)
        };
        for slot in obsolete {
            for hwnd in [slot.row, slot.remove] {
                unsafe {
                    if GetFocus() == hwnd {
                        SetFocus(self.selected_row());
                    }
                    let tool = TTTOOLINFOW {
                        cbSize: size_of::<TTTOOLINFOW>() as u32,
                        uFlags: TTF_IDISHWND,
                        hwnd: view,
                        uId: hwnd as usize,
                        ..Default::default()
                    };
                    SendMessageW(
                        self.tooltip.get(),
                        TTM_DELTOOLW,
                        0,
                        (&raw const tool) as isize,
                    );
                    DestroyWindow(hwnd);
                }
            }
        }
        Ok(())
    }
}
