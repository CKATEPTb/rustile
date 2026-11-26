use super::appearance::{blend, fill_outlined, inset, text_width};
use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Size {
    Row,
    Compact,
    Preview,
}

pub(super) struct Keycaps {
    keys: Vec<(String, i32)>,
    has_terminal: bool,
    size: Size,
    font: HFONT,
    dpi: u32,
    gap: i32,
    padding: i32,
    min_width: i32,
}

impl Keycaps {
    pub(super) fn new(
        ui: &ShortcutWindow,
        dc: HDC,
        fonts: &Fonts,
        (mods, key): (u32, Option<u16>),
        size: Size,
    ) -> Self {
        let (font, gap, padding, min_width) = match size {
            Size::Row => (fonts.small, 1, 9, 17),
            Size::Compact => (fonts.regular, 4, 15, 30),
            Size::Preview => (fonts.heading, 6, 24, 44),
        };
        let keys = Chord::key_labels(mods, key)
            .into_iter()
            .map(|label| {
                let width = text_width(dc, font, &label);
                (label, width)
            })
            .collect();
        Self {
            keys,
            has_terminal: key.is_some(),
            size,
            font,
            dpi: ui.dpi.get(),
            gap: ui.p(gap),
            padding: ui.p(padding),
            min_width: ui.p(min_width),
        }
    }

    fn key_width(&self, text: i32) -> i32 {
        (text + self.padding).max(self.min_width)
    }

    pub(super) fn width(&self) -> i32 {
        let gaps = self.keys.len().saturating_sub(1) as i32;
        self.keys
            .iter()
            .map(|(_, w)| self.key_width(*w))
            .sum::<i32>()
            + gaps * self.gap
    }

    pub(super) fn draw(&self, dc: HDC, bounds: RECT, colors: Colors) {
        let mut available = bounds.right - bounds.left;
        if self.keys.is_empty() || available <= 0 || bounds.bottom <= bounds.top {
            return;
        }
        let mut origin = bounds.left;
        if self.size == Size::Row && available > self.width() {
            origin += (available - self.width()) / 2;
            available = self.width();
        }
        let p = |dip| scale(dip, self.dpi);
        let (height, radius, foot) = match self.size {
            Size::Row => (21, 5, 1),
            Size::Compact => (29, 6, 2),
            Size::Preview => (40, 7, 2),
        };
        let height = p(height).min(bounds.bottom - bounds.top);
        let top = (bounds.top + bounds.bottom - height) / 2;
        let radius = p(radius);
        let stroke = p(1);
        let count = self.keys.len() as i32;
        let gap = self.gap.min(available / (2 * count));
        let width = available - (count - 1) * gap;
        // Shrink the padding before truncating a key name. In an exceptionally
        // narrow window distribute the remaining space across every key.
        let minimum = |w| w + 2 * stroke;
        let minimum_total = self.keys.iter().map(|(_, w)| minimum(*w)).sum::<i32>();
        let padding_total = self.width() - (count - 1) * self.gap - minimum_total;
        let text_budget = width.min(minimum_total);
        let padding_budget = (width - text_budget).min(padding_total);
        let mut text_used = 0;
        let mut padding_used = 0;
        let mut used = 0;
        for (i, (label, text)) in self.keys.iter().enumerate() {
            text_used += minimum(*text);
            padding_used += self.key_width(*text) - minimum(*text);
            let end = text_used * text_budget / minimum_total.max(1)
                + padding_used * padding_budget / padding_total.max(1);
            let left = origin + used + i as i32 * gap;
            let outer = RECT {
                left,
                top,
                right: left + end - used,
                bottom: top + height,
            };
            used = end;
            if outer.right <= outer.left + 2 * stroke {
                continue;
            }
            let terminal = self.has_terminal && i + 1 == self.keys.len();
            if self.size == Size::Row {
                if i > 0 {
                    let mid = (outer.top + outer.bottom) / 2;
                    let divider = RECT {
                        left: outer.left - gap,
                        right: outer.left - gap + stroke,
                        top: mid - p(5),
                        bottom: mid + p(5),
                    };
                    paint::fill(dc, &divider, colors.border, 0);
                }
                paint::text(
                    dc,
                    outer,
                    label,
                    self.font,
                    if terminal {
                        colors.text
                    } else {
                        colors.secondary
                    },
                    DT_CENTER | DT_END_ELLIPSIS,
                );
                continue;
            }
            let (face, ink) = if !terminal {
                (colors.control, colors.text)
            } else {
                (colors.accent, colors.on_accent)
            };
            let edge = if colors.high_contrast {
                colors.border
            } else {
                blend(face, colors.text, if colors.dark { 23 } else { 18 })
            };
            let lower_edge = if colors.high_contrast {
                edge
            } else {
                blend(edge, 0, if colors.dark { 28 } else { 9 })
            };
            paint::fill(dc, &outer, lower_edge, radius);
            let surface = RECT {
                bottom: outer.bottom - p(foot),
                ..outer
            };
            fill_outlined(dc, surface, face, edge, radius, stroke);
            paint::text(
                dc,
                inset(surface, stroke),
                label,
                self.font,
                ink,
                DT_CENTER | DT_END_ELLIPSIS,
            );
        }
    }
}

impl ShortcutWindow {
    pub(super) fn editor_chord(&self, action: Shortcut, slot: usize) -> (u32, Option<u16>) {
        if self.selected.get() == action
            && self.selected_slot.get() == slot
            && let Some(capture) = self.capture.borrow().as_ref()
        {
            return capture.keys.preview();
        }
        self.settings
            .borrow()
            .bindings(action)
            .get(slot)
            .map(|chord| (chord.mods, Some(chord.key)))
            .unwrap_or_default()
    }
}

pub(super) struct BindingMetrics {
    pub(super) row: HWND,
    pub(super) remove: Option<HWND>,
    pub(super) caps: Keycaps,
    pub(super) width: i32,
}

pub(super) struct RowMetrics {
    pub(super) label: HWND,
    pub(super) add: HWND,
    pub(super) reset: HWND,
    pub(super) label_width: i32,
    pub(super) bindings: Vec<BindingMetrics>,
}

impl ShortcutWindow {
    /// Intrinsic sizes are common to all client widths and both layout and paint.
    /// A row update or a font change discards the corresponding measurements.
    pub(super) fn measured_row(
        &self,
        dc: HDC,
        fonts: &Fonts,
        action: Shortcut,
    ) -> std::cell::Ref<'_, RowMetrics> {
        let index = action.index();
        if self.row_metrics.borrow()[index].is_none() {
            let assigned = self.settings.borrow().bindings(action);
            let capture = self.capture.borrow();
            let bindings = self.slots.borrow()[index]
                .iter()
                .enumerate()
                .map(|(i, slot)| {
                    let recording = capture
                        .as_ref()
                        .filter(|_| self.selected.get() == action && self.selected_slot.get() == i);
                    let chord = match recording {
                        Some(rec) => rec.keys.preview(),
                        None => assigned
                            .get(i)
                            .map(|c| (c.mods, Some(c.key)))
                            .unwrap_or_default(),
                    };
                    let caps = Keycaps::new(self, dc, fonts, chord, Size::Row);
                    let mut width = caps.width();
                    if recording.is_some() || width == 0 {
                        let prompt = appearance::placeholder(recording.is_some());
                        width = width.max(text_width(dc, fonts.small, prompt) + self.p(8));
                    }
                    BindingMetrics {
                        row: slot.row,
                        remove: recording.and_then(|rec| rec.original).map(|_| slot.remove),
                        caps,
                        width: width + self.p(6),
                    }
                })
                .collect();
            let view = child(self.hwnd.get(), VIEW);
            let id = index as i32;
            self.row_metrics.borrow_mut()[index] = Some(RowMetrics {
                label: child(view, LABEL_ROW + id),
                add: child(view, ADD_ROW + id),
                reset: child(view, RESET_ROW + id),
                label_width: text_width(dc, fonts.regular, action.label()) + self.p(3),
                bindings,
            });
        }
        std::cell::Ref::map(self.row_metrics.borrow(), |rows| {
            rows[index].as_ref().unwrap()
        })
    }
}
