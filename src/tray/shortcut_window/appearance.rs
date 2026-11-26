use super::*;

impl ShortcutWindow {
    pub(super) fn draw(&self, item: &DRAWITEMSTRUCT) {
        let mut cursor = POINT::default();
        let hovered = item.CtlType == ODT_BUTTON
            && item.itemState & ODS_DISABLED == 0
            && unsafe {
                GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == item.hwndItem
            };
        self.draw_cached(item, hovered);
    }

    pub(super) fn draw_cached(&self, item: &DRAWITEMSTRUCT, hovered: bool) {
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let id = item.CtlID as i32;
        let control = row_control(id);
        let mut prefix = *item;
        let mut cache = self.raster.borrow_mut();
        if (SECTION..SECTION + view::GROUPS.len() as i32).contains(&id) {
            let section = (id - SECTION) as usize;
            let title_width = *cache.section_widths[section].get_or_insert_with(|| {
                self.p(20)
                    + self.p(7)
                    + text_width(item.hDC, fonts.heading, tr(view::GROUPS[section].title))
                    + self.p(12)
            });
            // Only the flat divider to the right of the title changes width
            // during most drags. Keep the icon and title reusable at full size.
            let r = item.rcItem;
            let divider = r.left + title_width;
            if divider < r.right {
                prefix.rcItem.right = divider;
                let tail = RECT { left: divider, ..r };
                let colors = self.colors.get();
                paint::background(item.hDC, &tail, colors);
                let y = (r.top + r.bottom) / 2;
                paint::fill(
                    item.hDC,
                    &RECT {
                        top: y,
                        bottom: y + self.p(1),
                        ..tail
                    },
                    colors.border,
                    0,
                );
            }
        }

        // Identical icon buttons share one bitmap per visual state, even when
        // an action is edited or rows move to a different page.
        let content = match control {
            Some((Control::Row, action)) => raster::Content::Binding {
                hwnd: item.hwndItem,
                action: action.index(),
            },
            Some((Control::Add, _)) => raster::Content::Control(ADD_ROW),
            Some((Control::Reset, _)) => raster::Content::Control(RESET_ROW),
            Some((Control::Remove, _)) => raster::Content::Control(REMOVE_ROW),
            None if id == PAGE_LABEL => {
                raster::Content::PageNumber(self.page.get(), self.pages.get())
            }
            None => raster::Content::Control(id),
        };
        let selected =
            matches!(control, Some((Control::Row, _))) && item.hwndItem == self.selected_row();
        let mut state = item.itemState & (ODS_SELECTED | ODS_DISABLED | ODS_FOCUS);
        if item.itemState & ODS_NOFOCUSRECT != 0 {
            state &= !ODS_FOCUS;
        }
        let key = raster::Key {
            content,
            state,
            hovered: hovered && item.itemState & ODS_DISABLED == 0,
            selected,
            recording: selected && self.capture.borrow().is_some(),
        };
        let draw = |item: &DRAWITEMSTRUCT| self.paint_control(item, fonts, key);
        if !cache.blit(&prefix, key, draw) {
            draw(&prefix);
        }
    }

    fn paint_control(&self, item: &DRAWITEMSTRUCT, fonts: &Fonts, key: raster::Key) {
        let colors = self.colors.get();
        let dc = item.hDC;
        let r = item.rcItem;
        let id = item.CtlID as i32;
        paint::background(dc, &r, colors);
        unsafe { SetBkMode(dc, TRANSPARENT as i32) };

        if (SECTION..SECTION + view::GROUPS.len() as i32).contains(&id) {
            self.draw_section(item, fonts, (id - SECTION) as usize);
            return;
        }

        if let Some(action) = id
            .checked_sub(LABEL_ROW)
            .and_then(|index| Shortcut::ALL.get(index as usize))
        {
            paint::text(
                dc,
                r,
                action.label(),
                fonts.regular,
                colors.text,
                DT_LEFT | DT_END_ELLIPSIS,
            );
            return;
        }
        if id == PAGE_LABEL {
            let label = format!("{} / {}", self.page.get() + 1, self.pages.get());
            paint::text(dc, r, &label, fonts.small, colors.secondary, DT_CENTER);
            return;
        }

        let disabled = item.itemState & ODS_DISABLED != 0;
        let pressed = item.itemState & ODS_SELECTED != 0;
        let focused = item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
        let hovered = key.hovered;
        let control = row_control(id);
        let binding = control.and_then(|(kind, action)| (kind == Control::Row).then_some(action));
        let selected = key.selected;
        let recording = key.recording;
        let active = recording || selected || hovered || pressed;
        let mut ink = colors;
        let highlighted = colors.high_contrast && active;
        let face = if highlighted {
            ink.text = colors.hover_text;
            ink.secondary = colors.hover_text;
            ink.accent = colors.hover_text;
            colors.hover
        } else if pressed {
            blend(colors.hover, colors.text, 6)
        } else if selected {
            blend(
                colors.control,
                colors.accent,
                if recording { 14 } else { 7 },
            )
        } else if hovered {
            colors.hover
        } else if binding.is_some() {
            colors.control
        } else {
            colors.background
        };
        let edge = if focused || recording {
            if colors.high_contrast {
                colors.border
            } else {
                colors.accent
            }
        } else if selected && !colors.high_contrast {
            blend(colors.border, colors.accent, 55)
        } else {
            colors.border
        };
        let mut panel = inset(r, self.p(1));
        if binding.is_some() {
            let height = (panel.bottom - panel.top).min(self.p(22));
            panel.top = (r.top + r.bottom - height) / 2;
            panel.bottom = panel.top + height;
        }
        if focused || binding.is_some() {
            fill_outlined(dc, panel, face, edge, self.p(6), self.p(1));
        } else if hovered || pressed {
            paint::fill(dc, &panel, face, self.p(5));
        }

        if let Some(action) = binding {
            let value = RECT {
                left: panel.left + self.p(2),
                right: panel.right - self.p(2),
                ..panel
            };
            let slot = unsafe { GetWindowLongPtrW(item.hwndItem, GWLP_USERDATA) } as usize;
            let row = self.measured_row(dc, fonts, action);
            let Some(binding) = row.bindings.get(slot) else {
                return;
            };
            let caps = &binding.caps;
            if caps.width() == 0 {
                paint::text(
                    dc,
                    value,
                    placeholder(recording),
                    fonts.small,
                    if recording { ink.accent } else { ink.secondary },
                    DT_CENTER | DT_END_ELLIPSIS,
                );
            } else {
                caps.draw(dc, value, ink);
            }
            return;
        }
        let glyph = match (control.map(|(kind, _)| kind), id) {
            (Some(Control::Remove), _) => "\u{e74d}",
            (Some(Control::Add), _) => "\u{e710}",
            (Some(Control::Reset), _) => "\u{e777}",
            (_, PAGE_PREV) => "\u{e76b}",
            (_, PAGE_NEXT) => "\u{e76c}",
            _ => return,
        };
        paint::text(
            dc,
            r,
            glyph,
            fonts.symbol,
            if disabled {
                colors.disabled
            } else {
                ink.secondary
            },
            DT_CENTER,
        );
    }

    fn draw_section(&self, item: &DRAWITEMSTRUCT, fonts: &Fonts, section: usize) {
        let dc = item.hDC;
        let r = item.rcItem;
        let colors = self.colors.get();
        let group = &view::GROUPS[section];
        let title = tr(group.title);
        let glyph = RECT {
            right: r.left + self.p(20),
            ..r
        };
        paint::text(
            dc,
            glyph,
            group.icon,
            fonts.symbol,
            colors.secondary,
            DT_CENTER,
        );
        let label = RECT {
            left: glyph.right + self.p(7),
            ..r
        };
        paint::text(
            dc,
            label,
            title,
            fonts.heading,
            colors.text,
            DT_END_ELLIPSIS,
        );
    }
}

pub(super) fn placeholder(recording: bool) -> &'static str {
    if recording {
        tr(Msg::UiPressKey)
    } else {
        tr(Msg::UiUnassigned)
    }
}

pub(super) fn text_width(dc: HDC, font: HFONT, text: &str) -> i32 {
    let utf16 = wide(text);
    let mut size = SIZE::default();
    unsafe {
        let old = SelectObject(dc, font);
        GetTextExtentPoint32W(dc, utf16.as_ptr(), (utf16.len() - 1) as i32, &mut size);
        SelectObject(dc, old);
    }
    size.cx
}

pub(super) fn inset(r: RECT, n: i32) -> RECT {
    RECT {
        left: r.left + n,
        top: r.top + n,
        right: r.right - n,
        bottom: r.bottom - n,
    }
}

pub(super) fn fill_outlined(
    dc: HDC,
    r: RECT,
    fill: COLORREF,
    edge: COLORREF,
    radius: i32,
    stroke: i32,
) {
    paint::fill(dc, &r, edge, radius);
    paint::fill(dc, &inset(r, stroke), fill, (radius - stroke).max(0));
}

pub(super) fn blend(base: COLORREF, tint: COLORREF, percent: u32) -> COLORREF {
    let channel = |shift: u32| {
        let a = (base >> shift) & 255;
        let b = (tint >> shift) & 255;
        ((a * (100 - percent) + b * percent + 50) / 100) << shift
    };
    channel(0) | channel(8) | channel(16)
}
