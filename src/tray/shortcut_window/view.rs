use super::*;
use Shortcut::*;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

const GROUPS: [(&str, &[Shortcut]); 4] = [
    ("Перемещение окон", &[MoveLeft, MoveRight, MoveUp, MoveDown]),
    (
        "Навигация по окнам",
        &[FocusLeft, FocusRight, FocusUp, FocusDown],
    ),
    ("Приложение", &[Quit]),
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
        let view = create(
            hwnd,
            w!("STATIC"),
            "",
            VIEW,
            WS_VSCROLL | WS_CLIPCHILDREN,
            WS_EX_CONTROLPARENT,
        )?;
        unsafe { SetWindowSubclass(view, Some(view_proc), 1, self as *const Self as usize) };
        let button = WS_TABSTOP | BS_OWNERDRAW as u32 | BS_NOTIFY as u32;
        for (i, (label, actions)) in GROUPS.iter().enumerate() {
            create(view, w!("STATIC"), label, SECTION + i as i32, 0, 0)?;
            for &action in *actions {
                let row = create(
                    view,
                    w!("BUTTON"),
                    action.label(),
                    row_id(action),
                    button,
                    0,
                )?;
                unsafe { SetWindowSubclass(row, Some(row_proc), 1, 0) };
            }
        }
        create(hwnd, w!("STATIC"), "", EDIT_TITLE, 0, 0)?;
        for (i, (_, label)) in MODS.iter().enumerate() {
            let check = create(
                hwnd,
                w!("BUTTON"),
                label,
                MOD_BASE + i as i32,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                0,
            )?;
            unsafe { SetWindowSubclass(check, Some(input_proc), 1, self as *const Self as usize) };
        }
        // The label immediately precedes the combo in native dialog order,
        // which supplies its accessible name for keyboard and assistive tools.
        create(hwnd, w!("STATIC"), "Клавиша", KEY_LABEL, 0, 0)?;
        let combo = create(
            hwnd,
            w!("COMBOBOX"),
            "",
            KEY,
            WS_TABSTOP
                | WS_VSCROLL
                | (CBS_HASSTRINGS | CBS_DROPDOWNLIST | CBS_OWNERDRAWFIXED) as u32,
            0,
        )?;
        unsafe { SetWindowSubclass(combo, Some(input_proc), 1, self as *const Self as usize) };
        for &key in Chord::keys() {
            let label = wide(&Chord::key_label(key));
            if unsafe { SendMessageW(combo, CB_ADDSTRING, 0, label.as_ptr() as isize) } < 0 {
                return Err("Не удалось заполнить список клавиш.".into());
            }
        }
        create(hwnd, w!("BUTTON"), "Применить", APPLY, button, 0)?;
        create(hwnd, w!("STATIC"), "", STATUS, 0, 0)?;
        Ok(())
    }

    pub(super) fn update_fonts(&self, hwnd: HWND) {
        let fonts = Fonts::new(self.dpi.get());
        let view = child(hwnd, VIEW);
        unsafe {
            for i in 0..GROUPS.len() {
                SendMessageW(
                    child(view, SECTION + i as i32),
                    WM_SETFONT,
                    fonts.heading as usize,
                    1,
                );
            }
            for id in MOD_BASE..=STATUS {
                SendMessageW(child(hwnd, id), WM_SETFONT, fonts.regular as usize, 1);
            }
            SendMessageW(
                child(hwnd, KEY),
                CB_SETITEMHEIGHT,
                usize::MAX,
                self.p(26) as isize,
            );
            SendMessageW(child(hwnd, KEY), CB_SETITEMHEIGHT, 0, self.p(26) as isize);
            SendMessageW(
                child(hwnd, EDIT_TITLE),
                WM_SETFONT,
                fonts.heading as usize,
                1,
            );
            SendMessageW(
                child(hwnd, KEY_LABEL),
                WM_SETFONT,
                fonts.regular as usize,
                1,
            );
        }
        *self.fonts.borrow_mut() = Some(fonts);
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
            // Standard combo and checkbox chrome follow the Windows theme.
            let theme = if palette.dark {
                w!("DarkMode_Explorer")
            } else {
                w!("Explorer")
            };
            for id in MOD_BASE..=KEY {
                let control = child(hwnd, id);
                if !control.is_null() {
                    SetWindowTheme(control, theme, null());
                }
            }
            RedrawWindow(
                hwnd,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
    }

    fn footer_height(&self, width: i32) -> i32 {
        self.p(if width < self.p(610) { 151 } else { 112 })
    }

    pub(super) fn layout(&self, hwnd: HWND) {
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client) };
        let view = child(hwnd, VIEW);
        let top = (client.bottom - self.footer_height(client.right)).max(0);
        place(view, 0, 0, client.right, top);
        self.layout_rows(view, client.right, top);

        let p = |n| self.p(n);
        let margin = p(20);
        let narrow = client.right < p(610);
        let controls = top + p(39);
        let input_y = if narrow { controls + p(37) } else { controls };
        place(
            child(hwnd, EDIT_TITLE),
            margin,
            top + p(10),
            (client.right - 2 * margin).max(1),
            p(24),
        );
        for (i, x) in [0, 61, 122, 183].into_iter().enumerate() {
            place(
                child(hwnd, MOD_BASE + i as i32),
                margin + p(x),
                controls,
                p(58),
                p(30),
            );
        }
        let key_x = if narrow { margin } else { margin + p(254) };
        place(child(hwnd, KEY_LABEL), key_x, input_y + p(6), p(58), p(22));
        let combo_x = key_x + p(63);
        let apply_w = p(111).min((client.right / 3).max(1));
        let apply_x = client.right - margin - apply_w;
        let combo_w = (apply_x - combo_x - p(10)).clamp(1, p(152));
        place(child(hwnd, KEY), combo_x, input_y + p(2), combo_w, p(232));
        place(child(hwnd, APPLY), apply_x, input_y, apply_w, p(31));
        place(
            child(hwnd, STATUS),
            margin,
            input_y + p(38),
            client.right - 2 * margin,
            p(34),
        );
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    fn layout_rows(&self, view: HWND, width: i32, height: i32) {
        let p = |n| self.p(n);
        let split = width >= p(670);
        let group_height = |count: usize| p(27) + count as i32 * p(31) + p(13);
        let group_start = |index: usize| {
            if split && index == 3 {
                return p(16);
            }
            p(16)
                + GROUPS[..index]
                    .iter()
                    .map(|(_, a)| group_height(a.len()))
                    .sum::<i32>()
        };
        let total = GROUPS
            .iter()
            .enumerate()
            .map(|(i, (_, a))| group_start(i) + group_height(a.len()))
            .max()
            .unwrap_or(0);
        self.scroll
            .set(self.scroll.get().clamp(0, (total - height).max(0)));
        let scroll = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: total.saturating_sub(1),
            nPage: height.max(1) as u32,
            nPos: self.scroll.get(),
            ..Default::default()
        };
        let mut client = RECT::default();
        unsafe {
            SetScrollInfo(view, SB_VERT, &scroll, 1);
            GetClientRect(view, &mut client)
        };
        let margin = p(20);
        let gap = p(24);
        let col = if split {
            (client.right - 2 * margin - gap) / 2
        } else {
            client.right - 2 * margin
        };
        for (i, (_, actions)) in GROUPS.iter().enumerate() {
            let x = if split && i == 3 {
                margin + col + gap
            } else {
                margin
            };
            let y = group_start(i) - self.scroll.get();
            place(child(view, SECTION + i as i32), x, y, col, p(25));
            for (j, &action) in actions.iter().enumerate() {
                place(
                    child(view, row_id(action)),
                    x,
                    y + p(27) + j as i32 * p(31),
                    col,
                    p(30),
                );
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
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        self.layout_rows(
            child(hwnd, VIEW),
            rect.right,
            (rect.bottom - self.footer_height(rect.right)).max(0),
        );
    }

    pub(super) fn wheel(&self, hwnd: HWND, delta: i32) {
        self.scroll_by(hwnd, -delta * self.p(62) / 120);
    }

    pub(super) fn background(&self, hwnd: HWND, dc: HDC) {
        let mut rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        let colors = self.colors.get();
        paint::fill(dc, &rect, colors.background, 0);
        let y = rect.bottom - self.footer_height(rect.right);
        paint::fill(
            dc,
            &RECT {
                top: y,
                bottom: y + 1,
                ..rect
            },
            colors.border,
            0,
        );
    }

    pub(super) fn draw(&self, item: &DRAWITEMSTRUCT) {
        let colors = self.colors.get();
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let p = |n| self.p(n);
        let dc = item.hDC;
        let r = item.rcItem;
        let pressed = item.itemState & ODS_SELECTED != 0;
        let disabled = item.itemState & ODS_DISABLED != 0;
        unsafe { SetBkMode(dc, TRANSPARENT as i32) };
        paint::fill(dc, &r, colors.background, 0);
        if item.CtlID as i32 == KEY {
            if let Some(&key) = Chord::keys().get(item.itemID as usize) {
                paint::fill(
                    dc,
                    &r,
                    if pressed {
                        colors.accent
                    } else {
                        colors.control
                    },
                    0,
                );
                paint::text(
                    dc,
                    RECT {
                        left: r.left + p(8),
                        ..r
                    },
                    &Chord::key_label(key),
                    fonts.regular,
                    if pressed {
                        colors.on_accent
                    } else {
                        colors.text
                    },
                    DT_LEFT,
                );
            }
        } else if let Some(action) = row_action(item.CtlID as i32) {
            let chosen = action == self.selected.get();
            let mut cursor = POINT::default();
            let hovered = unsafe {
                GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == item.hwndItem
            };
            let hot = pressed || hovered;
            if hot || chosen {
                paint::fill(dc, &r, colors.hover, p(5));
            }
            let chord = self.settings.get().chord(action).label();
            let mut width = RECT::default();
            let text = wide(&chord);
            unsafe {
                let old = SelectObject(dc, fonts.small);
                DrawTextW(
                    dc,
                    text.as_ptr(),
                    (text.len() - 1) as i32,
                    &mut width,
                    DT_CALCRECT | DT_SINGLELINE,
                );
                SelectObject(dc, old);
            }
            let badge = RECT {
                left: (r.right - p(6) - width.right - p(14)).max(r.left),
                right: r.right - p(6),
                top: r.top + p(4),
                bottom: r.bottom - p(4),
            };
            let label = RECT {
                left: r.left + p(8),
                right: badge.left - p(6),
                ..r
            };
            paint::text(
                dc,
                label,
                action.label(),
                fonts.regular,
                if hot || chosen {
                    colors.hover_text
                } else {
                    colors.text
                },
                DT_LEFT | DT_END_ELLIPSIS,
            );
            paint::fill(
                dc,
                &badge,
                if chosen {
                    colors.accent
                } else {
                    colors.control
                },
                p(4),
            );
            paint::text(
                dc,
                badge,
                &chord,
                fonts.small,
                if chosen {
                    colors.on_accent
                } else {
                    colors.secondary
                },
                DT_CENTER,
            );
        } else if item.CtlID as i32 == APPLY {
            paint::fill(
                dc,
                &r,
                if disabled || pressed {
                    colors.hover
                } else {
                    colors.accent
                },
                p(5),
            );
            paint::text(
                dc,
                r,
                "Применить",
                fonts.regular,
                if disabled {
                    colors.disabled
                } else if pressed {
                    colors.hover_text
                } else {
                    colors.on_accent
                },
                DT_CENTER,
            );
        }
        if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
            let focus = RECT {
                left: r.left + p(2),
                top: r.top + p(2),
                right: r.right - p(2),
                bottom: r.bottom - p(2),
            };
            unsafe { DrawFocusRect(dc, &focus) };
        }
    }
}

// Keep native checkbox and combobox selection, accessibility and input handling.
// Only paint their faces: classic Win32 controls otherwise stay white on a dark surface.
impl ShortcutWindow {
    fn input_face(&self, hwnd: HWND, dc: HDC) {
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let p = |n| self.p(n);
        let colors = self.colors.get();
        unsafe {
            let mut r = RECT::default();
            GetClientRect(hwnd, &mut r);
            SetBkMode(dc, TRANSPARENT as i32);
            let id = GetDlgCtrlID(hwnd);
            let focused = GetFocus() == hwnd;
            paint::fill(dc, &r, colors.background, 0);
            if id == KEY {
                let border = if focused {
                    colors.accent
                } else {
                    colors.border
                };
                paint::fill(dc, &r, border, p(5));
                let inner = RECT {
                    left: r.left + 1,
                    top: r.top + 1,
                    right: r.right - 1,
                    bottom: r.bottom - 1,
                };
                paint::fill(dc, &inner, colors.control, p(4));
                let current = SendMessageW(hwnd, CB_GETCURSEL, 0, 0) as usize;
                if let Some(&key) = Chord::keys().get(current) {
                    paint::text(
                        dc,
                        RECT {
                            left: r.left + p(10),
                            right: r.right - p(26),
                            ..r
                        },
                        &Chord::key_label(key),
                        fonts.regular,
                        colors.text,
                        DT_LEFT | DT_END_ELLIPSIS,
                    );
                }
                paint::text(
                    dc,
                    RECT {
                        left: r.right - p(26),
                        ..r
                    },
                    "\u{e70d}",
                    fonts.symbol,
                    colors.secondary,
                    DT_CENTER,
                );
            } else if let Some((_, label)) = MODS.get((id - MOD_BASE) as usize) {
                let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                let left = p(2);
                let size = p(16);
                let top = (r.bottom - size) / 2;
                let check = RECT {
                    left,
                    top,
                    right: left + size,
                    bottom: top + size,
                };
                paint::fill(
                    dc,
                    &check,
                    if checked {
                        colors.accent
                    } else {
                        colors.secondary
                    },
                    p(3),
                );
                if checked {
                    paint::text(
                        dc,
                        check,
                        "\u{e73e}",
                        fonts.symbol,
                        colors.on_accent,
                        DT_CENTER,
                    );
                } else {
                    paint::fill(
                        dc,
                        &RECT {
                            left: left + 1,
                            top: top + 1,
                            right: check.right - 1,
                            bottom: check.bottom - 1,
                        },
                        colors.control,
                        p(2),
                    );
                }
                paint::text(
                    dc,
                    RECT {
                        left: check.right + p(4),
                        ..r
                    },
                    label,
                    fonts.regular,
                    colors.text,
                    DT_LEFT,
                );
                if focused
                    && SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) & UISF_HIDEFOCUS as isize == 0
                {
                    DrawFocusRect(dc, &r);
                }
            }
        }
    }
}

unsafe extern "system" fn input_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let ui = &*(data as *const ShortcutWindow);
        match msg {
            WM_PAINT => {
                let mut info = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut info);
                ui.input_face(hwnd, dc);
                EndPaint(hwnd, &info);
                0
            }
            WM_PRINTCLIENT => {
                ui.input_face(hwnd, wp as HDC);
                0
            }
            WM_ERASEBKGND => 1,
            WM_CTLCOLORLISTBOX => SendMessageW(GetParent(hwnd), msg, wp, lp),
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(input_proc), 1);
                DefSubclassProc(hwnd, msg, wp, lp)
            }
            BM_SETCHECK | CB_SETCURSEL | WM_SETFOCUS | WM_KILLFOCUS | WM_LBUTTONUP | WM_KEYUP
            | WM_UPDATEUISTATE | CB_SHOWDROPDOWN => {
                let result = DefSubclassProc(hwnd, msg, wp, lp);
                InvalidateRect(hwnd, null(), 0);
                result
            }
            _ => DefSubclassProc(hwnd, msg, wp, lp),
        }
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

unsafe extern "system" fn row_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        match msg {
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
                RemoveWindowSubclass(hwnd, Some(row_proc), 1);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}
