use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

use super::{Chord, Settings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SchemeKey {
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
    pub const ALL: [Self; 8] = [
        Self::Base,
        Self::Grab,
        Self::Desktop,
        Self::Resize,
        Self::Left,
        Self::Right,
        Self::Up,
        Self::Down,
    ];

    pub const fn label(self) -> &'static str {
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

    pub const fn is_modifier(self) -> bool {
        matches!(self, Self::Base | Self::Grab | Self::Desktop | Self::Resize)
    }

    pub const fn default_key(self) -> u16 {
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
}

impl Settings {
    pub fn scheme_key(&self, role: SchemeKey) -> u16 {
        self.scheme.0[role as usize]
    }

    pub(crate) fn scheme_modifier(&self, role: SchemeKey) -> u32 {
        modifier_bit(self.scheme_key(role))
    }

    pub fn scheme_partner(&self, role: SchemeKey, key: u16) -> Option<SchemeKey> {
        SchemeKey::ALL.into_iter().find(|&other| {
            other != role
                && other.is_modifier() == role.is_modifier()
                && self.scheme_key(other) == canonical(key)
        })
    }

    pub(super) fn set_scheme_key(&mut self, role: SchemeKey, key: u16) -> Result<(), String> {
        let key = canonical(key);
        role.validate(key)?;
        if let Some(other) = self.scheme_partner(role, key) {
            let original = self.scheme_key(role);
            other.validate(original)?;
            self.scheme.0[other as usize] = original;
        }
        self.scheme.0[role as usize] = key;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Command, Shortcut, WHEEL_UP};

    #[test]
    fn shared_keys_compose_directions_with_each_action_and_modal_alias() {
        use SchemeKey::{Base, Desktop, Down, Grab, Left, Resize};
        use Shortcut::*;
        let settings = Settings::default()
            .changed(Command::SchemeKey(Base, VK_RMENU))
            .unwrap()
            .changed(Command::SchemeKey(Grab, VK_RCONTROL))
            .unwrap()
            .changed(Command::SchemeKey(Left, VK_H))
            .unwrap();
        assert_eq!(settings.scheme_key(Base), VK_MENU);
        assert_eq!(settings.scheme_key(Grab), VK_CONTROL);
        assert_eq!(settings.scheme_key(Desktop), VK_SHIFT);
        assert_eq!(settings.scheme_key(Resize), VK_LWIN);
        for (action, mods, key) in [
            (FocusLeft, MOD_ALT, VK_H),
            (MoveLeft, MOD_ALT | MOD_CONTROL, VK_H),
            (PreviousDesktop, MOD_ALT | MOD_SHIFT, VK_H),
            (MovePreviousDesktop, MOD_ALT | MOD_CONTROL | MOD_SHIFT, VK_H),
            (NextDesktopAlt, MOD_ALT | MOD_SHIFT, Down.default_key()),
            (QuickLeft, MOD_ALT | MOD_WIN, VK_H),
            (QuickReverseLeft, MOD_ALT | MOD_WIN | MOD_CONTROL, VK_H),
            (GrowLeft, 0, VK_H),
            (ShrinkLeft, MOD_CONTROL, VK_H),
            (StartResize, MOD_ALT, VK_RETURN),
            (Confirm, 0, VK_RETURN),
            (Cancel, 0, VK_ESCAPE),
            (CloseWindow, MOD_ALT, VK_Q),
            (Desktop10, MOD_ALT, VK_0),
            (PreviousDesktopWheel, MOD_ALT, WHEEL_UP),
        ] {
            assert_eq!(
                settings.binding(action),
                Some(Chord { mods, key }),
                "{action:?}"
            );
        }
        assert!(settings.resize_alias(
            ShrinkLeft,
            Chord {
                mods: MOD_ALT | MOD_CONTROL,
                key: VK_H
            }
        ));
        assert!(!settings.resize_alias(
            ShrinkLeft,
            Chord {
                mods: MOD_WIN | MOD_SHIFT,
                key: VK_LEFT
            }
        ));
        assert!(!settings.resize_alias(
            GrowLeft,
            Chord {
                mods: MOD_WIN,
                key: VK_H
            }
        ));
        let file = toml::to_string_pretty(&settings).unwrap();
        assert!(file.contains("[scheme]"));
        assert!(!file.contains("shortcuts"));
        assert_eq!(
            toml::from_str::<Settings>(&file)
                .unwrap()
                .validated()
                .unwrap(),
            settings
        );
    }

    #[test]
    fn changing_scheme_keeps_overrides_and_individual_reset_restores_inheritance() {
        use SchemeKey::{Base, Left, Right};
        use Shortcut::{FocusLeft, MoveUp};
        let defaults = Settings::default();
        let old_left = defaults.binding(FocusLeft).unwrap();
        let initial = defaults
            .changed(Command::ReassignShortcut(
                MoveUp,
                old_left,
                FocusLeft.into(),
            ))
            .unwrap();
        let swapped = initial
            .changed(Command::SchemeKey(Base, VK_MENU))
            .unwrap()
            .changed(Command::SchemeKey(Left, VK_RIGHT))
            .unwrap();
        assert_eq!(swapped.scheme_key(Right), VK_LEFT);
        assert_eq!(swapped.binding(FocusLeft), None);
        assert_eq!(swapped.binding(MoveUp), Some(old_left));
        assert!(swapped.has_shortcut_override(FocusLeft));
        let reset = swapped.changed(Command::ResetShortcut(FocusLeft)).unwrap();
        assert!(!reset.has_shortcut_override(FocusLeft));
        assert_eq!(
            reset.binding(FocusLeft),
            Some(Chord {
                mods: MOD_ALT,
                key: VK_RIGHT
            })
        );
        assert_eq!(reset.changed(Command::ResetShortcuts).unwrap(), defaults);

        // Win+Q is now free for an override. Reading a scheme-aware profile
        // must not apply pre-scheme default-key migrations against this chord.
        let saved = reset
            .changed(Command::Shortcut(
                MoveUp,
                Chord {
                    mods: MOD_WIN,
                    key: VK_Q,
                },
            ))
            .unwrap();
        let roundtrip: Settings = toml::from_str(&toml::to_string(&saved).unwrap()).unwrap();
        assert_eq!(roundtrip.validated().unwrap(), saved);
    }

    #[test]
    fn launch_only_opener_stays_pinned_when_a_matching_base_changes() {
        let stored = Settings::default();
        let live = stored
            .with_resize_override(crate::settings::Modifier::Win)
            .unwrap();
        let command = Command::SchemeKey(SchemeKey::Base, VK_MENU);
        let saved = stored.changed_for_save(command, live).unwrap();
        assert_eq!(
            live.changed(command)
                .unwrap()
                .binding(Shortcut::StartResize),
            stored.binding(Shortcut::StartResize)
        );
        assert_eq!(
            saved.binding(Shortcut::StartResize),
            Some(Chord {
                mods: MOD_ALT,
                key: VK_RETURN
            })
        );
    }

    #[test]
    fn invalid_roles_reserved_keys_and_conflicts_are_rejected_atomically() {
        use SchemeKey::{Base, Grab, Left};
        let original = Settings::default();
        assert!(
            original
                .changed(Command::SchemeKey(Base, VK_LSHIFT))
                .is_err()
        );
        assert!(original.changed(Command::SchemeKey(Grab, VK_RWIN)).is_err());
        assert!(
            original
                .changed(Command::SchemeKey(Left, VK_RCONTROL))
                .is_err()
        );
        assert!(
            original
                .changed(Command::SchemeKey(Left, WHEEL_UP))
                .is_err()
        );
        assert!(original.changed(Command::SchemeKey(Left, VK_L)).is_err());
        let custom = original
            .changed(Command::Shortcut(
                Shortcut::CloseWindow,
                Chord {
                    mods: MOD_WIN,
                    key: VK_H,
                },
            ))
            .unwrap();
        let error = custom.changed(Command::SchemeKey(Left, VK_H)).unwrap_err();
        assert!(
            error.contains(Shortcut::FocusLeft.label())
                && error.contains(Shortcut::CloseWindow.label())
        );
        assert_eq!(custom.scheme_key(Left), VK_LEFT);
        let duplicate = toml::from_str::<Settings>("scheme.left = 39").unwrap();
        assert!(duplicate.validated().is_err());
    }
}
