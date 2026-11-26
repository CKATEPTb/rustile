use crate::settings::{Chord, Settings, Shortcut};

pub(super) struct Guide {
    pub(super) rows: Vec<HintRow>,
}

pub(super) struct HintRow {
    pub(super) action: String,
    choices: Vec<Chord>,
}

impl HintRow {
    // Each cap has a separate printed connector. For Shift+any arrow, the
    // repeated modifiers appear just once before the four direction choices.
    pub(super) fn keycaps(&self) -> Vec<(&'static str, String)> {
        let same_mods = self
            .choices
            .windows(2)
            .all(|pair| pair[0].mods == pair[1].mods);
        let mut caps = Vec::new();
        for (choice, chord) in self.choices.iter().enumerate() {
            let mods = if choice > 0 && same_mods {
                0
            } else {
                chord.mods
            };
            for (index, label) in Chord::key_labels(mods, Some(chord.key))
                .into_iter()
                .enumerate()
            {
                let connector = match (choice, index) {
                    (0, 0) => "",
                    (_, 0) => "/",
                    _ => "+",
                };
                caps.push((connector, label));
            }
        }
        caps
    }
}

impl Guide {
    pub(super) const TITLE: &'static str = "Изменение размера";

    pub(super) fn new(settings: &Settings) -> Self {
        use Shortcut::*;

        let mut rows = Vec::new();
        for (label, actions) in [
            (
                "Сдвинуть границу по стрелке",
                [GrowLeft, GrowUp, GrowDown, GrowRight],
            ),
            (
                "Сдвинуть её против стрелки",
                [ShrinkLeft, ShrinkUp, ShrinkDown, ShrinkRight],
            ),
        ] {
            if actions
                .iter()
                .all(|&action| settings.binding(action) == Some(action.default_chord()))
            {
                rows.push(HintRow {
                    action: label.into(),
                    choices: actions.into_iter().map(Shortcut::default_chord).collect(),
                });
            } else {
                for action in actions {
                    if let Some(chord) = settings.binding(action) {
                        rows.push(HintRow {
                            action: action.label().into(),
                            choices: vec![chord],
                        });
                    }
                }
            }
        }
        let choices: Vec<_> = [Confirm, Cancel]
            .into_iter()
            .filter_map(|action| settings.binding(action))
            .collect();
        rows.push(HintRow {
            action: if choices.is_empty() {
                "Для выхода выберите другое окно"
            } else {
                "Выйти из режима"
            }
            .into(),
            choices,
        });
        Self { rows }
    }

    pub(super) fn accessible_text(&self) -> String {
        let mut text = Self::TITLE.to_owned();
        for row in &self.rows {
            text.push_str("\r\n");
            text.push_str(&row.action);
            for (index, chord) in row.choices.iter().enumerate() {
                text.push_str(if index == 0 { " — " } else { " или " });
                text.push_str(&chord.label());
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_caps(row: &HintRow, expected: &[(&str, &str)]) {
        let caps = row.keycaps();
        let parts: Vec<_> = caps
            .iter()
            .map(|(join, key)| (*join, key.as_str()))
            .collect();
        assert_eq!(parts, expected);
    }

    #[test]
    fn standard_bindings_share_rows_and_exits_are_alternatives() {
        let guide = Guide::new(&Settings::default());
        assert_eq!(guide.rows.len(), 3);
        assert_caps(
            &guide.rows[0],
            &[("", "Left"), ("/", "Up"), ("/", "Down"), ("/", "Right")],
        );
        assert_caps(
            &guide.rows[1],
            &[
                ("", "Shift"),
                ("+", "Left"),
                ("/", "Up"),
                ("/", "Down"),
                ("/", "Right"),
            ],
        );
        assert_caps(&guide.rows[2], &[("", "Enter"), ("/", "Esc")]);
        let text = guide.accessible_text();
        assert!(text.contains("Shift+Left или Shift+Up или Shift+Down или Shift+Right"));
        assert!(text.contains("Enter или Esc"));
    }

    #[test]
    fn custom_bindings_name_their_steps_and_omit_disabled_keys() {
        let settings = toml::from_str(
            "shortcuts.grow_left = { mods = 0, key = 117 }\n\
             shortcuts.grow_up = 'disabled'\n\
             shortcuts.shrink_left = { mods = 4, key = 117 }\n\
             shortcuts.confirm = 'disabled'\n\
             shortcuts.cancel = 'disabled'",
        )
        .unwrap();
        let guide = Guide::new(&settings);
        assert_eq!(guide.rows.len(), 8);
        assert_eq!(guide.rows[0].action, Shortcut::GrowLeft.label());
        assert_caps(&guide.rows[0], &[("", "F6")]);
        assert_eq!(guide.rows[3].action, Shortcut::ShrinkLeft.label());
        assert_caps(&guide.rows[3], &[("", "Shift"), ("+", "F6")]);
        assert!(guide.rows.last().unwrap().choices.is_empty());
        let text = guide.accessible_text();
        assert!(text.contains("другое окно"));
        assert!(!text.contains(Shortcut::GrowUp.label()) && !text.contains("Esc"));
    }
}
