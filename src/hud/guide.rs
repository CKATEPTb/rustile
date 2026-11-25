use crate::settings::{Settings, Shortcut};

const ARROWS: &str = "← ↑ ↓ →";

pub(super) struct Guide {
    pub(super) rows: Vec<HintRow>,
}

pub(super) struct HintRow {
    pub(super) action: String,
    pub(super) keys: Vec<String>,
}

impl Guide {
    pub(super) const TITLE: &'static str = "Изменение размера";

    pub(super) fn new(settings: &Settings) -> Self {
        use Shortcut::*;

        let mut rows = Vec::new();
        for (label, keys, actions) in [
            (
                "Сдвинуть границу по стрелке",
                ARROWS,
                [
                    (GrowLeft, "Сдвинуть влево"),
                    (GrowRight, "Сдвинуть вправо"),
                    (GrowUp, "Сдвинуть вверх"),
                    (GrowDown, "Сдвинуть вниз"),
                ],
            ),
            (
                "Сдвинуть её против стрелки",
                "Shift + стрелка",
                [
                    (ShrinkLeft, "Развернуть шаг «Влево»"),
                    (ShrinkRight, "Развернуть шаг «Вправо»"),
                    (ShrinkUp, "Развернуть шаг «Вверх»"),
                    (ShrinkDown, "Развернуть шаг «Вниз»"),
                ],
            ),
        ] {
            if actions
                .iter()
                .all(|(action, _)| settings.binding(*action) == Some(action.default_chord()))
            {
                rows.push(HintRow {
                    action: label.into(),
                    keys: vec![keys.into()],
                });
            } else {
                for (action, label) in actions {
                    if let Some(chord) = settings.binding(action) {
                        rows.push(HintRow {
                            action: label.into(),
                            keys: vec![chord.label()],
                        });
                    }
                }
            }
        }
        let keys: Vec<_> = [Confirm, Cancel]
            .into_iter()
            .filter_map(|action| settings.binding(action).map(|chord| chord.label()))
            .collect();
        rows.push(HintRow {
            action: if keys.is_empty() {
                "Для выхода выберите другое окно"
            } else {
                "Выйти из режима"
            }
            .into(),
            keys,
        });
        Self { rows }
    }

    pub(super) fn accessible_text(&self) -> String {
        let mut text = Self::TITLE.to_owned();
        for row in &self.rows {
            text.push_str("\r\n");
            text.push_str(&row.action);
            for (index, key) in row.keys.iter().enumerate() {
                text.push_str(if index == 0 { " — " } else { " или " });
                text.push_str(if key == ARROWS {
                    "клавиши со стрелками"
                } else {
                    key
                });
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_bindings_share_rows_and_exits_are_alternatives() {
        let guide = Guide::new(&Settings::default());
        assert_eq!(guide.rows.len(), 3);
        assert_eq!(guide.rows[0].keys, [ARROWS]);
        assert_eq!(guide.rows[1].keys, ["Shift + стрелка"]);
        assert_eq!(guide.rows[2].keys, ["Enter", "Esc"]);
        assert!(guide.accessible_text().contains("Enter или Esc"));
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
        assert_eq!(guide.rows[0].action, "Сдвинуть влево");
        assert_eq!(guide.rows[0].keys, ["F6"]);
        assert_eq!(guide.rows[3].action, "Развернуть шаг «Влево»");
        assert_eq!(guide.rows[3].keys, ["Shift+F6"]);
        assert!(guide.rows.last().unwrap().keys.is_empty());
        let text = guide.accessible_text();
        assert!(text.contains("другое окно"));
        assert!(!text.contains("Сдвинуть вверх") && !text.contains("Esc"));
    }
}
