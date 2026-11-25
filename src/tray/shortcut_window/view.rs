use super::*;
use Shortcut::*;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

const GROUPS: [(&str, &[Shortcut]); 4] = [
    ("Перемещение окон", &[MoveLeft, MoveRight, MoveUp, MoveDown]),
    (
        "Навигация по окнам",
        &[FocusLeft, FocusRight, FocusUp, FocusDown],
    ),
    (
        "Изменение размера",
        &[
            StartResize,
            GrowLeft,
            GrowRight,
            GrowUp,
            GrowDown,
            ShrinkLeft,
            ShrinkRight,
            ShrinkUp,
            ShrinkDown,
            Confirm,
            Cancel,
        ],
    ),
    (
        "Быстрое изменение размера",
        &[
            QuickLeft,
            QuickRight,
            QuickUp,
            QuickDown,
            QuickReverseLeft,
            QuickReverseRight,
            QuickReverseUp,
            QuickReverseDown,
        ],
    ),
];

impl ShortcutWindow {
    pub(super) fn create_controls(&self, hwnd: HWND) -> Result<(), String> {
        let create = |parent: HWND, class, label: &str, id: i32, style, ex| {
            let window = unsafe {
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
            if window.is_null() {
                Err(io::Error::last_os_error().to_string())
            } else {
                Ok(window)
            }
        };
        let button = |parent: HWND, label: &str, id: i32| -> Result<HWND, String> {
            let hwnd = create(
                parent,
                w!("BUTTON"),
                label,
                id,
                WS_TABSTOP | BS_OWNERDRAW as u32 | BS_NOTIFY as u32,
                0,
            )?;
            unsafe { SetWindowSubclass(hwnd, Some(button_proc), 1, 0) };
            Ok(hwnd)
        };

        create(hwnd, w!("STATIC"), "", STATUS, 0, 0)?;
        let view = create(
            hwnd,
            w!("STATIC"),
            "",
            VIEW,
            WS_VSCROLL | WS_CLIPCHILDREN,
            WS_EX_CONTROLPARENT,
        )?;
        unsafe { SetWindowSubclass(view, Some(view_proc), 1, self as *const Self as usize) };
        button(view, "Сбросить всё", RESET_ALL)?;
        let tooltip = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                null(),
                WS_POPUP | TTS_NOPREFIX,
                0,
                0,
                0,
                0,
                hwnd,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        for (i, &(label, actions)) in GROUPS.iter().enumerate() {
            create(view, w!("STATIC"), label, SECTION + i as i32, 0, 0)?;
            for &action in actions {
                let row = button(view, action.label(), row_id(action))?;
                let reset = button(
                    view,
                    &format!("Сбросить сочетание для «{}»", action.label()),
                    RESET_ROW + action.index() as i32,
                )?;
                if !tooltip.is_null() {
                    for (control, hint) in [
                        (row, w!("Изменить сочетание: двойной щелчок или F2")),
                        (reset, w!("Сбросить сочетание")),
                    ] {
                        let tool = TTTOOLINFOW {
                            cbSize: size_of::<TTTOOLINFOW>() as u32,
                            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                            hwnd: view,
                            uId: control as usize,
                            lpszText: hint.cast_mut(),
                            ..Default::default()
                        };
                        unsafe {
                            SendMessageW(tooltip, TTM_ADDTOOLW, 0, (&raw const tool) as isize)
                        };
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn update_fonts(&self, hwnd: HWND) {
        let ui = Fonts::new(self.dpi.get());
        unsafe {
            SendMessageW(child(hwnd, STATUS), WM_SETFONT, ui.regular as usize, 1);
            for i in 0..GROUPS.len() {
                SendMessageW(
                    child(child(hwnd, VIEW), SECTION + i as i32),
                    WM_SETFONT,
                    ui.heading as usize,
                    1,
                );
            }
        }
        *self.fonts.borrow_mut() = Some(ui);
    }

    pub(super) fn apply_theme(&self, hwnd: HWND) {
        let palette = Colors::current();
        self.colors.set(palette);
        let dark = i32::from(palette.dark);
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&raw const dark).cast(),
                size_of_val(&dark) as u32,
            );
            let theme = if palette.dark {
                w!("DarkMode_Explorer")
            } else {
                w!("Explorer")
            };
            SetWindowTheme(child(hwnd, VIEW), theme, null());
            RedrawWindow(
                hwnd,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
    }

    fn margin(&self, width: i32) -> i32 {
        self.p(14).max((width - self.p(1020)) / 2)
    }

    pub(super) fn set_status(&self, message: &str) {
        let hwnd = self.hwnd.get();
        let status = child(hwnd, STATUS);
        if !status.is_null() && (!message.is_empty() || unsafe { GetWindowTextLengthW(status) } > 0)
        {
            caption(status, message);
            self.layout(hwnd);
        }
    }

    pub(super) fn layout(&self, hwnd: HWND) {
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client) };
        let width = client.right;
        let margin = self.margin(width);
        let status = child(hwnd, STATUS);
        let count = unsafe { GetWindowTextLengthW(status) };
        let mut top = 0;
        if count > 0 {
            let mut text = vec![0; count as usize + 1];
            let mut bounds = RECT {
                right: (width - 2 * margin).max(1),
                ..Default::default()
            };
            unsafe {
                GetWindowTextW(status, text.as_mut_ptr(), count + 1);
                let dc = GetDC(hwnd);
                let old = SelectObject(dc, SendMessageW(status, WM_GETFONT, 0, 0) as HGDIOBJ);
                DrawTextW(
                    dc,
                    text.as_mut_ptr(),
                    count,
                    &mut bounds,
                    DT_CALCRECT | DT_NOPREFIX | DT_WORDBREAK,
                );
                SelectObject(dc, old);
                ReleaseDC(hwnd, dc);
            }
            let height = bounds.bottom.max(self.p(18));
            place(status, margin, self.p(7), width - 2 * margin, height);
            top = height + self.p(10);
        }
        unsafe {
            ShowWindow(
                status,
                if count > 0 {
                    SW_SHOWNOACTIVATE
                } else {
                    SW_HIDE
                },
            );
        }
        let height = (client.bottom - top).max(1);
        let view = child(hwnd, VIEW);
        place(view, 0, top, width, height);
        self.layout_rows(view, width, height);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    fn layout_rows(&self, view: HWND, width: i32, height: i32) {
        let p = |n| self.p(n);
        let columns = width >= p(710);
        let column_for = |section: usize| if columns { section % 2 } else { 0 };
        let stacked = width < p(400);
        let row_h = if stacked { p(43) } else { p(29) };
        let header_h = p(29);
        let row_gap = |action| {
            if matches!(action, GrowLeft | ShrinkLeft | Confirm | QuickReverseLeft) {
                p(3)
            } else {
                0
            }
        };
        let mut starts = [0; GROUPS.len()];
        let mut end = [p(7); 2];
        for (i, &(_, actions)) in GROUPS.iter().enumerate() {
            let column = column_for(i);
            starts[i] = end[column];
            end[column] +=
                header_h + actions.iter().map(|&a| row_h + row_gap(a)).sum::<i32>() + p(10);
        }
        let total = end[0].max(end[1]);
        self.scroll
            .set(self.scroll.get().clamp(0, (total - height).max(0)));
        let scroll = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: total - 1,
            nPage: height.max(1) as u32,
            nPos: self.scroll.get(),
            ..Default::default()
        };
        let mut client = RECT::default();
        unsafe {
            SetScrollInfo(view, SB_VERT, &scroll, 1);
            GetClientRect(view, &mut client)
        };
        let margin = self.margin(width);
        let gap = p(18);
        let col = (client.right - 2 * margin - if columns { gap } else { 0 })
            / if columns { 2 } else { 1 };
        let reset_w = p(104);
        let reset_x = client.right - margin - reset_w;
        place(
            child(view, RESET_ALL),
            reset_x,
            p(7) - self.scroll.get(),
            reset_w,
            p(24),
        );
        for (i, &(_, actions)) in GROUPS.iter().enumerate() {
            let x = margin + column_for(i) as i32 * (col + gap);
            let mut y = starts[i] - self.scroll.get();
            place(
                child(view, SECTION + i as i32),
                x + p(2),
                y + p(2),
                if i == usize::from(columns) {
                    reset_x - x - p(6)
                } else {
                    col - p(4)
                },
                p(21),
            );
            y += header_h;
            let reset_w = p(26);
            for &action in actions {
                y += row_gap(action);
                place(self.row(action), x, y, col - reset_w, row_h);
                place(
                    child(view, RESET_ROW + action.index() as i32),
                    x + col - reset_w,
                    y,
                    reset_w,
                    row_h,
                );
                y += row_h;
            }
        }
    }

    pub(super) fn reveal_row(&self, hwnd: HWND, row: HWND) {
        let view = child(hwnd, VIEW);
        let mut rect = RECT::default();
        let mut button = RECT::default();
        unsafe {
            GetClientRect(view, &mut rect);
            GetWindowRect(row, &mut button);
            MapWindowPoints(null_mut(), view, (&raw mut button).cast(), 2);
        }
        let delta = if button.top < 0 {
            button.top
        } else {
            (button.bottom - rect.bottom).max(0)
        };
        if delta != 0 {
            self.scroll_by(hwnd, delta);
        }
    }

    fn scroll_by(&self, hwnd: HWND, delta: i32) {
        self.scroll.set(self.scroll.get().saturating_add(delta));
        let mut window = RECT::default();
        let mut viewport = RECT::default();
        let view = child(hwnd, VIEW);
        unsafe {
            GetClientRect(hwnd, &mut window);
            GetClientRect(view, &mut viewport)
        };
        self.layout_rows(view, window.right, viewport.bottom);
    }

    pub(super) fn wheel(&self, hwnd: HWND, delta: i32) {
        self.scroll_by(hwnd, -delta * self.p(72) / 120);
    }

    pub(super) fn background(&self, hwnd: HWND, dc: HDC) {
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        paint::background(dc, &rect, self.colors.get());
    }

    pub(super) fn draw(&self, item: &DRAWITEMSTRUCT) {
        let colors = self.colors.get();
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else {
            return;
        };
        let p = |n| self.p(n);
        let id = item.CtlID as i32;
        let r = item.rcItem;
        let dc = item.hDC;
        unsafe { SetBkMode(dc, TRANSPARENT as i32) };
        paint::background(dc, &r, colors);
        let disabled = item.itemState & ODS_DISABLED != 0;
        let mut cursor = POINT::default();
        let hovered =
            unsafe { GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == item.hwndItem };
        let hot = !disabled && (hovered || item.itemState & ODS_SELECTED != 0);
        if id == RESET_ALL {
            if hot {
                paint::fill(dc, &r, colors.hover, p(4));
            }
            let ink = if disabled {
                colors.disabled
            } else if hot {
                colors.hover_text
            } else {
                colors.text
            };
            paint::text(dc, r, "Сбросить всё", fonts.regular, ink, DT_CENTER);
        } else if let Some(action) = row_action(id) {
            let selected = action == self.selected.get();
            if selected || hot {
                paint::fill(
                    dc,
                    &r,
                    if hot { colors.hover } else { colors.control },
                    p(4),
                );
            }
            self.draw_binding(dc, r, action, fonts, hot);
        } else if reset_action(id).is_some() && !disabled {
            if hot {
                paint::fill(dc, &r, colors.hover, p(4));
            }
            let ink = if hot {
                colors.hover_text
            } else {
                colors.secondary
            };
            paint::text(dc, r, "\u{e777}", fonts.symbol, ink, DT_CENTER);
        }
        if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
            let outline = inset(r, p(2));
            let color = if row_action(id).is_some_and(|action| self.recording(action)) {
                colors.accent
            } else {
                colors.secondary
            };
            unsafe {
                let region = CreateRoundRectRgn(
                    outline.left,
                    outline.top,
                    outline.right + 1,
                    outline.bottom + 1,
                    p(8),
                    p(8),
                );
                if !region.is_null() {
                    SetDCBrushColor(dc, color);
                    FrameRgn(dc, region, GetStockObject(DC_BRUSH), p(1), p(1));
                    DeleteObject(region);
                }
            }
        }
    }

    fn draw_binding(&self, dc: HDC, r: RECT, action: Shortcut, fonts: &Fonts, hot: bool) {
        let p = |n| self.p(n);
        let colors = self.colors.get();
        let ink = if hot { colors.hover_text } else { colors.text };
        let stacked = r.bottom - r.top > p(38);
        let value = self.binding_text(action);
        let left = r.left + p(7);
        let right = r.right - p(6);
        let key_width = measure(dc, fonts.regular, &value);
        let value_x = if stacked {
            left
        } else {
            (right - key_width - p(2)).max(left)
        };
        let label = RECT {
            left,
            right: if stacked { right } else { value_x - p(5) },
            bottom: if stacked { r.top + p(22) } else { r.bottom },
            ..r
        };
        paint::text(
            dc,
            label,
            action.label(),
            fonts.regular,
            ink,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        let binding = RECT {
            left: value_x,
            right,
            top: if stacked { label.bottom } else { r.top },
            ..r
        };
        let value_ink = if hot {
            colors.hover_text
        } else if self.recording(action) {
            colors.accent
        } else {
            colors.secondary
        };
        paint::text(
            dc,
            binding,
            &value,
            fonts.regular,
            value_ink,
            if stacked { DT_LEFT } else { DT_RIGHT },
        );
    }
}

fn measure(dc: HDC, font: HFONT, text: &str) -> i32 {
    let utf16 = wide(text);
    let mut size = SIZE::default();
    unsafe {
        let old = SelectObject(dc, font);
        GetTextExtentPoint32W(dc, utf16.as_ptr(), (utf16.len() - 1) as i32, &mut size);
        SelectObject(dc, old);
    }
    size.cx
}

fn inset(r: RECT, n: i32) -> RECT {
    RECT {
        left: r.left + n,
        top: r.top + n,
        right: r.right - n,
        bottom: r.bottom - n,
    }
}
fn place(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
    unsafe {
        SetWindowPos(
            hwnd,
            null_mut(),
            x,
            y,
            width.max(1),
            height.max(1),
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
}

unsafe extern "system" fn view_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let ui = &*(data as *const ShortcutWindow);
        let parent = GetParent(hwnd);
        match msg {
            WM_ERASEBKGND => {
                ui.background(hwnd, wp as HDC);
                1
            }
            WM_DRAWITEM | WM_COMMAND | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_MOUSEWHEEL => {
                SendMessageW(parent, msg, wp, lp)
            }
            WM_VSCROLL => {
                let mut si = SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_ALL,
                    ..Default::default()
                };
                GetScrollInfo(hwnd, SB_VERT, &mut si);
                let delta = match (wp & 0xffff) as i32 {
                    SB_LINEUP => -ui.p(31),
                    SB_LINEDOWN => ui.p(31),
                    SB_PAGEUP => -(si.nPage as i32),
                    SB_PAGEDOWN => si.nPage as i32,
                    SB_TOP => -si.nPos,
                    SB_BOTTOM => si.nMax - si.nPos,
                    SB_THUMBTRACK | SB_THUMBPOSITION => si.nTrackPos - si.nPos,
                    _ => 0,
                };
                if delta != 0 {
                    ui.scroll_by(parent, delta);
                }
                0
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(view_proc), 1);
                DefSubclassProc(hwnd, msg, wp, lp)
            }
            _ => DefSubclassProc(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn button_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        match msg {
            WM_LBUTTONDBLCLK if row_action(GetDlgCtrlID(hwnd)).is_some() => {
                SetFocus(hwnd);
                let command = GetDlgCtrlID(hwnd) as usize | ((BN_DOUBLECLICKED as usize) << 16);
                SendMessageW(GetParent(hwnd), WM_COMMAND, command, hwnd as isize);
                return 0;
            }
            WM_MOUSEMOVE => {
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_QUERY,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tracking);
                if tracking.dwFlags & TME_LEAVE == 0 || tracking.hwndTrack != hwnd {
                    tracking.dwFlags = TME_LEAVE;
                    tracking.hwndTrack = hwnd;
                    TrackMouseEvent(&mut tracking);
                    InvalidateRect(hwnd, null(), 0);
                }
            }
            WM_MOUSELEAVE => {
                InvalidateRect(hwnd, null(), 0);
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(button_proc), 1);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}
