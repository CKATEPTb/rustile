use super::*;

// Move, Location, FullScreen, LightningBolt, ViewAll, Switch, TaskView, List.
const SECTION_ICONS: [&str; 8] = [
    "\u{e7c2}", "\u{e81d}", "\u{e740}", "\u{e945}", "\u{e8a9}", "\u{e8ab}", "\u{e7c4}", "\u{ea37}",
];

impl ShortcutWindow {
    pub(super) fn draw(&self, item: &DRAWITEMSTRUCT) {
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
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

        let disabled = item.itemState & ODS_DISABLED != 0;
        let pressed = item.itemState & ODS_SELECTED != 0;
        let focused = item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
        let mut cursor = POINT::default();
        let hovered = !disabled
            && unsafe {
                GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == item.hwndItem
            };
        let action = row_action(id);
        let selected = action == Some(self.selected.get());
        let reset =
            (reset_action(id).is_some() || scheme::reset_from_id(id).is_some()) && !disabled;
        let recording = action.is_some_and(|a| self.recording(a));
        let highlighted = colors.high_contrast && (selected || hovered || pressed);
        let surface = if highlighted {
            colors.hover
        } else if pressed {
            blend(colors.hover, colors.text, 7)
        } else if recording {
            blend(colors.background, colors.accent, 17)
        } else if selected {
            blend(colors.background, colors.accent, 10)
        } else if hovered {
            colors.hover
        } else if reset {
            colors.control
        } else {
            colors.background
        };
        let mut row_colors = colors;
        if highlighted {
            row_colors.text = colors.hover_text;
            row_colors.secondary = colors.hover_text;
            row_colors.accent = colors.hover_text;
        }

        let mut panel = inset(r, self.p(1));
        if reset {
            let mid = (r.top + r.bottom) / 2;
            panel.top = mid - self.p(11);
            panel.bottom = mid + self.p(11);
        }
        if focused || recording {
            let edge = if recording {
                row_colors.accent
            } else {
                row_colors.text
            };
            fill_outlined(dc, panel, surface, edge, self.p(5), self.p(1));
        } else if reset {
            fill_outlined(dc, panel, surface, colors.border, self.p(5), self.p(1));
        } else if selected || hovered || pressed {
            paint::fill(dc, &panel, surface, self.p(5));
        }

        if let Some(action) = action {
            if selected {
                let mid = (r.top + r.bottom) / 2;
                let rail = RECT {
                    left: r.left + self.p(3),
                    right: r.left + self.p(5),
                    top: mid - self.p(7),
                    bottom: mid + self.p(7),
                };
                paint::fill(dc, &rail, row_colors.accent, self.p(1));
            }
            self.draw_action(item, action, recording, fonts, row_colors);
        } else if reset {
            paint::text(
                dc,
                r,
                "\u{e777}",
                fonts.symbol,
                row_colors.secondary,
                DT_CENTER,
            );
        }
    }

    fn draw_section(&self, item: &DRAWITEMSTRUCT, fonts: &Fonts, section: usize) {
        let dc = item.hDC;
        let r = item.rcItem;
        let colors = self.colors.get();
        let title = view::GROUPS[section].0;
        let glyph = RECT {
            right: r.left + self.p(20),
            ..r
        };
        paint::text(
            dc,
            glyph,
            SECTION_ICONS[section],
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
        let left = label.left + text_width(dc, fonts.heading, title) + self.p(12);
        if left < r.right {
            let y = (r.top + r.bottom) / 2;
            paint::fill(
                dc,
                &RECT {
                    left,
                    top: y,
                    right: r.right,
                    bottom: y + self.p(1),
                },
                colors.border,
                0,
            );
        }
    }

    fn draw_action(
        &self,
        item: &DRAWITEMSTRUCT,
        action: Shortcut,
        recording: bool,
        fonts: &Fonts,
        colors: Colors,
    ) {
        let dc = item.hDC;
        let mut label = item.rcItem;
        label.left += self.p(11);
        label.right -= self.p(5);
        let bounds = label;
        let stacked = bounds.bottom - bounds.top >= self.p(44);
        let tight = bounds.right - bounds.left < self.p(325);
        let chord = self.settings.get().binding(action).filter(|_| !recording);
        let legend = chord.map_or_else(|| self.binding_text(action), |c| Chord::key_label(c.key));
        let mods = chord
            .map(|c| {
                Chord::modifier_labels(c.mods)
                    .collect::<Vec<_>>()
                    .join(if tight { "+" } else { " + " })
            })
            .unwrap_or_default();
        let gap = if mods.is_empty() {
            0
        } else {
            self.p(if tight { 4 } else { 6 })
        };
        let legend_width = text_width(dc, fonts.small, &legend);
        let cap_width = if chord.is_some() {
            let padding = self.p(if tight { 10 } else { 12 });
            (legend_width + padding).max(self.p(if tight { 21 } else { 23 }))
        } else {
            legend_width
        };
        let mods_width = text_width(dc, fonts.small, &mods);
        let value_width = mods_width + gap + cap_width;
        let ink = if recording {
            colors.accent
        } else {
            colors.secondary
        };
        let gutter = self.p(if tight { 6 } else { 8 });
        let label_width = text_width(dc, fonts.regular, action.label());

        // A custom media key with all four modifiers can fill most of a column.
        // Give it a second baseline without moving the neighbouring shortcuts.
        if !stacked && label_width + gutter + value_width > bounds.right - bounds.left {
            let mid = (bounds.top + bounds.bottom) / 2;
            // Segoe UI's line box includes leading above and below the glyphs.
            // Let the two lines share that leading at fractional scale factors.
            let leading = self.p(2);
            label.bottom = mid + leading;
            paint::text(
                dc,
                label,
                action.label(),
                fonts.small,
                colors.text,
                DT_LEFT | DT_END_ELLIPSIS,
            );
            let binding = self.binding_text(action);
            paint::text(
                dc,
                RECT {
                    top: mid - leading,
                    ..bounds
                },
                &binding,
                fonts.small,
                ink,
                DT_RIGHT | DT_END_ELLIPSIS,
            );
            return;
        }

        let mut value = bounds;
        if stacked {
            value.top = (bounds.top + bounds.bottom) / 2 - self.p(1);
            label.bottom = value.top;
            value.right = (value.left + value_width).min(value.right);
        } else {
            value.left = value.right - value_width;
            label.right = value.left - gutter;
        }
        paint::text(
            dc,
            label,
            action.label(),
            fonts.regular,
            colors.text,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        if chord.is_none() {
            paint::text(
                dc,
                value,
                &legend,
                fonts.small,
                ink,
                DT_LEFT | DT_END_ELLIPSIS,
            );
            return;
        }
        let mid = (value.top + value.bottom) / 2;
        let cap = RECT {
            left: (value.right - cap_width).max(value.left),
            right: value.right,
            top: mid - self.p(10),
            bottom: mid + self.p(10),
        };
        let palette = self.colors.get();
        fill_outlined(
            dc,
            cap,
            palette.control,
            palette.border,
            self.p(4),
            self.p(1),
        );
        paint::text(
            dc,
            inset(cap, self.p(1)),
            &legend,
            fonts.small,
            palette.text,
            DT_CENTER | DT_END_ELLIPSIS,
        );
        if !mods.is_empty() {
            value.right = cap.left - gap;
            paint::text(
                dc,
                value,
                &mods,
                fonts.small,
                ink,
                DT_RIGHT | DT_END_ELLIPSIS,
            );
        }
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
