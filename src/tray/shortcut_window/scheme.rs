use super::appearance::{blend, fill_outlined, inset, text_width};
use super::view::place;
use super::*;
use crate::settings::SchemeKey;

pub(super) const PAGE_SWITCH: i32 = 512;
const ROLE_ROW: i32 = 1200;
const RESET_ROLE: i32 = 1210;
pub(super) const HEADING: i32 = 1230;
pub(super) const EXAMPLE: i32 = 1240;
pub(super) const GROUP_LABELS: [&str; 3] = ["Модификаторы", "Направления", "Сочетания вправо"];
pub(super) const EXAMPLES: [&str; 5] = [
    "Перевести фокус",
    "Переместить окно",
    "Переключить рабочий стол",
    "Перенести окно на другой стол",
    "Изменить размер",
];

pub(super) fn role_id(role: SchemeKey) -> i32 {
    ROLE_ROW + role as i32
}

pub(super) fn reset_id(role: SchemeKey) -> i32 {
    RESET_ROLE + role as i32
}

pub(super) fn role_from_id(id: i32) -> Option<SchemeKey> {
    SchemeKey::ALL
        .get(id.checked_sub(ROLE_ROW)? as usize)
        .copied()
}

pub(super) fn reset_from_id(id: i32) -> Option<SchemeKey> {
    SchemeKey::ALL
        .get(id.checked_sub(RESET_ROLE)? as usize)
        .copied()
}

// Compose the legend from the effective chord, including per-action overrides.
// The base always reads first, followed by the context and the window grab.
fn example_modifiers(settings: &Settings, chord: Chord) -> impl Iterator<Item = &'static str> {
    [
        SchemeKey::Base,
        SchemeKey::Desktop,
        SchemeKey::Resize,
        SchemeKey::Grab,
    ]
    .into_iter()
    .filter_map(move |role| {
        let bit = settings.scheme_modifier(role);
        Chord::modifier_labels(chord.mods & bit).next()
    })
}

fn examples(role: SchemeKey) -> (&'static str, [Shortcut; 5]) {
    use Shortcut::*;
    match role {
        SchemeKey::Left => (
            "Сочетания влево",
            [
                FocusLeft,
                MoveLeft,
                PreviousDesktop,
                MovePreviousDesktop,
                QuickLeft,
            ],
        ),
        SchemeKey::Up => (
            "Сочетания вверх",
            [
                FocusUp,
                MoveUp,
                PreviousDesktopAlt,
                MovePreviousDesktopAlt,
                QuickUp,
            ],
        ),
        SchemeKey::Down => (
            "Сочетания вниз",
            [
                FocusDown,
                MoveDown,
                NextDesktopAlt,
                MoveNextDesktopAlt,
                QuickDown,
            ],
        ),
        _ => (
            "Сочетания вправо",
            [
                FocusRight,
                MoveRight,
                NextDesktop,
                MoveNextDesktop,
                QuickRight,
            ],
        ),
    }
}

impl ShortcutWindow {
    pub(super) fn scheme_visibility(&self) {
        let hwnd = self.hwnd.get();
        let view = child(hwnd, VIEW);
        if view.is_null() {
            return;
        }
        let advanced = self.advanced.get();
        let show = |id, visible| unsafe {
            ShowWindow(
                child(view, id),
                if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
            );
        };
        for (i, &(_, actions)) in view::GROUPS.iter().enumerate() {
            show(SECTION + i as i32, advanced);
            for &action in actions {
                show(row_id(action), advanced);
                show(RESET_ROW + action.index() as i32, advanced);
            }
        }
        for role in SchemeKey::ALL {
            show(role_id(role), !advanced);
            show(reset_id(role), !advanced);
        }
        for i in 0..GROUP_LABELS.len() {
            show(HEADING + i as i32, !advanced);
        }
        for i in 0..EXAMPLES.len() {
            show(EXAMPLE + i as i32, !advanced);
        }
        caption(
            child(hwnd, PAGE_SWITCH),
            if advanced {
                "Схема управления"
            } else {
                "Все действия"
            },
        );
        self.scroll.set(0);
        self.update_scheme();
        self.layout(hwnd);
    }

    pub(super) fn update_scheme(&self) {
        let hwnd = self.hwnd.get();
        let view = child(hwnd, VIEW);
        if self.advanced.get() || view.is_null() {
            return;
        }
        let settings = self.settings.get();
        let selected = self.role.get();
        let update = |control, name: &str| {
            caption(control, name);
            unsafe { InvalidateRect(control, null(), 0) };
        };
        for role in SchemeKey::ALL {
            let name = format!("{}, {}", role.label(), self.role_caption(role));
            update(child(view, role_id(role)), &name);
            let enabled = settings.scheme_key(role) != role.default_key() && !self.saving.get();
            unsafe { EnableWindow(child(view, reset_id(role)), enabled as i32) };
            if selected == role {
                update(child(hwnd, PREVIEW), &name);
            }
        }
        let (heading, actions) = examples(selected);
        update(child(view, HEADING + 2), heading);
        for (i, action) in actions.into_iter().enumerate() {
            let binding = settings.binding(action).map_or_else(
                || "Не назначено".into(),
                |chord| {
                    let mut label = String::new();
                    for modifier in example_modifiers(&settings, chord) {
                        label.push_str(modifier);
                        label.push('+');
                    }
                    label.push_str(&Chord::key_label(chord.key));
                    label
                },
            );
            update(
                child(view, EXAMPLE + i as i32),
                &format!("{}, {binding}", EXAMPLES[i]),
            );
        }
    }

    pub(super) fn layout_scheme(&self, view: HWND, width: i32, height: i32) {
        let p = |n| self.p(n);
        let side_by_side = width >= p(680);
        let group_height = p(34);
        let row_height = p(42);
        let group_gap = p(16);
        let group = group_height + 4 * row_height;
        let second = group + group_gap;
        let example_y = if side_by_side {
            0
        } else {
            second + group + group_gap
        };
        let example_h = p(72);
        let bottom =
            (second + group).max(example_y + group_height + example_h * EXAMPLES.len() as i32);
        let client_width = self.scroll_extent(view, p(16) + bottom + p(18), height);
        let margin = self.margin(width);
        let gap = p(30);
        let available = client_width - 2 * margin;
        let column = if side_by_side {
            (available - gap) / 2
        } else {
            available
        };
        let top = p(16) - self.scroll.get();
        let right_x = if side_by_side {
            margin + column + gap
        } else {
            margin
        };
        let right_w = client_width - margin - right_x;
        for (group_index, offset) in [0, second].into_iter().enumerate() {
            let y = top + offset;
            place(
                child(view, HEADING + group_index as i32),
                margin,
                y,
                column,
                group_height,
            );
            for (row, &role) in SchemeKey::ALL[group_index * 4..][..4].iter().enumerate() {
                let y = y + group_height + row_height * row as i32;
                let reset_w = p(28);
                place(
                    child(view, role_id(role)),
                    margin,
                    y,
                    column - reset_w,
                    row_height,
                );
                place(
                    child(view, reset_id(role)),
                    margin + column - reset_w,
                    y,
                    reset_w,
                    row_height,
                );
            }
        }
        let y = top + example_y;
        place(child(view, HEADING + 2), right_x, y, right_w, group_height);
        for i in 0..EXAMPLES.len() as i32 {
            place(
                child(view, EXAMPLE + i),
                right_x,
                y + group_height + i * example_h,
                right_w,
                example_h,
            );
        }
    }

    pub(super) fn draw_scheme(&self, item: &DRAWITEMSTRUCT) -> bool {
        let id = item.CtlID as i32;
        let heading = (HEADING..HEADING + GROUP_LABELS.len() as i32).contains(&id);
        let example = (EXAMPLE..EXAMPLE + EXAMPLES.len() as i32).contains(&id);
        let role = role_from_id(id);
        let preview = id == PREVIEW && !self.advanced.get();
        if !heading && !example && role.is_none() && !preview && id != PAGE_SWITCH {
            return false;
        }
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else {
            return true;
        };
        let colors = self.colors.get();
        let dc = item.hDC;
        let r = item.rcItem;
        unsafe { SetBkMode(dc, TRANSPARENT as i32) };
        if preview {
            self.draw_role_preview(item, fonts);
        } else if id == PAGE_SWITCH {
            self.draw_page_switch(item, fonts);
        } else {
            paint::fill(dc, &r, colors.background, 0);
            if let Some(role) = role {
                self.draw_role(item, role, fonts);
            } else if heading {
                let i = (id - HEADING) as usize;
                let label = if i == 2 {
                    examples(self.role.get()).0
                } else {
                    GROUP_LABELS[i]
                };
                let glyph = ["\u{e765}", "\u{e7c2}", "\u{e8a9}"][i];
                let icon = RECT {
                    right: r.left + self.p(19),
                    ..r
                };
                paint::text(dc, icon, glyph, fonts.symbol, colors.secondary, DT_CENTER);
                let name = RECT {
                    left: icon.right + self.p(7),
                    ..r
                };
                paint::text(dc, name, label, fonts.heading, colors.text, DT_LEFT);
                let start = name.left + text_width(dc, fonts.heading, label) + self.p(12);
                if start < r.right {
                    let y = (r.top + r.bottom) / 2;
                    paint::fill(
                        dc,
                        &RECT {
                            left: start,
                            top: y,
                            bottom: y + self.p(1),
                            ..r
                        },
                        colors.border,
                        0,
                    );
                }
            } else {
                self.draw_example(item, (id - EXAMPLE) as usize, fonts);
            }
        }
        true
    }

    fn draw_role(&self, item: &DRAWITEMSTRUCT, role: SchemeKey, fonts: &Fonts) {
        let c = self.colors.get();
        let p = |n| self.p(n);
        let dc = item.hDC;
        let r = item.rcItem;
        let selected = self.role.get() == role;
        let recording = selected && self.recording_role();
        let hot = hot(item);
        let highlighted = c.high_contrast && (selected || hot);
        let fill = if highlighted || hot {
            c.hover
        } else if selected {
            blend(c.background, c.accent, 11)
        } else {
            c.background
        };
        let ink = if highlighted { c.hover_text } else { c.text };
        let outline = focused(item) || recording;
        if outline {
            let border = if recording && !c.high_contrast {
                c.accent
            } else {
                ink
            };
            fill_outlined(dc, inset(r, p(1)), fill, border, p(6), p(1));
        } else if hot || selected {
            paint::fill(dc, &inset(r, p(1)), fill, p(6));
        }
        let text = self.role_caption(role);
        let cap_width = (text_width(dc, fonts.regular, &text) + p(20))
            .max(p(43))
            .min((r.right - r.left) / 2);
        let mid = (r.top + r.bottom) / 2;
        let cap = RECT {
            left: r.right - p(7) - cap_width,
            right: r.right - p(7),
            top: mid - p(13),
            bottom: mid + p(13),
        };
        paint::text(
            dc,
            RECT {
                left: r.left + p(11),
                right: cap.left - p(7),
                ..r
            },
            role.label(),
            fonts.regular,
            ink,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        let active = recording || selected;
        let face = if active { c.accent } else { c.control };
        let edge = if active && !c.high_contrast {
            blend(face, c.text, 18)
        } else {
            c.border
        };
        fill_outlined(dc, cap, face, edge, p(4), p(1));
        paint::text(
            dc,
            inset(cap, p(2)),
            &text,
            fonts.regular,
            if active { c.on_accent } else { c.text },
            DT_CENTER | DT_END_ELLIPSIS,
        );
    }

    fn draw_example(&self, item: &DRAWITEMSTRUCT, index: usize, fonts: &Fonts) {
        let c = self.colors.get();
        let p = |n| self.p(n);
        let r = item.rcItem;
        let dc = item.hDC;
        let title = RECT {
            left: r.left + p(4),
            top: r.top + p(5),
            bottom: r.top + p(28),
            ..r
        };
        paint::text(
            dc,
            title,
            EXAMPLES[index],
            fonts.regular,
            c.text,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        let mut cap = RECT {
            top: title.bottom + p(3),
            bottom: title.bottom + p(28),
            ..title
        };
        let action = examples(self.role.get()).1[index];
        let settings = self.settings.get();
        if let Some(chord) = settings.binding(action) {
            let key = Chord::key_label(chord.key);
            let mut labels: Vec<&str> = example_modifiers(&settings, chord).collect();
            labels.push(&key);
            for legend in labels {
                cap.right = (cap.left + (text_width(dc, fonts.regular, legend) + p(16)).max(p(32)))
                    .min(r.right);
                if cap.right <= cap.left {
                    break;
                }
                fill_outlined(dc, cap, c.control, c.border, p(4), p(1));
                paint::text(
                    dc,
                    inset(cap, p(2)),
                    legend,
                    fonts.regular,
                    c.text,
                    DT_CENTER | DT_END_ELLIPSIS,
                );
                cap.left = cap.right + p(5);
            }
        } else {
            paint::text(dc, cap, "Не назначено", fonts.regular, c.secondary, DT_LEFT);
        }
    }

    fn draw_page_switch(&self, item: &DRAWITEMSTRUCT, fonts: &Fonts) {
        let c = self.colors.get();
        let p = |n| self.p(n);
        let r = item.rcItem;
        let dc = item.hDC;
        let hot = hot(item);
        let surface = if hot { c.hover } else { c.control };
        let ink = if hot { c.hover_text } else { c.text };
        paint::fill(dc, &r, self.header_color(), 0);
        fill_outlined(
            dc,
            inset(r, p(1)),
            surface,
            if focused(item) { c.text } else { c.border },
            p(6),
            p(1),
        );
        let advanced = self.advanced.get();
        let label = if advanced {
            "Схема управления"
        } else {
            "Все действия"
        };
        let name_width = text_width(dc, fonts.regular, label);
        let start = (r.left + r.right - name_width - p(21)) / 2;
        let mut symbol = RECT {
            left: start,
            right: start + p(15),
            ..r
        };
        let mut name = RECT {
            left: start,
            right: start + name_width + p(1),
            ..r
        };
        if advanced {
            name.left += p(21);
            name.right += p(21);
        } else {
            symbol.left += name_width + p(6);
            symbol.right += name_width + p(6);
        }
        paint::text(dc, name, label, fonts.regular, ink, DT_LEFT);
        paint::text(
            dc,
            symbol,
            if advanced { "\u{e76b}" } else { "\u{e76c}" },
            fonts.symbol,
            ink,
            DT_CENTER,
        );
    }

    fn draw_role_preview(&self, item: &DRAWITEMSTRUCT, fonts: &Fonts) {
        let c = self.colors.get();
        let p = |n| self.p(n);
        let role = self.role.get();
        let recording = self.recording_role();
        let category = if recording {
            "Запись клавиши"
        } else {
            "Схема управления"
        };
        let value = self.role_caption(role);
        header::buffered(item, |dc, r| {
            paint::fill(dc, &r, self.header_color(), 0);
            let mid = (r.bottom + r.top) / 2;
            let cap_width = (text_width(dc, fonts.heading, &value) + p(28))
                .max(p(60))
                .min((r.right - r.left) / 2);
            let title = RECT {
                left: r.left,
                right: r.right - cap_width - p(9),
                top: mid - p(5),
                bottom: mid + p(26),
            };
            let group = RECT {
                top: mid - p(25),
                bottom: title.top,
                ..title
            };
            paint::text(
                dc,
                group,
                category,
                fonts.small,
                if recording && !c.high_contrast {
                    c.accent
                } else {
                    c.secondary
                },
                DT_LEFT | DT_END_ELLIPSIS,
            );
            let font = if text_width(dc, fonts.display, role.label()) <= title.right - title.left {
                fonts.display
            } else {
                fonts.heading
            };
            paint::text(
                dc,
                title,
                role.label(),
                font,
                c.text,
                DT_LEFT | DT_END_ELLIPSIS,
            );
            let cap = RECT {
                left: r.right - cap_width,
                right: r.right,
                top: mid - p(21),
                bottom: mid + p(21),
            };
            paint::fill(
                dc,
                &cap,
                if c.high_contrast {
                    c.border
                } else {
                    blend(c.accent, 0, 38)
                },
                p(6),
            );
            let face = RECT {
                bottom: cap.bottom - p(3),
                ..cap
            };
            fill_outlined(
                dc,
                face,
                c.accent,
                if c.high_contrast {
                    c.border
                } else {
                    blend(c.accent, c.text, 22)
                },
                p(6),
                p(1),
            );
            if value == "Win" {
                header::windows_mark(dc, face, p(15), p(1), c.on_accent);
            } else {
                paint::text(
                    dc,
                    inset(face, p(2)),
                    &value,
                    fonts.heading,
                    c.on_accent,
                    DT_CENTER | DT_END_ELLIPSIS,
                );
            }
        });
    }
}

fn hot(item: &DRAWITEMSTRUCT) -> bool {
    let mut point = POINT::default();
    item.itemState & ODS_SELECTED != 0
        || unsafe { GetCursorPos(&mut point) != 0 && WindowFromPoint(point) == item.hwndItem }
}

fn focused(item: &DRAWITEMSTRUCT) -> bool {
    item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0
}
