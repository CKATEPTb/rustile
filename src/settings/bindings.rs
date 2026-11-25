use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

use super::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Focus,
    Move,
    StartResize,
    Grow,
    Shrink,
    Finish,
}

macro_rules! shortcuts {
    ($( $name:ident, $label:literal, $group:ident, $key:ident; )+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum Shortcut { $( $name, )+ }

        impl Shortcut {
            pub const ALL: [Self; 19] = [$( Self::$name, )+];

            fn spec(self) -> (&'static str, Group, u16) {
                match self { $( Self::$name => ($label, Group::$group, $key), )+ }
            }
        }
    }
}

shortcuts! {
    FocusLeft,   "Фокус влево",        Focus, VK_LEFT;
    FocusRight,  "Фокус вправо",       Focus, VK_RIGHT;
    FocusUp,     "Фокус вверх",        Focus, VK_UP;
    FocusDown,   "Фокус вниз",         Focus, VK_DOWN;
    MoveLeft,    "Переместить влево",  Move, VK_LEFT;
    MoveRight,   "Переместить вправо", Move, VK_RIGHT;
    MoveUp,      "Переместить вверх",  Move, VK_UP;
    MoveDown,    "Переместить вниз",   Move, VK_DOWN;
    StartResize, "Войти в режим",      StartResize, VK_RETURN;
    GrowLeft,    "Расширить влево",    Grow, VK_LEFT;
    GrowRight,   "Расширить вправо",   Grow, VK_RIGHT;
    GrowUp,      "Расширить вверх",    Grow, VK_UP;
    GrowDown,    "Расширить вниз",     Grow, VK_DOWN;
    ShrinkLeft,  "Сузить слева",       Shrink, VK_LEFT;
    ShrinkRight, "Сузить справа",      Shrink, VK_RIGHT;
    ShrinkUp,    "Сузить сверху",      Shrink, VK_UP;
    ShrinkDown,  "Сузить снизу",       Shrink, VK_DOWN;
    Confirm,     "Применить размер",   Finish, VK_RETURN;
    Cancel,      "Отменить изменение", Finish, VK_ESCAPE;
}

impl Shortcut {
    pub fn label(self) -> &'static str {
        self.spec().0
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn is_resize(self) -> bool {
        matches!(self.spec().1, Group::Grow | Group::Shrink | Group::Finish)
    }

    fn shares_context(self, other: Self) -> bool {
        self.is_resize() == other.is_resize()
    }

    fn inherited(self, settings: &Settings) -> Chord {
        let (_, group, key) = self.spec();
        let mods = match group {
            Group::Focus => settings.focus.bits(),
            Group::Move => settings.movement.bits(),
            Group::StartResize => settings.resize.bits(),
            Group::Grow | Group::Finish => 0,
            Group::Shrink => MOD_SHIFT,
        };
        Chord { mods, key }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Chord {
    pub mods: u32,
    pub key: u16,
}

impl Chord {
    pub fn keys() -> &'static [u16] {
        &[
            VK_LEFT,
            VK_RIGHT,
            VK_UP,
            VK_DOWN,
            VK_RETURN,
            VK_ESCAPE,
            VK_SPACE,
            VK_TAB,
            VK_BACK,
            VK_DELETE,
            VK_INSERT,
            VK_HOME,
            VK_END,
            VK_PRIOR,
            VK_NEXT,
            VK_A,
            VK_B,
            VK_C,
            VK_D,
            VK_E,
            VK_F,
            VK_G,
            VK_H,
            VK_I,
            VK_J,
            VK_K,
            VK_L,
            VK_M,
            VK_N,
            VK_O,
            VK_P,
            VK_Q,
            VK_R,
            VK_S,
            VK_T,
            VK_U,
            VK_V,
            VK_W,
            VK_X,
            VK_Y,
            VK_Z,
            VK_0,
            VK_1,
            VK_2,
            VK_3,
            VK_4,
            VK_5,
            VK_6,
            VK_7,
            VK_8,
            VK_9,
            VK_F1,
            VK_F2,
            VK_F3,
            VK_F4,
            VK_F5,
            VK_F6,
            VK_F7,
            VK_F8,
            VK_F9,
            VK_F10,
            VK_F11,
            VK_F12,
            VK_F13,
            VK_F14,
            VK_F15,
            VK_F16,
            VK_F17,
            VK_F18,
            VK_F19,
            VK_F20,
            VK_F21,
            VK_F22,
            VK_F23,
            VK_F24,
            VK_NUMPAD0,
            VK_NUMPAD1,
            VK_NUMPAD2,
            VK_NUMPAD3,
            VK_NUMPAD4,
            VK_NUMPAD5,
            VK_NUMPAD6,
            VK_NUMPAD7,
            VK_NUMPAD8,
            VK_NUMPAD9,
            VK_ADD,
            VK_SUBTRACT,
            VK_MULTIPLY,
            VK_DIVIDE,
            VK_DECIMAL,
        ]
    }

    pub fn key_label(key: u16) -> String {
        match key {
            VK_A..=VK_Z | VK_0..=VK_9 => return char::from(key as u8).to_string(),
            VK_F1..=VK_F24 => return format!("F{}", key - VK_F1 + 1),
            VK_NUMPAD0..=VK_NUMPAD9 => return format!("Num {}", key - VK_NUMPAD0),
            VK_LEFT => "←",
            VK_RIGHT => "→",
            VK_UP => "↑",
            VK_DOWN => "↓",
            VK_RETURN => "Enter",
            VK_ESCAPE => "Esc",
            VK_SPACE => "Пробел",
            VK_TAB => "Tab",
            VK_BACK => "Backspace",
            VK_DELETE => "Delete",
            VK_INSERT => "Insert",
            VK_HOME => "Home",
            VK_END => "End",
            VK_PRIOR => "Page Up",
            VK_NEXT => "Page Down",
            VK_ADD => "Num +",
            VK_SUBTRACT => "Num −",
            VK_MULTIPLY => "Num *",
            VK_DIVIDE => "Num /",
            VK_DECIMAL => "Num .",
            _ => return format!("VK {key:02X}"),
        }
        .into()
    }

    pub fn modifier_labels(mods: u32) -> impl Iterator<Item = &'static str> {
        [
            (MOD_CONTROL, "Ctrl"),
            (MOD_ALT, "Alt"),
            (MOD_WIN, "Win"),
            (MOD_SHIFT, "Shift"),
        ]
        .into_iter()
        .filter_map(move |(bit, name)| (mods & bit != 0).then_some(name))
    }

    pub fn label(self) -> String {
        let mut text = String::new();
        for modifier in Self::modifier_labels(self.mods) {
            text.push_str(modifier);
            text.push('+');
        }
        text.push_str(&Self::key_label(self.key));
        text
    }

    fn validate(self, action: Shortcut) -> Result<(), String> {
        if !Self::keys().contains(&self.key)
            || self.mods & !(MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN) != 0
        {
            return Err("Сочетание содержит неподдерживаемую клавишу или модификатор.".into());
        }
        if !action.is_resize()
            && self.mods & (MOD_CONTROL | MOD_ALT | MOD_WIN) == 0
            && !(VK_F1..=VK_F24).contains(&self.key)
        {
            return Err("Добавьте Win, Ctrl или Alt либо выберите функциональную клавишу.".into());
        }
        let reserved = match self.key {
            VK_X | VK_L => self.mods == MOD_WIN,
            VK_TAB => self.mods & !MOD_SHIFT == MOD_ALT || self.mods == MOD_WIN,
            VK_DELETE => self.mods == (MOD_CONTROL | MOD_ALT),
            _ => false,
        };
        if reserved {
            return Err(format!(
                "Сочетание {} зарезервировано Windows.",
                self.label()
            ));
        }
        Ok(())
    }
}

// In memory, a fixed table keeps the hook allocation-free. The file contains
// only overrides, keyed by action name so adding an action preserves old files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "StoredOverrides", into = "BTreeMap<Shortcut, Chord>")]
pub(super) struct Overrides([Option<Chord>; Shortcut::ALL.len()]);

// Accept the retired setting independently, even if its chord is malformed.
// Keep normal validation for every other key and omit it when writing the file.
#[derive(Deserialize)]
struct StoredOverrides {
    #[serde(rename = "quit")]
    _legacy_quit: Option<serde::de::IgnoredAny>,
    #[serde(flatten)]
    bindings: BTreeMap<Shortcut, Chord>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }
}

impl From<StoredOverrides> for Overrides {
    fn from(stored: StoredOverrides) -> Self {
        let mut overrides = Self::default();
        for (action, chord) in stored.bindings {
            overrides.0[action.index()] = Some(chord);
        }
        overrides
    }
}

impl From<Overrides> for BTreeMap<Shortcut, Chord> {
    fn from(overrides: Overrides) -> Self {
        Shortcut::ALL
            .into_iter()
            .filter_map(|action| overrides.0[action.index()].map(|chord| (action, chord)))
            .collect()
    }
}

impl Settings {
    pub fn chord(&self, action: Shortcut) -> Chord {
        self.shortcuts.0[action.index()].unwrap_or_else(|| action.inherited(self))
    }

    pub(super) fn assign(&mut self, action: Shortcut, chord: Chord) {
        self.shortcuts.0[action.index()] = (chord != action.inherited(self)).then_some(chord);
    }

    // The original resize mode also accepts its default keys while the opener's
    // modifiers remain held. An explicit modal binding replaces those aliases.
    pub fn resize_alias(&self, action: Shortcut, input: Chord) -> bool {
        let (_, group, key) = action.spec();
        if key != input.key || self.shortcuts.0[action.index()].is_some() {
            return false;
        }
        let opener = self.chord(Shortcut::StartResize).mods;
        match group {
            Group::Grow | Group::Shrink => {
                let shift = if group == Group::Shrink { MOD_SHIFT } else { 0 };
                input.mods == MOD_WIN | shift || input.mods == (opener & !MOD_SHIFT) | shift
            }
            Group::Finish => {
                input.mods == opener || (action == Shortcut::Cancel && input.mods == MOD_WIN)
            }
            _ => false,
        }
    }

    pub(super) fn validate_shortcuts(&self) -> Result<(), String> {
        for (index, action) in Shortcut::ALL.into_iter().enumerate() {
            let chord = self.chord(action);
            chord.validate(action)?;
            for &other in &Shortcut::ALL[..index] {
                if action.shares_context(other) && self.chord(other) == chord {
                    return Err(format!(
                        "{} уже назначено на «{}» и «{}».",
                        chord.label(),
                        other.label(),
                        action.label()
                    ));
                }
            }
        }
        Ok(())
    }
}
