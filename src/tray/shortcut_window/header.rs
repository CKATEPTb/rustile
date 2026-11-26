use super::appearance::{blend, fill_outlined, inset, text_width};
use super::*;

impl ShortcutWindow {
    pub(super) fn header_color(&self) -> COLORREF {
        let colors = self.colors.get();
        if colors.high_contrast {
            colors.background
        } else {
            blend(
                colors.background,
                colors.accent,
                if colors.dark { 5 } else { 4 },
            )
        }
    }

    pub(super) fn draw_preview(&self, item: &DRAWITEMSTRUCT) {
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let colors = self.colors.get();
        let action = self.selected.get();
        let recording = self.recording(action);
        let chord = self.editor_chord(action, self.selected_slot.get());
        let category = if recording {
            tr(Msg::UiRecording)
        } else {
            view::group_title(action)
        };
        let p = |n| self.p(n);
        buffered(item, |dc, r| {
            paint::fill(dc, &r, self.header_color(), 0);
            let width = r.right - r.left;
            let compact = width < p(460);
            let title_font = if compact {
                fonts.heading
            } else {
                fonts.display
            };
            let key_size = if compact {
                keycaps::Size::Compact
            } else {
                keycaps::Size::Preview
            };
            let caps = keycaps::Keycaps::new(self, dc, fonts, chord, key_size);
            let legend_width = caps.width();
            let title_width = text_width(dc, title_font, action.label()).max(text_width(
                dc,
                fonts.small,
                category,
            ));
            let show_keys = legend_width > 0 && title_width + legend_width + p(18) <= width;
            let label_right = if show_keys {
                r.right - legend_width - p(18)
            } else {
                r.right
            };
            let mid = (r.bottom + r.top) / 2;
            let category_rect = RECT {
                left: r.left,
                right: label_right,
                top: mid - p(22),
                bottom: mid - p(3),
            };
            let secondary = if recording && !colors.high_contrast {
                colors.accent
            } else {
                colors.secondary
            };
            paint::text(
                dc,
                category_rect,
                category,
                fonts.small,
                secondary,
                DT_END_ELLIPSIS,
            );
            let title_rect = RECT {
                top: category_rect.bottom,
                bottom: mid + p(24),
                ..category_rect
            };
            paint::text(
                dc,
                title_rect,
                action.label(),
                title_font,
                colors.text,
                DT_END_ELLIPSIS,
            );
            if show_keys {
                let legend = RECT {
                    left: r.right - legend_width,
                    ..r
                };
                caps.draw(dc, legend, colors);
            }
        });
    }

    pub(super) fn draw_reset_all(&self, item: &DRAWITEMSTRUCT) {
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let colors = self.colors.get();
        let disabled = item.itemState & ODS_DISABLED != 0;
        let pressed = item.itemState & ODS_SELECTED != 0;
        let focused = item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
        let mut cursor = POINT::default();
        let hovered = !disabled
            && unsafe {
                GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == item.hwndItem
            };
        let face = if disabled {
            self.header_color()
        } else if pressed && !colors.high_contrast {
            blend(colors.hover, colors.text, 6)
        } else if hovered || pressed {
            colors.hover
        } else {
            colors.control
        };
        let ink = if disabled {
            colors.disabled
        } else if hovered || pressed {
            colors.hover_text
        } else {
            colors.text
        };
        buffered(item, |dc, r| {
            paint::fill(dc, &r, self.header_color(), 0);
            let border = if focused { colors.text } else { colors.border };
            fill_outlined(
                dc,
                inset(r, self.p(1)),
                face,
                border,
                self.p(6),
                self.p(if focused { 2 } else { 1 }),
            );
            let label = tr(Msg::UiResetAll);
            let gap = self.p(7);
            let icon_width = self.p(17);
            let label_width = text_width(dc, fonts.regular, label);
            let total = icon_width + gap + label_width;
            if total + self.p(10) <= r.right - r.left {
                let left = (r.left + r.right - total) / 2;
                let icon = RECT {
                    left,
                    right: left + icon_width,
                    ..r
                };
                paint::text(dc, icon, "\u{e777}", fonts.symbol, ink, DT_CENTER);
                paint::text(
                    dc,
                    RECT {
                        left: icon.right + gap,
                        ..r
                    },
                    label,
                    fonts.regular,
                    ink,
                    DT_LEFT,
                );
            } else {
                paint::text(dc, r, "\u{e777}", fonts.symbol, ink, DT_CENTER);
            }
        });
    }
}

fn buffered(item: &DRAWITEMSTRUCT, paint: impl FnOnce(HDC, RECT)) {
    let r = item.rcItem;
    let (width, height) = (r.right - r.left, r.bottom - r.top);
    if width <= 0 || height <= 0 {
        return;
    }
    unsafe {
        let mut dc = null_mut();
        let buffer = BeginBufferedPaint(item.hDC, &r, BPBF_COMPATIBLEBITMAP, null(), &mut dc);
        if buffer == 0 {
            let saved = SaveDC(item.hDC);
            SetBkMode(item.hDC, TRANSPARENT as i32);
            paint(item.hDC, r);
            RestoreDC(item.hDC, saved);
        } else {
            SetBkMode(dc, TRANSPARENT as i32);
            paint(dc, r);
            EndBufferedPaint(buffer, 1);
        }
    }
}
