use super::*;

const TEMPLATE: usize = 132;
const BODY: i32 = 1010;
const CHORD: i32 = 1011;

impl ShortcutWindow {
    pub(super) fn ask_transfer(
        &self,
        assignment: Assignment,
        conflicts: BindingConflicts,
    ) -> Result<(), String> {
        let owner = self.hwnd.get();
        let transfer = Command::Transfer(assignment, conflicts);
        // Validate against the live profile before offering a destructive change.
        self.settings.borrow().changed(transfer)?;
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
            return Err(render(
                Msg::UiConfirmError,
                &[("error", &io::Error::last_os_error().to_string())],
            ));
        }
        self.transfer.set(Some(transfer));
        self.conflict.set(dialog);
        self.localize_conflict(dialog, assignment, conflicts);
        unsafe {
            EnableWindow(owner, 0);
            SetWindowPos(
                dialog,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            SetFocus(child(dialog, IDCANCEL));
        }
        Ok(())
    }

    pub(super) fn localize_conflict(
        &self,
        dialog: HWND,
        assignment: Assignment,
        conflicts: BindingConflicts,
    ) {
        let owner = self.hwnd.get();
        caption(dialog, tr(Msg::UiConflictTitle));
        caption(child(dialog, IDOK), tr(Msg::UiReassign));
        caption(child(dialog, IDCANCEL), tr(Msg::UiCancel));
        let entries: Vec<_> = conflicts.iter().collect();
        let action = assignment.action();
        let legends = entries
            .iter()
            .map(|(chord, owners)| format!("{} — {}", chord.label(), owners.labels()))
            .collect::<Vec<_>>()
            .join("\r\n");
        caption(child(dialog, CHORD), &legends);
        let message = if let [(_, owners)] = entries.as_slice() {
            render(
                Msg::UiConfirmTransfer,
                &[("action", action.label()), ("owners", &owners.labels())],
            )
        } else {
            render(Msg::UiConfirmRestore, &[("action", action.label())])
        };
        caption(child(dialog, BODY), &message);
        unsafe {
            let mut owner_rect = RECT::default();
            let mut rect = RECT::default();
            GetWindowRect(owner, &mut owner_rect);
            GetWindowRect(dialog, &mut rect);
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top
                + self.fit_conflicts(dialog, conflicts)
                + fit_message(dialog, &message);
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
            SetWindowPos(
                dialog,
                null_mut(),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            RedrawWindow(dialog, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    fn fit_conflicts(&self, dialog: HWND, conflicts: BindingConflicts) -> i32 {
        if conflicts.iter().count() == 1 {
            return 0;
        }
        unsafe {
            let chord = child(dialog, CHORD);
            let mut bounds = RECT::default();
            GetClientRect(chord, &mut bounds);
            let dc = GetDC(chord);
            let Some(fonts) = self.fonts.borrow().as_ref().map(|f| f.regular) else {
                ReleaseDC(chord, dc);
                return 0;
            };
            let mut height = 0;
            for (_, owners) in conflicts.iter() {
                height +=
                    self.p(32) + wrapped(dc, fonts, &owners.labels(), bounds.right) + self.p(5);
            }
            ReleaseDC(chord, dc);
            let delta = height - bounds.bottom;
            SetWindowPos(
                chord,
                null_mut(),
                0,
                0,
                bounds.right,
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            for id in [BODY, IDCANCEL, IDOK] {
                move_down(dialog, id, delta);
            }
            delta
        }
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
                SetFocus(self.selected_row());
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
            move_down(dialog, id, delta);
        }
        delta
    }
}

fn move_down(dialog: HWND, id: i32, delta: i32) {
    unsafe {
        let hwnd = child(dialog, id);
        let mut pos = RECT::default();
        GetWindowRect(hwnd, &mut pos);
        MapWindowPoints(null_mut(), dialog, (&raw mut pos).cast(), 2);
        SetWindowPos(
            hwnd,
            null_mut(),
            pos.left,
            pos.top + delta,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn wrapped(dc: HDC, font: HFONT, text: &str, width: i32) -> i32 {
    let mut rect = RECT {
        right: width,
        ..RECT::default()
    };
    unsafe {
        let old = SelectObject(dc, font);
        let height = DrawTextW(
            dc,
            wide(text).as_ptr(),
            -1,
            &mut rect,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        height
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
                let dc = item.hDC;
                let r = item.rcItem;
                paint::fill(dc, &r, colors.background, 0);
                SetBkMode(dc, TRANSPARENT as i32);
                if item.CtlID == CHORD as u32 {
                    if let Some(Command::Transfer(_, conflicts)) = ui.transfer.get() {
                        let grouped = conflicts.iter().count() > 1;
                        let mut top = r.top;
                        for (chord, owners) in conflicts.iter() {
                            let bottom = if grouped { top + ui.p(30) } else { r.bottom };
                            let caps = keycaps::Keycaps::new(
                                ui,
                                dc,
                                fonts,
                                (chord.mods, Some(chord.key)),
                                keycaps::Size::Compact,
                            );
                            caps.draw(dc, RECT { top, bottom, ..r }, colors);
                            if grouped {
                                let name = owners.labels();
                                top += ui.p(32);
                                let height = wrapped(dc, fonts.regular, &name, r.right - r.left);
                                let mut bounds = RECT {
                                    top,
                                    bottom: top + height,
                                    ..r
                                };
                                let old = SelectObject(dc, fonts.regular);
                                SetTextColor(dc, colors.secondary);
                                DrawTextW(
                                    dc,
                                    wide(&name).as_ptr(),
                                    -1,
                                    &mut bounds,
                                    DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
                                );
                                SelectObject(dc, old);
                                top += height + ui.p(5);
                            }
                        }
                    }
                    return 1;
                }
                let accept = item.CtlID == IDOK as u32;
                let hot = item.itemState & (ODS_SELECTED | ODS_HOTLIGHT) != 0;
                let focused =
                    item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
                let fill = if accept && hot && !colors.high_contrast {
                    appearance::blend(colors.accent, colors.on_accent, 8)
                } else if hot {
                    colors.hover
                } else if accept {
                    colors.accent
                } else {
                    colors.control
                };
                let ink = if hot && (!accept || colors.high_contrast) {
                    colors.hover_text
                } else if accept {
                    colors.on_accent
                } else {
                    colors.text
                };
                let edge = if focused { colors.text } else { colors.border };
                appearance::fill_outlined(
                    dc,
                    appearance::inset(r, ui.p(1)),
                    fill,
                    edge,
                    ui.p(6),
                    ui.p(if focused { 2 } else { 1 }),
                );
                paint::text(
                    dc,
                    r,
                    tr(if accept {
                        Msg::UiReassign
                    } else {
                        Msg::UiCancel
                    }),
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
