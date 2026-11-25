use super::*;

const TEMPLATE: usize = 132;
const BODY: i32 = 1010;

impl ShortcutWindow {
    pub(super) fn ask_transfer(
        &self,
        action: Shortcut,
        chord: Chord,
        previous: BindingOwners,
    ) -> Result<(), String> {
        let owner = self.hwnd.get();
        let transfer = Command::ReassignShortcut(action, chord, previous);
        // Validate against the live profile before offering a destructive change.
        self.settings.get().changed(transfer)?;
        let dialog = unsafe {
            CreateDialogParamW(
                GetModuleHandleW(null()),
                TEMPLATE as _,
                owner,
                Some(dialog_proc),
                self as *const Self as isize,
            )
        };
        if dialog.is_null() {
            return Err(format!(
                "Не удалось открыть подтверждение. {}",
                io::Error::last_os_error()
            ));
        }
        self.transfer.set(Some(transfer));
        self.conflict.set(dialog);
        let message = match previous {
            BindingOwners::One(_) => format!(
                "{} уже назначено действию {}. Перенести сочетание на «{}»? Прежнее действие останется без сочетания.",
                chord.label(),
                previous.labels(),
                action.label()
            ),
            BindingOwners::Both(_, _) => format!(
                "{} уже назначено действиям {}. Перенести сочетание на «{}»? Оба прежних действия останутся без сочетания.",
                chord.label(),
                previous.labels(),
                action.label()
            ),
        };
        caption(child(dialog, BODY), &message);
        unsafe {
            let mut owner_rect = RECT::default();
            let mut rect = RECT::default();
            GetWindowRect(owner, &mut owner_rect);
            GetWindowRect(dialog, &mut rect);
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top + fit_message(dialog, &message);
            let mut x = owner_rect.left + (owner_rect.right - owner_rect.left - width) / 2;
            let mut y = owner_rect.top + (owner_rect.bottom - owner_rect.top - height) / 2;
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(
                MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
                &mut info,
            ) != 0
            {
                let work = info.rcWork;
                x = x.clamp(work.left, work.left.max(work.right - width));
                y = y.clamp(work.top, work.top.max(work.bottom - height));
            }
            EnableWindow(owner, 0);
            SetWindowPos(dialog, HWND_TOP, x, y, width, height, SWP_SHOWWINDOW);
            SetFocus(child(dialog, IDCANCEL));
        }
        Ok(())
    }

    pub(super) fn finish_transfer(&self, dialog: HWND, accept: bool) {
        let command = self.transfer.take();
        let owner = self.hwnd.get();
        unsafe {
            let return_focus = GetForegroundWindow() == dialog;
            EnableWindow(owner, 1);
            DestroyWindow(dialog);
            if return_focus {
                SetActiveWindow(owner);
                SetFocus(self.row(self.selected.get()));
            }
        }
        if accept && let Some(command) = command {
            self.submit(command);
        }
    }
}

// Keep only the space actually needed by the localized, wrapped message.
fn fit_message(dialog: HWND, message: &str) -> i32 {
    unsafe {
        let body = child(dialog, BODY);
        let mut bounds = RECT::default();
        GetClientRect(body, &mut bounds);
        let width = bounds.right;
        let original = bounds.bottom;
        let dc = GetDC(body);
        let font = SendMessageW(body, WM_GETFONT, 0, 0) as HFONT;
        let old = SelectObject(dc, font);
        let text = wide(message);
        let height = DrawTextW(
            dc,
            text.as_ptr(),
            -1,
            &mut bounds,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        ReleaseDC(body, dc);
        if height <= 0 {
            return 0;
        }
        let delta = height - original;
        SetWindowPos(
            body,
            null_mut(),
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        for id in [IDCANCEL, IDOK] {
            let button = child(dialog, id);
            let mut pos = RECT::default();
            GetWindowRect(button, &mut pos);
            MapWindowPoints(null_mut(), dialog, (&raw mut pos).cast(), 2);
            SetWindowPos(
                button,
                null_mut(),
                pos.left,
                pos.top + delta,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        delta
    }
}

// A modeless Win32 dialog keeps the manager's event loop running while its
// editor waits for confirmation.
unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    unsafe {
        if msg == WM_INITDIALOG {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
        }
        let Some(ui) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ShortcutWindow).as_ref()
        else {
            return 0;
        };
        let colors = ui.colors.get();
        match msg {
            WM_INITDIALOG | WM_THEMECHANGED | WM_SETTINGCHANGE => {
                let dark = i32::from(colors.dark);
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                    (&raw const dark).cast(),
                    size_of_val(&dark) as u32,
                );
                1
            }
            WM_DRAWITEM if lp != 0 => {
                let item = &*(lp as *const DRAWITEMSTRUCT);
                let fonts = ui.fonts.borrow();
                let Some(fonts) = fonts.as_ref() else {
                    return 0;
                };
                let accept = item.CtlID == IDOK as u32;
                let hot = item.itemState & (ODS_SELECTED | ODS_HOTLIGHT) != 0;
                let focused =
                    item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
                let dc = item.hDC;
                let mut r = item.rcItem;
                paint::fill(dc, &r, colors.background, 0);
                let radius = ui.p(5);
                let fill = if hot {
                    colors.hover
                } else if accept {
                    colors.accent
                } else {
                    colors.control
                };
                let ink = if hot {
                    colors.hover_text
                } else if accept {
                    colors.on_accent
                } else {
                    colors.text
                };
                paint::fill(
                    dc,
                    &r,
                    if focused { colors.text } else { colors.border },
                    radius,
                );
                InflateRect(&mut r, -ui.p(1), -ui.p(1));
                paint::fill(dc, &r, fill, radius - 1);
                SetBkMode(dc, TRANSPARENT as i32);
                paint::text(
                    dc,
                    r,
                    if accept {
                        "Переназначить"
                    } else {
                        "Отмена"
                    },
                    fonts.regular,
                    ink,
                    DT_CENTER,
                );
                1
            }
            WM_CTLCOLORDLG | WM_CTLCOLORSTATIC => {
                let dc = wp as HDC;
                SetTextColor(dc, colors.text);
                SetBkColor(dc, colors.background);
                SetDCBrushColor(dc, colors.background);
                GetStockObject(DC_BRUSH) as isize
            }
            WM_COMMAND if matches!((wp & 0xffff) as i32, IDOK | IDCANCEL) => {
                ui.finish_transfer(hwnd, (wp & 0xffff) as i32 == IDOK);
                1
            }
            WM_CLOSE => {
                ui.finish_transfer(hwnd, false);
                1
            }
            WM_NCDESTROY => {
                ui.conflict.set(null_mut());
                ui.transfer.set(None);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                0
            }
            _ => 0,
        }
    }
}
