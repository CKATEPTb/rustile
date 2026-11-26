use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

use super::Chord;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SchemeKey {
    Base,
    Grab,
    Desktop,
    Resize,
    Left,
    Right,
    Up,
    Down,
}

impl SchemeKey {
    const ALL: [Self; 8] = [
        Self::Base,
        Self::Grab,
        Self::Desktop,
        Self::Resize,
        Self::Left,
        Self::Right,
        Self::Up,
        Self::Down,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Base => "Основная клавиша",
            Self::Grab => "Захват окна",
            Self::Desktop => "Рабочие столы",
            Self::Resize => "Быстрое изменение размера",
            Self::Left => "Влево",
            Self::Right => "Вправо",
            Self::Up => "Вверх",
            Self::Down => "Вниз",
        }
    }

    const fn is_modifier(self) -> bool {
        matches!(self, Self::Base | Self::Grab | Self::Desktop | Self::Resize)
    }

    const fn default_key(self) -> u16 {
        match self {
            Self::Base => VK_LWIN,
            Self::Grab => VK_SHIFT,
            Self::Desktop => VK_CONTROL,
            Self::Resize => VK_MENU,
            Self::Left => VK_LEFT,
            Self::Right => VK_RIGHT,
            Self::Up => VK_UP,
            Self::Down => VK_DOWN,
        }
    }

    fn validate(self, key: u16) -> Result<(), String> {
        if self == Self::Base && key == VK_SHIFT {
            return Err("Для управления Rustile выберите Win, Ctrl или Alt. Shift нужен для обычного ввода заглавных букв.".into());
        }
        if self.is_modifier() && modifier_bit(key) == 0 {
            return Err("Для этой роли выберите Win, Shift, Ctrl или Alt.".into());
        }
        if !self.is_modifier() && (!Chord::supports_key(key) || key >= 0x100) {
            return Err("Для направления выберите одну обычную клавишу клавиатуры.".into());
        }
        Ok(())
    }
}

fn canonical(key: u16) -> u16 {
    match key {
        VK_RWIN => VK_LWIN,
        VK_LSHIFT | VK_RSHIFT => VK_SHIFT,
        VK_LCONTROL | VK_RCONTROL => VK_CONTROL,
        VK_LMENU | VK_RMENU => VK_MENU,
        _ => key,
    }
}

fn modifier_bit(key: u16) -> u32 {
    match key {
        VK_LWIN => MOD_WIN,
        VK_SHIFT => MOD_SHIFT,
        VK_CONTROL => MOD_CONTROL,
        VK_MENU => MOD_ALT,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "BTreeMap<SchemeKey, u16>", into = "BTreeMap<SchemeKey, u16>")]
// Existing profiles can still supply shared defaults. Keep them when saving so
// unchanged modal shortcuts retain their held-modifier variants as well.
pub(super) struct Scheme([u16; 8]);

impl Default for Scheme {
    fn default() -> Self {
        Self(SchemeKey::ALL.map(SchemeKey::default_key))
    }
}

impl From<BTreeMap<SchemeKey, u16>> for Scheme {
    fn from(stored: BTreeMap<SchemeKey, u16>) -> Self {
        let mut scheme = Self::default();
        for (role, key) in stored {
            scheme.0[role as usize] = canonical(key);
        }
        scheme
    }
}

impl From<Scheme> for BTreeMap<SchemeKey, u16> {
    fn from(scheme: Scheme) -> Self {
        SchemeKey::ALL
            .into_iter()
            .filter_map(|role| {
                let key = scheme.0[role as usize];
                (key != role.default_key()).then_some((role, key))
            })
            .collect()
    }
}

impl Scheme {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        for (i, role) in SchemeKey::ALL.into_iter().enumerate() {
            let key = self.0[i];
            role.validate(key)?;
            if let Some(other) = SchemeKey::ALL[..i]
                .iter()
                .find(|&&other| self.0[other as usize] == key)
            {
                return Err(format!(
                    "{} назначена и на «{}», и на «{}».",
                    Chord::key_label(key),
                    other.label(),
                    role.label()
                ));
            }
        }
        Ok(())
    }

    pub(super) fn key(&self, role: SchemeKey) -> u16 {
        self.0[role as usize]
    }

    pub(super) fn modifier(&self, role: SchemeKey) -> u32 {
        modifier_bit(self.key(role))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Command, Settings, Shortcut, WHEEL_UP};

    fn loaded(text: &str) -> Settings {
        toml::from_str::<Settings>(text)
            .unwrap()
            .validated()
            .unwrap()
    }

    #[test]
    fn saved_scheme_preserves_its_chords_aliases_and_exceptions_across_individual_edits() {
        use Shortcut::*;
        let previous = loaded(
            "[scheme]\nbase = 18\ngrab = 17\ndesktop = 16\nresize = 91\nleft = 72\n\
             [shortcuts]\nmove_down = 'disabled'\nmove_right = { mods = 8, key = 81 }",
        );
        let changed = previous.changed(Command::ResetShortcut(FocusLeft)).unwrap();
        let reloaded = loaded(&toml::to_string_pretty(&changed).unwrap());
        assert_eq!(changed, reloaded);
        assert_eq!(
            previous.binding(FocusLeft),
            Some(Chord {
                mods: MOD_ALT,
                key: VK_H
            })
        );
        assert_eq!(reloaded.binding(FocusLeft), Some(FocusLeft.default_chord()));
        for other in Shortcut::ALL {
            if other != FocusLeft {
                assert_eq!(
                    reloaded.binding(other),
                    previous.binding(other),
                    "{other:?}"
                );
            }
        }
        for (action, mods, key) in [
            (MoveLeft, MOD_ALT | MOD_CONTROL, VK_H),
            (PreviousDesktop, MOD_ALT | MOD_SHIFT, VK_H),
            (MovePreviousDesktop, MOD_ALT | MOD_CONTROL | MOD_SHIFT, VK_H),
            (QuickLeft, MOD_ALT | MOD_WIN, VK_H),
            (QuickReverseLeft, MOD_ALT | MOD_WIN | MOD_CONTROL, VK_H),
            (ShrinkLeft, MOD_CONTROL, VK_H),
            (StartResize, MOD_ALT, VK_RETURN),
            (CloseWindow, MOD_ALT, VK_Q),
            (PreviousDesktopWheel, MOD_ALT, WHEEL_UP),
            (MoveRight, MOD_WIN, VK_Q),
        ] {
            assert_eq!(
                reloaded.binding(action),
                Some(Chord { mods, key }),
                "{action:?}"
            );
        }
        assert_eq!(reloaded.binding(MoveDown), None);
        assert!(reloaded.resize_alias(
            ShrinkLeft,
            Chord {
                mods: MOD_ALT | MOD_CONTROL,
                key: VK_H
            }
        ));
        assert!(!reloaded.resize_alias(
            ShrinkLeft,
            Chord {
                mods: MOD_WIN | MOD_SHIFT,
                key: VK_LEFT
            }
        ));
        assert_eq!(
            reloaded.changed(Command::ResetShortcuts).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn resetting_to_a_factory_chord_keeps_its_current_owner_until_confirmed() {
        use Shortcut::*;
        let settings = loaded("scheme.left = 72\nshortcuts.move_up = { mods = 8, key = 37 }");
        assert!(settings.changed(Command::ResetShortcut(FocusLeft)).is_err());
        let reassigned = settings
            .changed(Command::ReassignShortcut(
                FocusLeft,
                FocusLeft.default_chord(),
                MoveUp.into(),
            ))
            .unwrap();
        let reloaded = loaded(&toml::to_string_pretty(&reassigned).unwrap());
        assert_eq!(reloaded.binding(MoveUp), None);
        assert_eq!(reloaded.binding(FocusLeft), Some(FocusLeft.default_chord()));
        assert_eq!(reloaded.binding(MoveLeft), settings.binding(MoveLeft));
    }

    #[test]
    fn invalid_scheme_files_still_report_validation_errors() {
        for text in [
            "scheme.left = 39",
            "scheme.base = 16",
            "scheme.left = 256",
            "scheme.left = 76",
        ] {
            let settings: Settings = toml::from_str(text).unwrap();
            assert!(settings.validated().is_err(), "{text}");
        }
    }
}
