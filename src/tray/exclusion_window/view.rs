use super::*;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

impl Editor {
    pub(super) fn p(&self, value: i32) -> i32 {
        scale(value, self.dpi.get())
    }

    fn client(&self) -> RECT {
        let mut rect = RECT::default();
        unsafe { GetClientRect(self.hwnd.get(), &mut rect) };
        rect
    }

    fn divider(&self, client: RECT) -> i32 {
        (client.right / 3).min(self.p(226))
    }

    pub(super) fn create_controls(&self) -> Result<(), String> {
        let make = |class, id, label: &str, flags| unsafe {
            let control = CreateWindowExW(
                0,
                class,
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | flags,
                0,
                0,
                1,
                1,
                self.hwnd.get(),
                id as _,
                GetModuleHandleW(null()),
                null(),
            );
            if control.is_null() {
                Err(io::Error::last_os_error().to_string())
            } else {
                Ok(control)
            }
        };
        let label = |id| make(w!("STATIC"), id, "", SS_NOPREFIX);
        let button = |id| make(w!("BUTTON"), id, "", BS_OWNERDRAW as u32 | WS_TABSTOP);
        label(LIST_LABEL)?;
        make(
            w!("LISTBOX"),
            LIST,
            "",
            WS_TABSTOP
                | WS_VSCROLL
                | LBS_NOTIFY as u32
                | LBS_OWNERDRAWFIXED as u32
                | LBS_HASSTRINGS as u32
                | LBS_NOINTEGRALHEIGHT as u32,
        )?;
        button(NEW)?;
        button(REMOVE)?;
        label(FORM_LABEL)?;
        button(PICK)?;
        label(HINT)?;
        for (id, field) in [
            (PROCESS_LABEL, PROCESS),
            (TITLE_LABEL, TITLE_FIELD),
            (CLASS_LABEL, CLASS_FIELD),
        ] {
            label(id)?;
            let hwnd = make(w!("EDIT"), field, "", ES_AUTOHSCROLL as u32 | WS_TABSTOP)?;
            // A Unicode scalar may occupy two UTF-16 code units. The matcher
            // checks its actual limit rather than truncating valid input here.
            unsafe { SendMessageW(hwnd, EM_SETLIMITTEXT, 2049, 0) };
        }
        label(MODE_LABEL)?;
        let combo = make(
            w!("COMBOBOX"),
            MODE,
            "",
            WS_TABSTOP
                | WS_VSCROLL
                | CBS_DROPDOWNLIST as u32
                | CBS_HASSTRINGS as u32
                | CBS_OWNERDRAWFIXED as u32,
        )?;
        // The closed system combo otherwise draws its own white arrow and
        // bevel on Windows' dark palette. Keep its list and input behavior.
        unsafe { SetWindowSubclass(combo, Some(mode_proc), 1, self as *const Self as usize) };
        label(STATUS)?;
        button(SAVE)?;
        self.localize_controls();
        Ok(())
    }

    pub(super) fn localize_controls(&self) {
        for (id, message) in [
            (LIST_LABEL, Msg::RuleList),
            (NEW, Msg::RuleNew),
            (REMOVE, Msg::RuleDelete),
            (FORM_LABEL, Msg::RuleConditions),
            (PICK, Msg::RulePick),
            (HINT, Msg::RuleHint),
            (PROCESS_LABEL, Msg::RuleProcess),
            (TITLE_LABEL, Msg::RuleTitle),
            (CLASS_LABEL, Msg::RuleClass),
            (MODE_LABEL, Msg::RuleMatch),
            (SAVE, Msg::RuleSave),
        ] {
            set_text(self.child(id), tr(message));
        }
        let combo = self.child(MODE);
        unsafe {
            let selected = SendMessageW(combo, CB_GETCURSEL, 0, 0);
            SendMessageW(combo, CB_RESETCONTENT, 0, 0);
            for (_, name) in MODES {
                SendMessageW(combo, CB_ADDSTRING, 0, wide_message(name) as _);
            }
            SendMessageW(combo, CB_SETCURSEL, selected as usize, 0);
        }
    }

    pub(super) fn refresh_fonts(&self) {
        let fonts = Fonts::new(self.dpi.get());
        let mut hwnd = unsafe { GetWindow(self.hwnd.get(), GW_CHILD) };
        while !hwnd.is_null() {
            let id = unsafe { GetDlgCtrlID(hwnd) };
            let font = match id {
                LIST_LABEL | FORM_LABEL => fonts.heading,
                HINT | STATUS | PROCESS_LABEL | TITLE_LABEL | CLASS_LABEL | MODE_LABEL => {
                    fonts.small
                }
                _ => fonts.regular,
            };
            unsafe {
                SendMessageW(hwnd, WM_SETFONT, font as usize, 0);
                hwnd = GetWindow(hwnd, GW_HWNDNEXT);
            }
        }
        self.fonts.replace(Some(fonts));
        unsafe {
            SendMessageW(self.child(LIST), LB_SETITEMHEIGHT, 0, self.p(46) as isize);
            SendMessageW(
                self.child(MODE),
                CB_SETITEMHEIGHT,
                usize::MAX,
                self.p(24) as isize,
            );
            SendMessageW(self.child(MODE), CB_SETITEMHEIGHT, 0, self.p(26) as isize);
        }
    }

    pub(super) fn theme(&self) {
        let colors = Colors::current();
        self.colors.set(colors);
        let dark = i32::from(colors.dark);
        unsafe {
            DwmSetWindowAttribute(
                self.hwnd.get(),
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&raw const dark).cast(),
                size_of::<i32>() as u32,
            );
            let theme = if colors.high_contrast {
                null()
            } else if colors.dark {
                w!("DarkMode_Explorer")
            } else {
                w!("Explorer")
            };
            for id in [LIST, MODE] {
                let control = self.child(id);
                if !control.is_null() {
                    SetWindowTheme(control, theme, null());
                }
            }
            RedrawWindow(
                self.hwnd.get(),
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ALLCHILDREN,
            );
        }
    }

    pub(super) fn layout(&self) {
        let r = self.client();
        let margin = self.p(18);
        let split = self.divider(r);
        let form = split + margin;
        let right = r.right - margin;
        let foot = r.bottom - self.p(54);
        let bottom = r.bottom - self.p(42);
        let mut pick_w = self.p(138);
        let mut remove_w = self.p(67);
        let mut save_w = self.p(104);
        if let Some(fonts) = self.fonts.borrow().as_ref() {
            let hwnd = self.hwnd.get();
            let dc = unsafe { GetDC(hwnd) };
            if !dc.is_null() {
                let button_width = |key| {
                    paint::text(dc, RECT::default(), tr(key), fonts.regular, 0, DT_CALCRECT)
                        + self.p(16)
                };
                let pair_space = split - margin * 2 - self.p(6);
                remove_w = remove_w
                    .max(button_width(Msg::RuleDelete))
                    .min((pair_space - button_width(Msg::RuleNew)).max(pair_space / 3));
                pick_w = pick_w.max(button_width(Msg::RulePick));
                save_w = save_w.max(button_width(Msg::RuleSave));
                unsafe { ReleaseDC(hwnd, dc) };
            }
        }
        let put = |id, x, y, w: i32, h: i32| unsafe {
            SetWindowPos(
                self.child(id),
                null_mut(),
                x,
                y,
                w.max(1),
                h.max(1),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        };
        let label_y = self.p(16);
        put(LIST_LABEL, margin, label_y, split - margin * 2, self.p(25));
        put(
            FORM_LABEL,
            form,
            label_y,
            right - form - pick_w - self.p(2),
            self.p(25),
        );
        put(PICK, right - pick_w, self.p(12), pick_w, self.p(30));
        put(HINT, form, self.p(47), right - form, self.p(20));
        let fields_top = self.p(78);
        let stride = ((foot - fields_top - self.p(3)) / 4).min(self.p(60));
        let label_h = self.p(18);
        let field_h = (stride - label_h - self.p(6)).clamp(self.p(18), self.p(29));
        for (index, (label, id)) in [
            (PROCESS_LABEL, PROCESS),
            (TITLE_LABEL, TITLE_FIELD),
            (CLASS_LABEL, CLASS_FIELD),
            (MODE_LABEL, MODE),
        ]
        .into_iter()
        .enumerate()
        {
            let y = fields_top + index as i32 * stride;
            put(label, form, y, right - form, label_h);
            if id == MODE {
                put(id, form, y + label_h + self.p(2), right - form, self.p(150));
            } else {
                let text_h = field_h.min(self.p(21));
                put(
                    id,
                    form + self.p(8),
                    y + label_h + self.p(2) + (field_h - text_h) / 2,
                    right - form - self.p(16),
                    text_h,
                );
            }
        }
        let list_top = self.p(52);
        put(
            LIST,
            self.p(8),
            list_top,
            split - self.p(16),
            foot - list_top - self.p(8),
        );
        let left_width = split - margin * 2;
        put(
            NEW,
            margin,
            bottom,
            left_width - self.p(6) - remove_w,
            self.p(31),
        );
        put(
            REMOVE,
            split - margin - remove_w,
            bottom,
            remove_w,
            self.p(31),
        );
        put(
            STATUS,
            form,
            foot + self.p(4),
            right - form - save_w - self.p(10),
            r.bottom - foot - self.p(6),
        );
        put(SAVE, right - save_w, bottom, save_w, self.p(31));
        unsafe { InvalidateRect(self.hwnd.get(), null(), 0) };
    }

    pub(super) fn populate(&self) {
        let list = self.child(LIST);
        unsafe {
            SendMessageW(list, LB_RESETCONTENT, 0, 0);
            let rules = self.rules.borrow();
            for rule in rules.iter() {
                let caption = identifiers(rule).collect::<Vec<_>>().join(" · ");
                SendMessageW(list, LB_ADDSTRING, 0, wide(&caption).as_ptr() as _);
            }
            ShowWindow(list, if rules.is_empty() { SW_HIDE } else { SW_SHOWNA });
            InvalidateRect(self.hwnd.get(), null(), 0);
        }
    }

    pub(super) fn paint(&self, dc: HDC) {
        let r = self.client();
        let c = self.colors.get();
        let split = self.divider(r);
        let foot = r.bottom - self.p(54);
        paint::background(dc, &r, c);
        let line = |rect| paint::fill(dc, &rect, c.border, 0);
        line(RECT {
            left: split,
            right: split + self.p(1),
            ..r
        });
        line(RECT {
            top: foot,
            bottom: foot + self.p(1),
            ..r
        });
        if self.rules.borrow().is_empty()
            && let Some(fonts) = self.fonts.borrow().as_ref()
        {
            let empty = RECT {
                left: self.p(18),
                top: self.p(52),
                right: split - self.p(18),
                bottom: foot,
            };
            unsafe { SetBkMode(dc, TRANSPARENT as _) };
            paint::text(
                dc,
                empty,
                tr(Msg::RuleEmpty),
                fonts.regular,
                c.secondary,
                DT_CENTER,
            );
        }
        let focus = unsafe { GetFocus() };
        for id in FIELDS {
            let hwnd = self.child(id);
            if hwnd.is_null() {
                continue;
            }
            let mut field = RECT::default();
            unsafe {
                GetWindowRect(hwnd, &mut field);
                MapWindowPoints(null_mut(), self.hwnd.get(), (&raw mut field).cast(), 2);
                InflateRect(&mut field, self.p(8), self.p(4));
            }
            paint::fill(
                dc,
                &field,
                if hwnd == focus { c.accent } else { c.border },
                self.p(5),
            );
            unsafe { InflateRect(&mut field, -self.p(1), -self.p(1)) };
            paint::fill(dc, &field, c.control, self.p(4));
        }
    }

    pub(super) fn control_color(&self, dc: HDC, hwnd: HWND, msg: u32) -> LRESULT {
        let c = self.colors.get();
        let id = unsafe { GetDlgCtrlID(hwnd) };
        let surface = if msg == WM_CTLCOLOREDIT || FIELDS.contains(&id) || id == MODE {
            c.control
        } else {
            c.background
        };
        unsafe {
            SetTextColor(
                dc,
                match id {
                    HINT | PROCESS_LABEL | TITLE_LABEL | CLASS_LABEL | MODE_LABEL => c.secondary,
                    STATUS => c.text,
                    _ => c.text,
                },
            );
            SetBkColor(dc, surface);
            SetDCBrushColor(dc, surface);
            GetStockObject(DC_BRUSH) as isize
        }
    }

    pub(super) fn draw(&self, item: &DRAWITEMSTRUCT) {
        let fonts = self.fonts.borrow();
        let Some(f) = fonts.as_ref() else { return };
        let c = self.colors.get();
        let dc = item.hDC;
        let mut r = item.rcItem;
        let selected = item.itemState & ODS_SELECTED != 0;
        let disabled = item.itemState & ODS_DISABLED != 0;
        let focus = item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
        unsafe { SetBkMode(dc, TRANSPARENT as _) };
        paint::background(dc, &r, c);
        match item.CtlID as i32 {
            LIST => {
                let rules = self.rules.borrow();
                let Some(rule) = rules.get(item.itemID as usize) else {
                    return;
                };
                unsafe { InflateRect(&mut r, -self.p(1), -self.p(1)) };
                if selected || focus {
                    paint::fill(dc, &r, if focus { c.accent } else { c.hover }, self.p(5));
                    unsafe { InflateRect(&mut r, -self.p(1), -self.p(1)) };
                    paint::fill(dc, &r, c.hover, self.p(4));
                }
                r.left += self.p(9);
                r.right -= self.p(6);
                let mut identifiers = identifiers(rule);
                let main = identifiers.next().unwrap_or_default();
                let mode = MODES.iter().find(|(mode, _)| *mode == rule.mode).unwrap().1;
                let remaining = identifiers.collect::<Vec<_>>().join(" · ");
                let detail = if remaining.is_empty() {
                    tr(mode)
                } else {
                    &remaining
                };
                let middle = (r.top + r.bottom) / 2;
                let ink = if selected { c.hover_text } else { c.text };
                let secondary = if selected && c.high_contrast {
                    c.hover_text
                } else {
                    c.secondary
                };
                paint::text(
                    dc,
                    RECT {
                        top: r.top + self.p(3),
                        bottom: middle + self.p(2),
                        ..r
                    },
                    main,
                    f.regular,
                    ink,
                    DT_LEFT | DT_END_ELLIPSIS,
                );
                paint::text(
                    dc,
                    RECT {
                        top: middle,
                        bottom: r.bottom - self.p(2),
                        ..r
                    },
                    detail,
                    f.small,
                    secondary,
                    DT_LEFT | DT_END_ELLIPSIS,
                );
            }
            MODE => {
                let Some((_, name)) = MODES.get(item.itemID as usize) else {
                    return;
                };
                let active = selected && item.itemState & ODS_COMBOBOXEDIT == 0;
                paint::fill(dc, &r, if active { c.hover } else { c.control }, 0);
                r.left += self.p(7);
                paint::text(
                    dc,
                    r,
                    tr(*name),
                    f.regular,
                    if active { c.hover_text } else { c.text },
                    DT_LEFT | DT_END_ELLIPSIS,
                );
            }
            id => {
                let primary = id == SAVE;
                let mut cursor = POINT::default();
                let hovering = unsafe {
                    GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == item.hwndItem
                };
                let (face, ink) = if disabled {
                    (c.background, c.disabled)
                } else if primary {
                    (
                        if selected { c.hover } else { c.accent },
                        if selected { c.hover_text } else { c.on_accent },
                    )
                } else if selected || hovering {
                    (c.hover, c.hover_text)
                } else {
                    (c.control, c.text)
                };
                unsafe { InflateRect(&mut r, -self.p(1), -self.p(1)) };
                paint::fill(dc, &r, if focus { c.accent } else { c.border }, self.p(5));
                let inset = self.p(if focus { 2 } else { 1 });
                unsafe { InflateRect(&mut r, -inset, -inset) };
                paint::fill(dc, &r, face, self.p(4));
                paint::text(
                    dc,
                    r,
                    &text_of(item.hwndItem),
                    f.regular,
                    ink,
                    DT_CENTER | DT_END_ELLIPSIS,
                );
            }
        }
    }

    fn draw_mode_face(&self, hwnd: HWND, dc: HDC) {
        let mut r = RECT::default();
        unsafe { GetClientRect(hwnd, &mut r) };
        let c = self.colors.get();
        paint::background(dc, &r, c);
        let fonts = self.fonts.borrow();
        let Some(f) = fonts.as_ref() else { return };
        let focused = unsafe { GetFocus() == hwnd };
        paint::fill(dc, &r, if focused { c.accent } else { c.border }, self.p(5));
        unsafe { InflateRect(&mut r, -self.p(1), -self.p(1)) };
        paint::fill(dc, &r, c.control, self.p(4));
        let index = unsafe { SendMessageW(hwnd, CB_GETCURSEL, 0, 0) } as usize;
        let Some((_, name)) = MODES.get(index) else {
            return;
        };
        unsafe { SetBkMode(dc, TRANSPARENT as _) };
        let chevron = RECT {
            left: r.right - self.p(32),
            ..r
        };
        r.left += self.p(7);
        r.right = chevron.left;
        paint::text(
            dc,
            r,
            tr(*name),
            f.regular,
            c.text,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        paint::text(dc, chevron, "\u{e70d}", f.symbol, c.secondary, DT_CENTER);
    }
}

unsafe extern "system" fn mode_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let ui = &*(data as *const Editor);
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                ui.draw_mode_face(hwnd, dc);
                EndPaint(hwnd, &ps);
                0
            }
            WM_PRINT | WM_PRINTCLIENT => {
                ui.draw_mode_face(hwnd, wp as HDC);
                0
            }
            WM_ERASEBKGND => 1,
            WM_NCPAINT => 0,
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(mode_proc), subclass);
                DefSubclassProc(hwnd, msg, wp, lp)
            }
            _ => {
                let result = DefSubclassProc(hwnd, msg, wp, lp);
                if matches!(
                    msg,
                    WM_SETFOCUS
                        | WM_KILLFOCUS
                        | CB_SHOWDROPDOWN
                        | CB_SETCURSEL
                        | WM_KEYDOWN
                        | WM_LBUTTONUP
                        | WM_ENABLE
                ) {
                    InvalidateRect(hwnd, null(), 0);
                }
                result
            }
        }
    }
}

fn identifiers(rule: &ExclusionRule) -> impl Iterator<Item = &str> {
    [
        rule.process.as_str(),
        rule.title.as_str(),
        rule.class.as_str(),
    ]
    .into_iter()
    .filter(|value| !value.is_empty())
}
