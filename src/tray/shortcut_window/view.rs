//! Lays out the shortcut editor by topic, then paginates its action rows to
//! fit the available work area. Placement and painting share the same groups
//! and order as keyboard navigation.

use super::slots::{button, create};
use super::*;
use Shortcut::*;
use windows_sys::Win32::System::SystemServices::SS_OWNERDRAW;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

use Topic::{Desktops, Resize, Windows};

mod canvas;
mod pagination;
mod position;
pub(super) use canvas::ResizeCanvas;
pub(super) use pagination::Placement;
use pagination::{ActionLines, CellPlacement, Grid, Section};

const THREE_COLUMNS: i32 = 1140;
const MAX_CONTENT_WIDTH: i32 = 1460;
const SIDE_MARGIN: i32 = 24;

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
    pub(super) title: Msg,
    pub(super) icon: &'static str,
    pub(super) actions: &'static [Shortcut],
}

// Native tab order follows the upper columns, then the full-width transfer section.
pub(super) const GROUPS: [Group; 8] = [
    Group {
        topic: Windows,
        title: Msg::UiGroupWindows,
        icon: "\u{e8a9}",
        actions: &[ToggleMosaic, Shortcut::CloseWindow, ToggleMaximize],
    },
    Group {
        topic: Windows,
        title: Msg::UiGroupFocus,
        icon: "\u{e81d}",
        actions: &[FocusLeft, FocusRight, FocusUp, FocusDown],
    },
    Group {
        topic: Windows,
        title: Msg::UiGroupMove,
        icon: "\u{e7c2}",
        actions: &[MoveLeft, MoveRight, MoveUp, MoveDown],
    },
    Group {
        topic: Desktops,
        title: Msg::UiGroupSwitchDesktop,
        icon: "\u{e7c4}",
        actions: &[PreviousDesktop, NextDesktop],
    },
    Group {
        topic: Desktops,
        title: Msg::UiGroupDesktopNumber,
        icon: "\u{ea37}",
        actions: &[
            Desktop1, Desktop2, Desktop3, Desktop4, Desktop5, Desktop6, Desktop7, Desktop8,
            Desktop9, Desktop10,
        ],
    },
    Group {
        topic: Resize,
        title: Msg::UiGroupResize,
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
        title: Msg::UiGroupQuickResize,
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
        title: Msg::UiGroupTransfer,
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
        .map_or("", |group| tr(group.title))
}

impl ShortcutWindow {
    pub(super) fn create_controls(&self, hwnd: HWND) -> Result<(), String> {
        create(hwnd, w!("STATIC"), "", STATUS, 0, 0)?;
        create(hwnd, w!("STATIC"), "", PREVIEW, SS_OWNERDRAW, 0)?;
        button(hwnd, "", RESET_ALL)?;
        let view = create(
            hwnd,
            w!("STATIC"),
            "",
            VIEW,
            WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            WS_EX_CONTROLPARENT,
        )?;
        unsafe { SetWindowSubclass(view, Some(view_proc), 1, self as *const Self as usize) };
        let preview = create(hwnd, w!("STATIC"), "", RESIZE_VIEW, WS_CLIPSIBLINGS, 0)?;
        hide(preview);
        if unsafe { SetWindowSubclass(preview, Some(resize_proc), 1, self as *const Self as usize) }
            == 0
        {
            return Err(io::Error::last_os_error().to_string());
        }
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
        button(view, "", PAGE_PREV)?;
        button(view, "", PAGE_NEXT)?;
        create(view, w!("STATIC"), "", PAGE_LABEL, SS_OWNERDRAW, 0)?;
        for (i, group) in GROUPS.iter().enumerate() {
            create(view, w!("STATIC"), "", SECTION + i as i32, SS_OWNERDRAW, 0)?;
            for &action in group.actions {
                create(
                    view,
                    w!("STATIC"),
                    "",
                    LABEL_ROW + action.index() as i32,
                    SS_OWNERDRAW,
                    0,
                )?;
                self.sync_slots(action, self.settings.borrow().bindings(action).len().max(1))?;
                button(view, "", ADD_ROW + action.index() as i32)?;
                button(view, "", RESET_ROW + action.index() as i32)?;
            }
        }
        self.localize_controls(hwnd, TTM_ADDTOOLW);
        Ok(())
    }

    fn localize_controls(&self, hwnd: HWND, tooltip_message: u32) {
        let view = child(hwnd, VIEW);
        for (parent, id, label, hint) in [
            (hwnd, RESET_ALL, Msg::UiResetAll, Msg::UiResetAllHint),
            (view, PAGE_PREV, Msg::UiPreviousPage, Msg::UiPreviousPage),
            (view, PAGE_NEXT, Msg::UiNextPage, Msg::UiNextPage),
        ] {
            let button = child(parent, id);
            caption(button, tr(label));
            self.hint(parent, button, hint, tooltip_message);
        }
        for (i, group) in GROUPS.iter().enumerate() {
            let category = tr(group.title);
            caption(child(view, SECTION + i as i32), category);
            for &action in group.actions {
                let id = action.index() as i32;
                caption(child(view, LABEL_ROW + id), action.label());
                let args = [("action", action.label()), ("category", category)];
                for (base, label, hint) in [
                    (ADD_ROW, Msg::UiAddForAction, Msg::UiAddShortcut),
                    (RESET_ROW, Msg::UiResetForAction, Msg::UiRestoreAction),
                ] {
                    let button = child(view, base + id);
                    caption(button, &render(label, &args));
                    self.hint(view, button, hint, tooltip_message);
                }
            }
        }
    }

    pub(super) fn localize_window(&self, hwnd: HWND) {
        caption(hwnd, tr(Msg::UiShortcutTitle));
        self.localize_controls(hwnd, TTM_UPDATETIPTEXTW);
        let view = child(hwnd, VIEW);
        for slots in self.slots.borrow().iter() {
            for slot in slots {
                self.hint(view, slot.row, Msg::UiEditHint, TTM_UPDATETIPTEXTW);
                self.hint(view, slot.remove, Msg::UiRemoveShortcut, TTM_UPDATETIPTEXTW);
            }
        }
        self.update_fonts(hwnd);
        caption(child(hwnd, STATUS), "");
        if let Some(Command::Transfer(assignment, conflicts)) = self.transfer.get() {
            self.localize_conflict(self.conflict.get(), assignment, conflicts);
        }
        unsafe {
            RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    pub(super) fn update_fonts(&self, hwnd: HWND) {
        let ui = Fonts::new(self.dpi.get());
        unsafe {
            let dc = GetDC(hwnd);
            let text_width = appearance::text_width(dc, ui.regular, tr(Msg::UiResetAll));
            ReleaseDC(hwnd, dc);
            self.reset_button_width.set(text_width + self.p(48));
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
        self.row_metrics.borrow_mut().fill_with(|| None);
        self.raster.borrow_mut().clear();
    }

    pub(super) fn apply_theme(&self, hwnd: HWND) {
        let palette = Colors::current();
        self.raster.borrow_mut().clear();
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
        let edge = if width < self.p(620) { 14 } else { SIDE_MARGIN };
        self.p(edge).max((width - self.p(MAX_CONTENT_WIDTH)) / 2)
    }

    fn header_height(&self, width: i32) -> i32 {
        self.p(if width < self.p(620) { 58 } else { 60 })
    }

    pub(super) fn fit_on_open(&self, hwnd: HWND, work: RECT) {
        let (mut outer, mut client) = (RECT::default(), RECT::default());
        unsafe {
            GetWindowRect(hwnd, &mut outer);
            GetClientRect(hwnd, &mut client);
        }
        let frame_w = outer.right - outer.left - client.right;
        let frame_h = outer.bottom - outer.top - client.bottom;
        let work_w = work.right - work.left;
        let work_h = work.bottom - work.top;
        let max_w = (work_w - frame_w).max(1);
        let max_h = (work_h - frame_h).max(1);
        let view = child(hwnd, VIEW);
        let mut width = max_w.min(self.p(THREE_COLUMNS));
        let limit = max_w.min(self.p(MAX_CONTENT_WIDTH) + 2 * self.p(SIDE_MARGIN));
        let measure = |width| {
            let top = self.header_height(width) + self.p(1);
            let Some(pages) = self.row_pages(view, width, (max_h - top).max(1)) else {
                return (usize::MAX, max_h);
            };
            let height = if let [page] = pages.as_slice() {
                let bottom = page.iter().map(|p| p.y + p.height).max().unwrap_or(0);
                (top + bottom + self.p(8)).min(max_h)
            } else {
                max_h
            };
            (pages.len(), height)
        };
        let mut layout = measure(width);
        let widest = if width == limit {
            layout
        } else {
            measure(limit)
        };
        let mut size = (width, layout.1);
        let mut best = (layout.0, i64::from(width) * i64::from(layout.1));
        // Only widening enough to remove a page or a wrapped line can improve
        // the fit. Find those widths using the same layout as the visible editor.
        while layout > widest {
            let (mut lo, mut hi) = (width + 1, limit);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if measure(mid) < layout {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            width = lo;
            layout = if width == limit {
                widest
            } else {
                measure(width)
            };
            let rank = (layout.0, i64::from(width) * i64::from(layout.1));
            if rank < best {
                best = rank;
                size = (width, layout.1);
            }
        }
        let (w, h) = (
            (size.0 + frame_w).min(work_w),
            (size.1 + frame_h).min(work_h),
        );
        let placed = unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                work.left + (work_w - w) / 2,
                work.top + (work_h - h) / 2,
                w,
                h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        };
        // A size change lays out the controls synchronously via WM_SIZE.
        if placed == 0 || (client.right, client.bottom) == size {
            self.layout(hwnd);
        }
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
        self.layout_editor(hwnd, false);
    }

    pub(super) fn layout_on_resize(&self, hwnd: HWND) {
        self.layout_editor(hwnd, true);
    }

    pub(super) fn finish_resize(&self, hwnd: HWND) {
        let preview = child(hwnd, RESIZE_VIEW);
        if !preview.is_null() && unsafe { IsWindowVisible(preview) } != 0 {
            // The native controls take over at the exact positions in the last frame.
            self.layout(hwnd);
            hide(preview);
            unsafe {
                RedrawWindow(
                    hwnd,
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                );
            }
        }
        self.resize_canvas.take();
    }

    fn layout_editor(&self, hwnd: HWND, resizing: bool) {
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client) };
        let width = client.right;
        let margin = self.margin(width);
        let status = child(hwnd, STATUS);
        let count = unsafe { GetWindowTextLengthW(status) };
        let p = |n| self.p(n);
        let compact = width < p(620);
        let content = width - 2 * margin;
        let reset_width = if compact {
            p(34)
        } else {
            self.reset_button_width.get()
        };
        let header_height = self.header_height(width);
        let button_height = p(32);
        let reset_y = (header_height - button_height) / 2;
        let preview_width = content - reset_width - p(if compact { 10 } else { 20 });
        position::place_header(
            hwnd,
            child(hwnd, PREVIEW),
            margin,
            0,
            preview_width,
            header_height,
            false,
        );
        position::place_header(
            hwnd,
            child(hwnd, RESET_ALL),
            width - margin - reset_width,
            reset_y,
            reset_width,
            button_height,
            false,
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
            position::place_header(hwnd, status, margin, top + p(4), content, height, false);
            top += height + p(12);
        } else {
            hide(status);
        }
        top += p(1);
        let height = (client.bottom - top).max(1);
        let view = child(hwnd, VIEW);
        if self.sizing.get() {
            // Windows owns the input during a nonclient drag. Lay out the same
            // controls into one paint surface until the native buttons take over.
            let preview = child(hwnd, RESIZE_VIEW);
            self.layout_rows(view, width, height, None, resizing);
            unsafe {
                // The editor clips both sibling surfaces out of its header paint.
                // Keep the native viewport below the header even when a compact
                // header change exposes a strip of its old position.
                let mut old = RECT::default();
                GetWindowRect(view, &mut old);
                MapWindowPoints(null_mut(), hwnd, (&raw mut old).cast(), 2);
                if old.top != top {
                    SetWindowPos(view, null_mut(), 0, top, 0, 0, LAYOUT_FLAGS | SWP_NOSIZE);
                }
                SetWindowPos(
                    preview,
                    HWND_TOP,
                    0,
                    top,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOREDRAW | SWP_SHOWWINDOW,
                );
                InvalidateRect(preview, null(), 0);
                if old.top != top {
                    let header = RECT {
                        bottom: top,
                        ..client
                    };
                    RedrawWindow(
                        hwnd,
                        &header,
                        null_mut(),
                        RDW_INVALIDATE | RDW_ERASE | RDW_NOCHILDREN,
                    );
                }
            }
        } else {
            let same_origin = position::place_header(hwnd, view, 0, top, width, height, true);
            self.layout_rows(view, width, height, None, resizing && same_origin);
        }
    }

    fn action_lines(&self, dc: HDC, fonts: &Fonts, action: Shortcut, col: i32) -> ActionLines {
        let p = |n| self.p(n);
        let row = self.measured_row(dc, fonts, action);
        let tool_width = p(24);
        let text_area = (col - 2 * tool_width - p(4)).max(1);
        let label_width = row.label_width.min(text_area);
        let header = [
            CellPlacement::new(row.label, 0, label_width),
            CellPlacement::new(row.add, col - 2 * tool_width, tool_width),
            CellPlacement::new(row.reset, col - tool_width, tool_width),
        ];
        let total_width = |binding: &keycaps::BindingMetrics| {
            let trash = i32::from(binding.remove.is_some()) * tool_width;
            binding.width.min((col - trash).max(1)) + trash
        };
        let gap = p(6);
        let bindings_width = row.bindings.iter().map(total_width).sum::<i32>()
            + row.bindings.len().saturating_sub(1) as i32 * gap;
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
        for binding in &row.bindings {
            let total = total_width(binding);
            let width = total - i32::from(binding.remove.is_some()) * tool_width;
            if !inline && x > 0 && x + total > col {
                lines.push(Vec::new());
                x = 0;
            }
            let line = lines.last_mut().unwrap();
            line.push(CellPlacement::new(binding.row, x, width));
            x += width;
            if let Some(remove) = binding.remove {
                line.push(CellPlacement::new(remove, x, tool_width));
                x += tool_width;
            }
            x += gap;
        }
        ActionLines { header, lines }
    }

    fn row_pages(&self, view: HWND, width: i32, height: i32) -> Option<Vec<Vec<Placement>>> {
        let p = |n| self.p(n);
        let columns = if width >= p(THREE_COLUMNS) {
            3
        } else if width >= p(780) {
            2
        } else {
            1
        };
        let margin = self.margin(width);
        let gap = p(20);
        let col = ((width - 2 * margin - (columns - 1) as i32 * gap) / columns as i32).max(1);
        let sections: [_; GROUPS.len()] = {
            let fonts = self.fonts.borrow();
            let fonts = fonts.as_ref()?;
            let dc = unsafe { GetDC(view) };
            let sections = std::array::from_fn(|i| {
                let group = &GROUPS[i];
                Section {
                    heading: child(view, SECTION + i as i32),
                    topic: group.topic.column(),
                    actions: group
                        .actions
                        .iter()
                        .map(|&a| self.action_lines(dc, fonts, a, col))
                        .collect(),
                }
            });
            unsafe { ReleaseDC(view, dc) };
            sections
        };
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
        if pages.len() > 1 {
            pages = grid.pages(upper, transfers, height - p(34) - p(4));
        }
        Some(pages)
    }

    fn layout_rows(
        &self,
        view: HWND,
        width: i32,
        height: i32,
        reveal: Option<HWND>,
        resizing: bool,
    ) {
        let editor = self.hwnd.get();
        if editor.is_null() || view.is_null() {
            return;
        }
        let Some(mut pages) = self.row_pages(view, width, height) else {
            return;
        };
        let p = |n| self.p(n);
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
        let old_page = self.page.replace(page);
        let old_count = self.pages.replace(pages.len());
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
            let y = height - p(34);
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
            if !resizing || page != old_page || page_count != old_count {
                caption(label, &page_text);
            }
        }
        if self.sizing.get() {
            self.visible_layout.replace(std::mem::take(items));
            return;
        }
        let previous = self.visible_layout.take();
        if resizing
            && old_count == page_count
            && old_page == page
            && position::resize_visible(view, &previous, items)
        {
            self.visible_layout.replace(std::mem::take(items));
            return;
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
                place(item.hwnd, item.x, item.y, item.width, item.height);
            }
            EnableWindow(prev, i32::from(page > 0));
            EnableWindow(next, i32::from(page + 1 < page_count));
            let mut hwnd = GetWindow(view, GW_CHILD);
            while !hwnd.is_null() {
                if !visible.contains(&hwnd) {
                    hide(hwnd);
                }
                hwnd = GetWindow(hwnd, GW_HWNDNEXT);
            }
            // Focus notifications can re-enter layout. Send them only after
            // applying this page and keep the input session on its own binding.
            let target = needs_focus
                .then(|| {
                    recording.or_else(|| {
                        items
                            .iter()
                            .find(|item| {
                                row_control(GetDlgCtrlID(item.hwnd))
                                    .is_some_and(|(kind, _)| kind == Control::Row)
                            })
                            .map(|item| item.hwnd)
                    })
                })
                .flatten();
            self.visible_layout.replace(std::mem::take(items));
            if let Some(target) = target {
                SetFocus(target);
            }
            // Both containers clip their children. Repaint the final layout as a
            // tree, including the background exposed by moved or hidden controls.
            RedrawWindow(
                editor,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
    }

    pub(super) fn reveal_row(&self, hwnd: HWND, row: HWND) {
        if row.is_null() || unsafe { GetWindowLongW(row, GWL_STYLE) as u32 } & WS_VISIBLE != 0 {
            return;
        }
        let view = child(hwnd, VIEW);
        let mut viewport = RECT::default();
        unsafe { GetClientRect(view, &mut viewport) };
        self.layout_rows(view, viewport.right, viewport.bottom, Some(row), false);
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
            let viewport =
                if self.sizing.get() && unsafe { IsWindowVisible(child(hwnd, RESIZE_VIEW)) } != 0 {
                    RESIZE_VIEW
                } else {
                    VIEW
                };
            if unsafe { GetWindowRect(child(hwnd, viewport), &mut view) } != 0 {
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

// The final RedrawWindow also covers the header. Do not copy pixels from the
// previous layout or paint intermediate positions during a column change.
const LAYOUT_FLAGS: SET_WINDOW_POS_FLAGS =
    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOREDRAW | SWP_NOCOPYBITS;

fn place(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
    unsafe {
        SetWindowPos(
            hwnd,
            null_mut(),
            x,
            y,
            width.max(1),
            height.max(1),
            LAYOUT_FLAGS | SWP_SHOWWINDOW,
        )
    };
}

fn hide(hwnd: HWND) {
    unsafe {
        if GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0 {
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                LAYOUT_FLAGS | SWP_NOMOVE | SWP_NOSIZE | SWP_HIDEWINDOW,
            );
        }
    }
}

unsafe extern "system" fn resize_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let ui = &*(data as *const ShortcutWindow);
        match msg {
            WM_ERASEBKGND => 1,
            WM_PAINT | WM_PRINTCLIENT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = if msg == WM_PAINT {
                    BeginPaint(hwnd, &mut ps)
                } else {
                    wp as HDC
                };
                let mut bounds = RECT::default();
                GetClientRect(hwnd, &mut bounds);
                canvas::paint(ui, dc, &ui.visible_layout.borrow(), &bounds);
                if msg == WM_PAINT {
                    EndPaint(hwnd, &ps);
                }
                0
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(resize_proc), id);
                DefSubclassProc(hwnd, msg, wp, lp)
            }
            _ => DefSubclassProc(hwnd, msg, wp, lp),
        }
    }
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
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                paint::background(dc, &paint.rcPaint, ui.colors.get());
                EndPaint(hwnd, &paint);
                0
            }
            // The opaque WM_PAINT above covers the entire update region.
            WM_ERASEBKGND => 1,
            WM_PRINTCLIENT => {
                ui.background(hwnd, wp as HDC);
                0
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
