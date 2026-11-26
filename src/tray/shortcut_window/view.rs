use super::slots::{button, create};
use super::*;
use Shortcut::*;
use windows_sys::Win32::System::SystemServices::SS_OWNERDRAW;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

pub(super) const GROUPS: [(&str, &[Shortcut]); 8] = [
    ("Перемещение окон", &[MoveLeft, MoveRight, MoveUp, MoveDown]),
    (
        "Переключение фокуса",
        &[FocusLeft, FocusRight, FocusUp, FocusDown],
    ),
    (
        "Режим изменения размера",
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
        ],
    ),
    (
        "Изменение размера без входа в режим",
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
    (
        "Управление окнами",
        &[Shortcut::CloseWindow, ToggleMaximize, ToggleMosaic],
    ),
    (
        "Перенос между рабочими столами",
        &[MovePreviousDesktop, MoveNextDesktop],
    ),
    ("Смена рабочего стола", &[PreviousDesktop, NextDesktop]),
    (
        "Выбор стола по номеру",
        &[
            Desktop1, Desktop2, Desktop3, Desktop4, Desktop5, Desktop6, Desktop7, Desktop8,
            Desktop9, Desktop10,
        ],
    ),
];

// Keep keyboard traversal in reading order at every column count.
const READING_ORDER: [usize; 8] = [0, 1, 4, 5, 2, 3, 6, 7];

impl ShortcutWindow {
    pub(super) fn create_controls(&self, hwnd: HWND) -> Result<(), String> {
        create(hwnd, w!("STATIC"), "", STATUS, 0, 0)?;
        create(hwnd, w!("STATIC"), "", PREVIEW, SS_OWNERDRAW, 0)?;
        let reset_all = button(hwnd, "Сбросить всё", RESET_ALL)?;
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
        self.tooltip.set(tooltip);
        self.hint(hwnd, reset_all, w!("Сбросить все сочетания"));
        for i in READING_ORDER {
            let (label, actions) = GROUPS[i];
            create(
                view,
                w!("STATIC"),
                label,
                SECTION + i as i32,
                SS_OWNERDRAW,
                0,
            )?;
            for &action in actions {
                self.sync_slots(action, self.settings.borrow().bindings(action).len().max(1))?;
                let add = button(
                    view,
                    &format!("Добавить сочетание для «{}» — {label}", action.label()),
                    ADD_ROW + action.index() as i32,
                )?;
                let reset = button(
                    view,
                    &format!("Сбросить сочетания для «{}» — {label}", action.label()),
                    RESET_ROW + action.index() as i32,
                )?;
                self.hint(view, add, w!("Добавить сочетание"));
                self.hint(view, reset, w!("Восстановить сочетания действия"));
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
            let caption = if palette.high_contrast {
                DWMWA_COLOR_DEFAULT
            } else {
                self.header_color()
            };
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR as u32,
                (&raw const caption).cast(),
                size_of_val(&caption) as u32,
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
        let edge = if width < self.p(620) { 14 } else { 24 };
        self.p(edge).max((width - self.p(1460)) / 2)
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
        let p = |n| self.p(n);
        let compact = width < p(620);
        let content = width - 2 * margin;
        let reset_width = p(if compact { 34 } else { 133 });
        let header_height = p(if compact { 92 } else { 86 });
        let button_height = p(32);
        let reset_y = (header_height - button_height) / 2;
        let preview_width = content - reset_width - p(if compact { 10 } else { 20 });
        place(
            child(hwnd, PREVIEW),
            margin,
            0,
            preview_width,
            header_height,
        );
        place(
            child(hwnd, RESET_ALL),
            width - margin - reset_width,
            reset_y,
            reset_width,
            button_height,
        );
        let mut top = header_height;
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
            place(status, margin, top + p(4), content, height);
            top += height + p(12);
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
        top += p(1);
        let height = (client.bottom - top).max(1);
        let view = child(hwnd, VIEW);
        place(view, 0, top, width, height);
        self.layout_rows(view, width, height);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    fn layout_rows(&self, view: HWND, width: i32, height: i32) {
        let p = |n| self.p(n);
        let columns = if width >= p(1140) {
            3
        } else if width >= p(780) {
            2
        } else {
            1
        };
        let margin = self.margin(width);
        let gap = p(24);
        let button_w = p(25);
        let tools_gap = p(5);
        let tools_w = 4 * button_w + tools_gap;
        let column_width = |viewport| (viewport - 2 * margin - (columns - 1) * gap) / columns;
        let header_h = p(34);
        let row_gap = |action| {
            if matches!(action, GrowLeft | ShrinkLeft | QuickReverseLeft) {
                p(5)
            } else {
                0
            }
        };
        let dc = unsafe { GetDC(view) };
        let measure = |viewport| {
            let col = column_width(viewport);
            let heights = Shortcut::ALL.map(|a| self.action_row_height(dc, a, col - tools_w));
            let mut starts = [(0, 0); GROUPS.len()];
            let mut ends = [p(12); 3];
            for (order, &i) in READING_ORDER.iter().enumerate() {
                let column = match columns {
                    3 if order >= 6 => 2,
                    3 if order >= 4 => 1,
                    2 if order >= 5 => 1,
                    _ => 0,
                };
                starts[i] = (column, ends[column]);
                let rows: i32 = GROUPS[i]
                    .1
                    .iter()
                    .map(|&a| heights[a.index()] + row_gap(a) + self.extra_height(a))
                    .sum();
                ends[column] += header_h + rows + p(15);
            }
            (heights, starts, *ends.iter().max().unwrap())
        };
        // Measure without the scrollbar first so rows can return to one line
        // when the user widens the window or assigns a shorter chord.
        let mut measured = measure(width);
        if measured.2 > height {
            let scrollbar = unsafe { GetSystemMetricsForDpi(SM_CXVSCROLL, self.dpi.get()) };
            measured = measure(width - scrollbar);
        }
        unsafe { ReleaseDC(view, dc) };
        let (heights, starts, total) = measured;
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
        let col = column_width(client.right);
        for (i, &(_, actions)) in GROUPS.iter().enumerate() {
            let (column, start) = starts[i];
            let x = margin + column as i32 * (col + gap);
            let mut y = start - self.scroll.get();
            place(child(view, SECTION + i as i32), x, y, col, header_h);
            y += header_h;
            for &action in actions {
                y += row_gap(action);
                let row_h = heights[action.index()];
                let label_width = col - tools_w;
                let slots = self.slots.borrow()[action.index()].clone();
                for (index, slot) in slots.into_iter().enumerate() {
                    let h = if index == 0 { row_h } else { self.p(27) };
                    place(slot.row, x, y, label_width, h);
                    place(slot.edit, x + label_width, y, button_w, h);
                    place(slot.remove, x + label_width + button_w, y, button_w, h);
                    if index == 0 {
                        place(
                            child(view, ADD_ROW + action.index() as i32),
                            x + col - 2 * button_w,
                            y,
                            button_w,
                            h,
                        );
                        place(
                            child(view, RESET_ROW + action.index() as i32),
                            x + col - button_w,
                            y,
                            button_w,
                            h,
                        );
                    }
                    y += h;
                }
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
        let colors = self.colors.get();
        if hwnd == self.hwnd.get() {
            paint::fill(dc, &rect, self.header_color(), 0);
            let mut view = RECT::default();
            if unsafe { GetWindowRect(child(hwnd, VIEW), &mut view) } != 0 {
                unsafe { MapWindowPoints(null_mut(), hwnd, (&raw mut view).cast(), 2) };
                rect.bottom = view.top;
                rect.top = rect.bottom - self.p(1);
                paint::fill(dc, &rect, colors.border, 0);
            }
        } else {
            paint::background(dc, &rect, colors);
        }
    }
}

pub(super) fn place(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
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

pub(super) unsafe extern "system" fn button_proc(
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
                RemoveWindowSubclass(hwnd, Some(button_proc), 1);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}
