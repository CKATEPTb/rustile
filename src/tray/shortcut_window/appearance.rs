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
        let control = row_control(id);
        let slot = unsafe { GetWindowLongPtrW(item.hwndItem, GWLP_USERDATA) } as usize;
        let action = control.and_then(|(kind, a)| (kind == Control::Row).then_some(a));
        let selected = action == Some(self.selected.get()) && slot == self.selected_slot.get();
        let icon = control.and_then(|(kind, _)| match kind {
            Control::Edit => Some("\u{e70f}"),
            Control::Remove => Some("\u{e711}"),
            Control::Add => Some("\u{e710}"),
            Control::Reset => Some("\u{e777}"),
            Control::Row => None,
        });
        let recording = selected && self.capture.borrow().is_some();
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
        if icon.is_some() {
            let mid = (r.top + r.bottom) / 2;
            panel.top = mid - self.p(13);
            panel.bottom = mid + self.p(13);
        }
        if focused || recording {
            let edge = if recording {
                row_colors.accent
            } else {
                row_colors.text
            };
            fill_outlined(dc, panel, surface, edge, self.p(5), self.p(1));
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
        } else if let Some(glyph) = icon {
            paint::text(
                dc,
                r,
                glyph,
                fonts.symbol,
                if disabled {
                    colors.disabled
                } else if control.is_some_and(|c| c.0 == Control::Remove)
                    && !(hovered || pressed || focused || colors.high_contrast)
                {
                    blend(colors.secondary, colors.background, 28)
                } else {
                    row_colors.secondary
                },
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

    pub(super) fn action_row_height(&self, dc: HDC, action: Shortcut, row_width: i32) -> i32 {
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else {
            return self.p(32);
        };
        let binding = self.settings.borrow().bindings(action).first().copied();
        let width = if let Some(chord) = binding {
            keycaps::Keycaps::new(
                self,
                dc,
                fonts,
                (chord.mods, Some(chord.key)),
                keycaps::Size::Row,
            )
            .width()
        } else {
            text_width(dc, fonts.small, "Не назначено")
        };
        let label = text_width(dc, fonts.regular, action.label());
        if label + self.p(11) + self.p(8) + width + self.p(5) > row_width {
            self.p(48)
        } else {
            self.p(32)
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
        let slot = unsafe { GetWindowLongPtrW(item.hwndItem, GWLP_USERDATA) } as usize;
        let mut label = item.rcItem;
        label.left += self.p(11);
        label.right -= self.p(5);
        let mut value = label;
        let caps = keycaps::Keycaps::new(
            self,
            dc,
            fonts,
            self.editor_chord(action, slot),
            keycaps::Size::Row,
        );
        let placeholder = if recording {
            "Нажмите клавиши"
        } else {
            "Не назначено"
        };
        let width = caps.width();
        let visible_width = if width == 0 {
            text_width(dc, fonts.small, placeholder)
        } else {
            width
        };
        if slot > 0 {
            value.left = (value.right - visible_width).max(value.left + self.p(22));
            label.right = value.left - self.p(5);
        } else if label.bottom - label.top >= self.p(44) {
            label.bottom = label.top + self.p(22);
            value.top = label.bottom;
            value.bottom -= self.p(2);
        } else {
            value.left = (value.right - visible_width).max(value.left);
            label.right = (value.left - self.p(8)).max(label.left);
        }
        paint::text(
            dc,
            label,
            if slot == 0 { action.label() } else { "или" },
            if slot == 0 {
                fonts.regular
            } else {
                fonts.small
            },
            if slot == 0 {
                colors.text
            } else {
                colors.secondary
            },
            if slot == 0 {
                DT_LEFT | DT_END_ELLIPSIS
            } else {
                DT_RIGHT
            },
        );
        if width == 0 {
            paint::text(
                dc,
                value,
                placeholder,
                fonts.small,
                if recording {
                    colors.accent
                } else {
                    colors.secondary
                },
                DT_LEFT | DT_END_ELLIPSIS,
            );
        } else {
            caps.draw(dc, value, self.colors.get());
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
