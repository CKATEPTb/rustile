use super::*;
use Shortcut::*;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

const HEADING: i32 = 511;
pub(super) const HINT: &str =
    "Дважды щёлкните по действию и нажмите нужное сочетание. Esc отменяет ввод.";
const GROUPS: [(&str, &[Shortcut]); 3] = [
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
];

pub(super) struct EditorFonts {
    ui: Fonts,
    title: HFONT,
}

impl Drop for EditorFonts {
    fn drop(&mut self) {
        if !self.title.is_null() {
            unsafe { DeleteObject(self.title) };
        }
    }
}

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

        create(hwnd, w!("STATIC"), "Горячие клавиши", HEADING, 0, 0)?;
        create(hwnd, w!("STATIC"), HINT, STATUS, 0, 0)?;
        button(hwnd, "Сбросить все", RESET_ALL)?;
        let view = create(
            hwnd,
            w!("STATIC"),
            "",
            VIEW,
            WS_VSCROLL | WS_CLIPCHILDREN,
            WS_EX_CONTROLPARENT,
        )?;
        unsafe { SetWindowSubclass(view, Some(view_proc), 1, self as *const Self as usize) };
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
                button(view, action.label(), row_id(action))?;
                let reset = button(
                    view,
                    &format!("Сбросить сочетание для «{}»", action.label()),
                    RESET_ROW + action.index() as i32,
                )?;
                if !tooltip.is_null() {
                    let tool = TTTOOLINFOW {
                        cbSize: size_of::<TTTOOLINFOW>() as u32,
                        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                        hwnd: view,
                        uId: reset as usize,
                        lpszText: w!("Сбросить сочетание").cast_mut(),
                        ..Default::default()
                    };
                    unsafe { SendMessageW(tooltip, TTM_ADDTOOLW, 0, (&raw const tool) as isize) };
                }
            }
        }
        Ok(())
    }

    pub(super) fn update_fonts(&self, hwnd: HWND) {
        let ui = Fonts::new(self.dpi.get());
        let title = unsafe {
            CreateFontW(
                -self.p(25),
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                DEFAULT_CHARSET.into(),
                0,
                0,
                CLEARTYPE_QUALITY.into(),
                0,
                w!("Segoe UI"),
            )
        };
        unsafe {
            SendMessageW(child(hwnd, HEADING), WM_SETFONT, title as usize, 1);
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
        *self.fonts.borrow_mut() = Some(EditorFonts { ui, title });
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
        self.p(if width < self.p(480) { 16 } else { 24 })
            .max((width - self.p(1072)) / 2)
    }

    pub(super) fn layout(&self, hwnd: HWND) {
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client) };
        let p = |n| self.p(n);
        let width = client.right;
        let margin = self.margin(width);
        let stacked = width < p(410);
        let button_w = p(140);
        let body_w = (width - 2 * margin).max(1);
        let button_x = if stacked {
            margin
        } else {
            width - margin - button_w
        };
        let hint_y = p(55);
        let hint_h = if width < p(640) { p(40) } else { p(22) };
        let button_y = if stacked {
            hint_y + hint_h + p(4)
        } else {
            p(20)
        };
        place(
            child(hwnd, HEADING),
            margin,
            p(17),
            if stacked {
                body_w
            } else {
                body_w - button_w - p(12)
            },
            p(34),
        );
        place(child(hwnd, STATUS), margin, hint_y, body_w, hint_h);
        place(child(hwnd, RESET_ALL), button_x, button_y, button_w, p(30));
        let top = if stacked {
            button_y + p(44)
        } else {
            hint_y + hint_h + p(12)
        };
        let height = (client.bottom - top).max(1);
        let view = child(hwnd, VIEW);
        place(view, 0, top, width, height);
        self.layout_rows(view, width, height);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    fn layout_rows(&self, view: HWND, width: i32, height: i32) {
        let p = |n| self.p(n);
        let columns = width >= p(794);
        let stacked = width < p(430);
        let row_h = if stacked { p(54) } else { p(36) };
        let header_h = p(32);
        let row_gap = |action| {
            if matches!(action, GrowLeft | ShrinkLeft | Confirm) {
                p(5)
            } else {
                0
            }
        };
        let mut starts = [0; GROUPS.len()];
        let mut end = [p(5); 2];
        for (i, &(_, actions)) in GROUPS.iter().enumerate() {
            let column = usize::from(columns && i == 2);
            starts[i] = end[column];
            end[column] +=
                header_h + actions.iter().map(|&a| row_h + row_gap(a)).sum::<i32>() + p(16);
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
        let gap = p(22);
        let col = (client.right - 2 * margin - if columns { gap } else { 0 })
            / if columns { 2 } else { 1 };
        for (i, &(_, actions)) in GROUPS.iter().enumerate() {
            let x = if columns && i == 2 {
                margin + col + gap
            } else {
                margin
            };
            let mut y = starts[i] - self.scroll.get();
            place(
                child(view, SECTION + i as i32),
                x + p(2),
                y + p(2),
                col - p(4),
                p(25),
            );
            y += header_h;
            let reset_w = p(34);
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
        let Some(fonts) = fonts.as_ref().map(|f| &f.ui) else {
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
            paint::fill(dc, &r, colors.border, p(5));
            let inner = inset(r, 1);
            paint::fill(
                dc,
                &inner,
                if hot { colors.hover } else { colors.control },
                p(4),
            );
            let ink = if disabled {
                colors.disabled
            } else if hot {
                colors.hover_text
            } else {
                colors.text
            };
            let glyph = RECT {
                left: r.left + p(8),
                right: r.left + p(35),
                ..r
            };
            paint::text(dc, glyph, "\u{e777}", fonts.symbol, ink, DT_CENTER);
            paint::text(
                dc,
                RECT {
                    left: glyph.right + p(3),
                    ..r
                },
                "Сбросить все",
                fonts.regular,
                ink,
                DT_LEFT,
            );
        } else {
            let reset = id >= RESET_ROW;
            let action = if reset {
                Shortcut::ALL.get((id - RESET_ROW) as usize).copied()
            } else {
                row_action(id)
            };
            let Some(action) = action else { return };
            let first = matches!(
                action,
                MoveLeft | FocusLeft | StartResize | GrowLeft | ShrinkLeft | Confirm
            );
            let last = matches!(
                action,
                MoveDown | FocusDown | StartResize | GrowDown | ShrinkDown | Cancel
            );
            // The two native buttons meet on a single surface. Only the outside
            // corners of each related block of actions are rounded.
            let block = RECT {
                left: r.left - if reset { p(7) } else { 0 },
                right: r.right + if reset { 0 } else { p(7) },
                top: r.top - if first { 0 } else { p(7) },
                bottom: r.bottom + if last { 0 } else { p(7) },
            };
            let surface = if hot { colors.hover } else { colors.control };
            paint::fill(dc, &block, surface, p(6));
            if !last {
                let rule = RECT {
                    left: r.left + if reset { 0 } else { p(12) },
                    right: r.right - if reset { p(9) } else { 0 },
                    top: r.bottom - 1,
                    bottom: r.bottom,
                };
                paint::fill(dc, &rule, colors.border, 0);
            }
            let ink = if hot { colors.hover_text } else { colors.text };
            if reset {
                let ink = if disabled { colors.disabled } else { ink };
                paint::text(dc, r, "\u{e777}", fonts.symbol, ink, DT_CENTER);
            } else {
                self.draw_binding(dc, r, action, fonts, ink);
            }
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

    fn draw_binding(&self, dc: HDC, r: RECT, action: Shortcut, fonts: &Fonts, ink: COLORREF) {
        let colors = self.colors.get();
        let p = |n| self.p(n);
        let stacked = r.bottom - r.top > p(42);
        let label_x = r.left + p(12);
        let mut end = r.right - p(5);
        let y = if stacked {
            r.top + p(26)
        } else {
            (r.top + r.bottom - p(23)) / 2
        };
        let center = |a: i32, b: i32| RECT {
            left: a,
            right: b,
            top: y,
            bottom: y + p(23),
        };
        let is_recording = self.recording(action);
        let keycap = |rect: RECT, label: &str| {
            let stroke = if is_recording {
                colors.accent
            } else {
                colors.border
            };
            paint::fill(dc, &rect, stroke, p(4));
            paint::fill(
                dc,
                &inset(rect, 1),
                if is_recording {
                    colors.accent
                } else {
                    colors.background
                },
                p(3),
            );
            paint::text(
                dc,
                rect,
                label,
                fonts.small,
                if is_recording {
                    colors.on_accent
                } else {
                    colors.text
                },
                DT_CENTER | DT_END_ELLIPSIS,
            );
        };
        if is_recording {
            let text = self.binding_text(action);
            let extent = measure(dc, fonts.small, &text) + p(16);
            let left = if stacked {
                label_x
            } else {
                (end - extent).max(label_x)
            };
            keycap(center(left, (left + extent).min(end)), &text);
            end = left - p(8);
        } else {
            let chord = self.settings.get().chord(action);
            let key = Chord::key_label(chord.key);
            let parts = [
                (MOD_CONTROL, "Ctrl"),
                (MOD_ALT, "Alt"),
                (MOD_WIN, "Win"),
                (MOD_SHIFT, "Shift"),
                (0, key.as_str()),
            ];
            let mut caps = Vec::with_capacity(parts.len());
            for (bit, name) in parts {
                if bit == 0 || chord.mods & bit != 0 {
                    caps.push((name, (measure(dc, fonts.small, name) + p(12)).max(p(23))));
                }
            }
            let span = caps.iter().map(|cap| cap.1).sum::<i32>() + (caps.len() as i32 - 1) * p(3);
            let left = if stacked { label_x } else { end - span };
            let mut x = left;
            for (name, width) in caps {
                keycap(center(x, x + width), name);
                x += width + p(3);
            }
            end = left - p(7);
        }
        let label = RECT {
            left: label_x,
            right: if stacked { r.right } else { end },
            bottom: if stacked { r.top + p(26) } else { r.bottom },
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
