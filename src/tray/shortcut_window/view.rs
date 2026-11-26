use super::slots::{button, create};
use super::*;
use Shortcut::*;
use windows_sys::Win32::System::SystemServices::SS_OWNERDRAW;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

use Topic::{Desktops, Resize, Windows};

mod pagination;
use pagination::{ActionLines, CellPlacement, Grid, Section};

#[derive(Clone, Copy)]
enum Topic {
    Windows,
    Desktops,
    Resize,
}

impl Topic {
    fn column(self) -> usize {
        match self {
            Windows => 0,
            Desktops => 1,
            Resize => 2,
        }
    }
}

pub(super) struct Group {
    topic: Topic,
    pub(super) title: &'static str,
    pub(super) icon: &'static str,
    pub(super) actions: &'static [Shortcut],
}

// Native tab order follows the upper columns, then the full-width transfer section.
pub(super) const GROUPS: [Group; 8] = [
    Group {
        topic: Windows,
        title: "Управление окнами",
        icon: "\u{e8a9}",
        actions: &[ToggleMosaic, Shortcut::CloseWindow, ToggleMaximize],
    },
    Group {
        topic: Windows,
        title: "Переключение фокуса",
        icon: "\u{e81d}",
        actions: &[FocusLeft, FocusRight, FocusUp, FocusDown],
    },
    Group {
        topic: Windows,
        title: "Перемещение окон",
        icon: "\u{e7c2}",
        actions: &[MoveLeft, MoveRight, MoveUp, MoveDown],
    },
    Group {
        topic: Desktops,
        title: "Смена рабочего стола",
        icon: "\u{e7c4}",
        actions: &[PreviousDesktop, NextDesktop],
    },
    Group {
        topic: Desktops,
        title: "Выбор стола по номеру",
        icon: "\u{ea37}",
        actions: &[
            Desktop1, Desktop2, Desktop3, Desktop4, Desktop5, Desktop6, Desktop7, Desktop8,
            Desktop9, Desktop10,
        ],
    },
    Group {
        topic: Resize,
        title: "Режим изменения размера",
        icon: "\u{e740}",
        actions: &[
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
    },
    Group {
        topic: Resize,
        title: "Изменение размера без входа в режим",
        icon: "\u{e945}",
        actions: &[
            QuickLeft,
            QuickRight,
            QuickUp,
            QuickDown,
            QuickReverseLeft,
            QuickReverseRight,
            QuickReverseUp,
            QuickReverseDown,
        ],
    },
    Group {
        topic: Desktops,
        title: "Перенос окна между столами",
        icon: "\u{e8ab}",
        actions: &[
            MovePreviousDesktop,
            MoveNextDesktop,
            MoveDesktop1,
            MoveDesktop2,
            MoveDesktop3,
            MoveDesktop4,
            MoveDesktop5,
            MoveDesktop6,
            MoveDesktop7,
            MoveDesktop8,
            MoveDesktop9,
            MoveDesktop10,
        ],
    },
];

pub(super) fn group_title(action: Shortcut) -> &'static str {
    GROUPS
        .iter()
        .find(|group| group.actions.contains(&action))
        .map_or("", |group| group.title)
}

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
            WS_CLIPCHILDREN,
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
        let prev = button(view, "Предыдущая страница", PAGE_PREV)?;
        let next = button(view, "Следующая страница", PAGE_NEXT)?;
        create(view, w!("STATIC"), "", PAGE_LABEL, SS_OWNERDRAW, 0)?;
        self.hint(view, prev, w!("Предыдущая страница"));
        self.hint(view, next, w!("Следующая страница"));
        for (i, group) in GROUPS.iter().enumerate() {
            let label = group.title;
            create(
                view,
                w!("STATIC"),
                label,
                SECTION + i as i32,
                SS_OWNERDRAW,
                0,
            )?;
            for &action in group.actions {
                create(
                    view,
                    w!("STATIC"),
                    action.label(),
                    LABEL_ROW + action.index() as i32,
                    SS_OWNERDRAW,
                    0,
                )?;
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
        let header_height = p(if compact { 58 } else { 60 });
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
        self.layout_rows(view, width, height, None);
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }

    fn action_lines(&self, view: HWND, dc: HDC, action: Shortcut, col: i32) -> ActionLines {
        let p = |n| self.p(n);
        let id = action.index() as i32;
        let label = child(view, LABEL_ROW + id);
        let add = child(view, ADD_ROW + id);
        let reset = child(view, RESET_ROW + id);
        let tool_width = p(24);
        let text_area = (col - 2 * tool_width - p(4)).max(1);
        let label_width = self
            .fonts
            .borrow()
            .as_ref()
            .map_or(p(120), |fonts| {
                appearance::text_width(dc, fonts.regular, action.label()) + p(3)
            })
            .min(text_area);
        let header = [
            CellPlacement::new(label, 0, label_width),
            CellPlacement::new(add, col - 2 * tool_width, tool_width),
            CellPlacement::new(reset, col - tool_width, tool_width),
        ];
        let removable = self.selected.get() == action
            && self
                .capture
                .borrow()
                .as_ref()
                .is_some_and(|c| c.original.is_some());
        let slots = self.slots.borrow()[action.index()].clone();
        let bindings: Vec<_> = slots
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let trash = (removable && i == self.selected_slot.get()).then_some(slot.remove);
                let width = self
                    .binding_width(dc, action, i)
                    .min((col - i32::from(trash.is_some()) * tool_width).max(1));
                (slot.row, width, trash)
            })
            .collect();
        let gap = p(6);
        let bindings_width: i32 = bindings
            .iter()
            .map(|&(_, width, trash)| width + i32::from(trash.is_some()) * tool_width)
            .sum::<i32>()
            + bindings.len().saturating_sub(1) as i32 * gap;
        // Keep complete alternatives together. When they do not fit beside their
        // label, let the entire group use the full column below it.
        let inline = label_width + p(5) + bindings_width <= text_area;
        let mut lines = vec![Vec::from(header)];
        let mut x = if inline {
            text_area - bindings_width
        } else {
            0
        };
        if !inline {
            lines.push(Vec::new());
        }
        for (row, width, trash) in bindings {
            let total = width + i32::from(trash.is_some()) * tool_width;
            if !inline && x > 0 && x + total > col {
                lines.push(Vec::new());
                x = 0;
            }
            let line = lines.last_mut().unwrap();
            line.push(CellPlacement::new(row, x, width));
            x += width;
            if let Some(remove) = trash {
                line.push(CellPlacement::new(remove, x, tool_width));
                x += tool_width;
            }
            x += gap;
        }
        ActionLines { header, lines }
    }

    fn layout_rows(&self, view: HWND, width: i32, height: i32, reveal: Option<HWND>) {
        if view.is_null() {
            return;
        }
        let p = |n| self.p(n);
        let columns = if width >= p(1140) {
            3
        } else if width >= p(780) {
            2
        } else {
            1
        };
        let margin = self.margin(width);
        let gap = p(20);
        let col = ((width - 2 * margin - (columns - 1) as i32 * gap) / columns as i32).max(1);
        let dc = unsafe { GetDC(view) };
        let sections: [_; GROUPS.len()] = std::array::from_fn(|i| {
            let group = &GROUPS[i];
            Section {
                heading: child(view, SECTION + i as i32),
                topic: group.topic.column(),
                actions: group
                    .actions
                    .iter()
                    .map(|&a| self.action_lines(view, dc, a, col))
                    .collect(),
            }
        });
        unsafe { ReleaseDC(view, dc) };
        let grid = Grid {
            columns,
            margin,
            col,
            gap,
            top: p(6),
            row: p(24),
            heading: p(26),
            section_gap: p(8),
        };
        let [upper @ .., transfers] = &sections;
        let mut pages = grid.pages(upper, transfers, height - p(5));
        let pager_height = p(34);
        if pages.len() > 1 {
            pages = grid.pages(upper, transfers, height - pager_height - p(4));
        }
        let recording = self.capture.borrow().is_some().then(|| self.selected_row());
        let page = recording
            .or(reveal)
            .and_then(|hwnd| {
                pages
                    .iter()
                    .position(|page| page.iter().any(|item| item.hwnd == hwnd))
            })
            .unwrap_or_else(|| self.page.get())
            .min(pages.len() - 1);
        self.page.set(page);
        self.pages.set(pages.len());
        let page_count = pages.len();
        let items = &mut pages[page];
        let prev = child(view, PAGE_PREV);
        let next = child(view, PAGE_NEXT);
        if page_count > 1 {
            let mid = width / 2;
            let w = p(28);
            let label = child(view, PAGE_LABEL);
            let page_text = format!("{} / {}", page + 1, page_count);
            let dc = unsafe { GetDC(view) };
            let number_w = self.fonts.borrow().as_ref().map_or(p(50), |fonts| {
                appearance::text_width(dc, fonts.small, &page_text) + p(20)
            });
            unsafe { ReleaseDC(view, dc) };
            let y = height - pager_height;
            let left = mid - number_w / 2;
            for (hwnd, x, width) in [
                (prev, left - w, w),
                (label, left, number_w),
                (next, left + number_w, w),
            ] {
                items.push(pagination::Placement {
                    hwnd,
                    x,
                    y,
                    width,
                    height: w,
                });
            }
            caption(label, &page_text);
        }
        // Compute the entire visible set before changing any windows. A page
        // change may hide the currently focused native button.
        let visible: std::collections::HashSet<_> = items.iter().map(|item| item.hwnd).collect();
        let focus = unsafe { GetFocus() };
        let needs_focus = unsafe { GetParent(focus) == view }
            && (!visible.contains(&focus)
                || (focus == prev && page == 0)
                || (focus == next && page + 1 == page_count));
        unsafe {
            for item in items.iter() {
                SetWindowPos(
                    item.hwnd,
                    null_mut(),
                    item.x,
                    item.y,
                    item.width,
                    item.height,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
                );
            }
            EnableWindow(prev, i32::from(page > 0));
            EnableWindow(next, i32::from(page + 1 < page_count));
            let mut hwnd = GetWindow(view, GW_CHILD);
            while !hwnd.is_null() {
                if !visible.contains(&hwnd)
                    && GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0
                {
                    ShowWindow(hwnd, SW_HIDE);
                }
                hwnd = GetWindow(hwnd, GW_HWNDNEXT);
            }
            // Focus notifications can re-enter layout. Send them only after
            // applying this page and keep the input session on its own binding.
            if needs_focus
                && let Some(target) = recording.or_else(|| {
                    items
                        .iter()
                        .find(|item| {
                            row_control(GetDlgCtrlID(item.hwnd))
                                .is_some_and(|(kind, _)| kind == Control::Row)
                        })
                        .map(|item| item.hwnd)
                })
            {
                SetFocus(target);
            }
        }
    }

    pub(super) fn reveal_row(&self, hwnd: HWND, row: HWND) {
        if row.is_null() || unsafe { GetWindowLongW(row, GWL_STYLE) as u32 } & WS_VISIBLE != 0 {
            return;
        }
        let view = child(hwnd, VIEW);
        let mut viewport = RECT::default();
        unsafe { GetClientRect(view, &mut viewport) };
        self.layout_rows(view, viewport.right, viewport.bottom, Some(row));
    }

    pub(super) fn set_page(&self, hwnd: HWND, delta: i32) {
        let page = self
            .page
            .get()
            .saturating_add_signed(delta as isize)
            .min(self.pages.get().saturating_sub(1));
        if self.page.replace(page) != page {
            self.layout(hwnd);
        }
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
