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
        let capture = self.capture.borrow().as_ref().map(|c| c.keys.preview());
        let (mods, key) = capture
            .or_else(|| {
                self.settings
                    .get()
                    .binding(action)
                    .map(|c| (c.mods, Some(c.key)))
            })
            .unwrap_or_default();
        let key = key.map(Chord::key_label);
        let mut labels: Vec<&str> = Chord::modifier_labels(mods).collect();
        labels.extend(key.as_deref());
        let category = if capture.is_some() {
            "Запись сочетания"
        } else {
            view::GROUPS
                .iter()
                .find_map(|&(title, actions)| actions.contains(&action).then_some(title))
                .unwrap_or("")
        };
        let placeholder = if capture.is_some() {
            "Нажмите клавиши"
        } else {
            "Не назначено"
        };
        let p = |n| self.p(n);
        buffered(item, |dc, r| {
            paint::fill(dc, &r, self.header_color(), 0);
            let available = r.right - r.left;
            let width_of = |label: &str, font, compact| {
                let label_width = if label == "Win" {
                    p(15)
                } else {
                    text_width(dc, font, label)
                };
                if compact {
                    (label_width + p(12)).max(p(30))
                } else {
                    (label_width + p(26)).max(p(48))
                }
            };
            let needed = |widths: &[i32], gap| {
                widths.iter().sum::<i32>() + widths.len().saturating_sub(1) as i32 * gap
            };
            let mut widths: Vec<_> = labels
                .iter()
                .map(|label| width_of(label, fonts.heading, false))
                .collect();
            let legend_width = if widths.is_empty() {
                text_width(dc, fonts.regular, placeholder)
            } else {
                needed(&widths, p(6))
            };
            let title_width = text_width(dc, fonts.display, action.label()).max(text_width(
                dc,
                fonts.small,
                category,
            ));
            let stacked = available < p(422) || available < title_width + legend_width + p(24);
            let mid_y = (r.bottom + r.top) / 2;
            let title_right = if stacked {
                r.right
            } else {
                r.right - legend_width - p(18)
            };
            let group_top = if stacked { r.top + p(3) } else { mid_y - p(24) };
            let category_rect = RECT {
                left: r.left,
                top: group_top,
                right: title_right,
                bottom: group_top + p(19),
            };
            let category_ink = if capture.is_some() && !colors.high_contrast {
                colors.accent
            } else {
                colors.secondary
            };
            paint::text(
                dc,
                category_rect,
                category,
                fonts.small,
                category_ink,
                DT_END_ELLIPSIS,
            );
            let title_rect = RECT {
                top: category_rect.bottom,
                bottom: category_rect.bottom + p(29),
                ..category_rect
            };
            paint::text(
                dc,
                title_rect,
                action.label(),
                fonts.display,
                colors.text,
                DT_END_ELLIPSIS,
            );

            let top = if stacked {
                title_rect.bottom + p(4)
            } else {
                mid_y - p(22)
            };
            let height = if stacked {
                p(30).min(r.bottom - top)
            } else {
                p(44)
            };
            let left = if stacked {
                r.left
            } else {
                r.right - legend_width
            };
            let legend_rect = RECT {
                left,
                top,
                right: r.right,
                bottom: top + height,
            };
            if labels.is_empty() {
                paint::text(
                    dc,
                    legend_rect,
                    placeholder,
                    fonts.regular,
                    category_ink,
                    DT_END_ELLIPSIS,
                );
                return;
            }
            let (key_font, gap) = if stacked {
                for (label, width) in labels.iter().zip(&mut widths) {
                    *width = width_of(label, fonts.regular, true);
                }
                (fonts.regular, p(4))
            } else {
                (fonts.heading, p(6))
            };
            // A five-key chord with a localized media key can exceed a narrow window.
            // Keep every modifier visible and give the terminal key the remaining space.
            let overflow = (needed(&widths, gap) - available).max(0);
            if let Some(last) = widths.last_mut() {
                *last = (*last - overflow).max(p(14));
            }
            let mut x = left;
            for (i, (&label, width)) in labels.iter().zip(widths).enumerate() {
                let outer = RECT {
                    left: x,
                    right: (x + width).min(r.right),
                    ..legend_rect
                };
                if outer.right <= outer.left {
                    break;
                }
                let terminal = key.is_some() && i + 1 == labels.len();
                let (face, ink) = if terminal {
                    (colors.accent, colors.on_accent)
                } else {
                    (colors.control, colors.text)
                };
                let edge = if colors.high_contrast {
                    colors.border
                } else {
                    blend(face, colors.text, if terminal { 14 } else { 23 })
                };
                let foot = if colors.high_contrast {
                    edge
                } else {
                    blend(face, 0, if colors.dark { 45 } else { 22 })
                };
                let radius = if stacked { p(4) } else { p(6) };
                paint::fill(dc, &outer, foot, radius);
                let surface = RECT {
                    bottom: outer.bottom - if stacked { p(2) } else { p(3) },
                    ..outer
                };
                fill_outlined(dc, surface, face, edge, radius, p(1));
                if label == "Win" {
                    let side = if stacked { p(12) } else { p(15) };
                    windows_mark(dc, surface, side, p(1), ink);
                } else {
                    paint::text(
                        dc,
                        inset(surface, p(1)),
                        label,
                        key_font,
                        ink,
                        DT_CENTER | DT_END_ELLIPSIS,
                    );
                }
                x += width + gap;
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
            let label = "Сбросить всё";
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

fn windows_mark(dc: HDC, bounds: RECT, side: i32, gutter: i32, ink: COLORREF) {
    let pane = ((side - gutter) / 2).max(1);
    let span = 2 * pane + gutter;
    let x = (bounds.left + bounds.right - span) / 2;
    let y = (bounds.top + bounds.bottom - span) / 2;
    for left in [x, x + pane + gutter] {
        for top in [y, y + pane + gutter] {
            paint::fill(
                dc,
                &RECT {
                    left,
                    top,
                    right: left + pane,
                    bottom: top + pane,
                },
                ink,
                0,
            );
        }
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
