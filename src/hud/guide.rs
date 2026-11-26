use crate::{
    i18n::{Msg, tr},
    settings::{Chord, Settings, Shortcut},
};

pub(super) struct Guide {
    pub(super) rows: Vec<HintRow>,
}

pub(super) struct HintRow {
    pub(super) action: String,
    choices: Vec<Chord>,
    compact_directions: bool,
}

impl HintRow {
    // Each cap has a separate printed connector. For Shift+any arrow, the
    // repeated modifiers appear just once before the four direction choices.
    pub(super) fn keycaps(&self) -> Vec<(&'static str, String)> {
        let mut caps = Vec::new();
        for (choice, chord) in self.choices.iter().enumerate() {
            let mods = if choice > 0 && self.compact_directions {
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
    pub(super) fn title() -> &'static str {
        tr(Msg::HudTitle)
    }

    pub(super) fn new(settings: &Settings) -> Self {
        use Shortcut::*;

        let mut rows = Vec::new();
        for (label, actions) in [
            (Msg::HudWithArrow, [GrowLeft, GrowUp, GrowDown, GrowRight]),
            (
                Msg::HudAgainstArrow,
                [ShrinkLeft, ShrinkUp, ShrinkDown, ShrinkRight],
            ),
        ] {
            if actions
                .iter()
                .all(|&action| settings.bindings(action) == [action.default_chord()])
            {
                rows.push(HintRow {
                    action: tr(label).into(),
                    choices: actions.into_iter().map(Shortcut::default_chord).collect(),
                    compact_directions: true,
                });
            } else {
                for action in actions {
                    let choices = settings.bindings(action);
                    if !choices.is_empty() {
                        rows.push(HintRow {
                            action: action.label().into(),
                            choices,
                            compact_directions: false,
                        });
                    }
                }
            }
        }
        rows.push(HintRow {
            action: tr(Msg::HudExit).into(),
            choices: vec![Confirm.default_chord(), Cancel.default_chord()],
            compact_directions: false,
        });
        Self { rows }
    }

    pub(super) fn accessible_text(&self) -> String {
        let mut text = Self::title().to_owned();
        let or = format!(" {} ", tr(Msg::HudOr));
        for row in &self.rows {
            text.push_str("\r\n");
            text.push_str(&row.action);
            for (index, chord) in row.choices.iter().enumerate() {
                text.push_str(if index == 0 { " — " } else { &or });
                text.push_str(&chord.label());
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alternatives(keys: &[&str]) -> String {
        keys.join(&format!(" {} ", tr(Msg::HudOr)))
    }

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
        assert!(text.contains(&alternatives(&[
            "Shift+Left",
            "Shift+Up",
            "Shift+Down",
            "Shift+Right"
        ])));
        assert!(text.contains(&alternatives(&["Enter", "Esc"])));
    }

    #[test]
    fn custom_alternatives_show_each_complete_chord() {
        use crate::settings::{Assignment, Command};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_SHIFT, VK_J};
        let action = Shortcut::ShrinkLeft;
        let settings = Settings::default()
            .changed(Command::Assign(Assignment::Add(
                action,
                Chord {
                    mods: MOD_SHIFT,
                    key: VK_J,
                },
            )))
            .unwrap();
        let guide = Guide::new(&settings);
        let row = guide
            .rows
            .iter()
            .find(|row| row.action == action.label())
            .unwrap();
        assert_caps(
            row,
            &[("", "Shift"), ("+", "Left"), ("/", "Shift"), ("+", "J")],
        );
        assert!(
            guide
                .accessible_text()
                .contains(&alternatives(&["Shift+Left", "Shift+J"]))
        );
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
        assert_caps(guide.rows.last().unwrap(), &[("", "Enter"), ("/", "Esc")]);
        let text = guide.accessible_text();
        assert!(text.contains(&alternatives(&["Enter", "Esc"])));
        assert!(!text.contains(Shortcut::GrowUp.label()));
    }
}
