use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

use super::{Command, Modifier, SchemeKey, Settings};

// Wheel directions share the binding table with keyboard virtual keys,
// which occupy 0x00..=0xff.
pub(crate) const WHEEL_UP: u16 = 0x100;
pub(crate) const WHEEL_DOWN: u16 = 0x101;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Focus,
    Move,
    StartResize,
    Grow,
    Shrink,
    Finish,
    Quick,
    QuickReverse,
    Window,
    Desktop,
    DesktopMove,
}

macro_rules! shortcuts {
    ($( $name:ident, $label:literal, $group:ident, $key:ident; )+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum Shortcut { $( $name, )+ }

        impl Shortcut {
            pub const ALL: [Self; 50] = [$( Self::$name, )+];

            fn spec(self) -> (&'static str, Group, u16) {
                match self { $( Self::$name => ($label, Group::$group, $key), )+ }
            }
        }
    }
}

shortcuts! {
    FocusLeft,             "Выбрать окно слева",             Focus, VK_LEFT;
    FocusRight,            "Выбрать окно справа",            Focus, VK_RIGHT;
    FocusUp,               "Выбрать окно сверху",            Focus, VK_UP;
    FocusDown,             "Выбрать окно снизу",             Focus, VK_DOWN;
    MoveLeft,              "Переместить влево",              Move, VK_LEFT;
    MoveRight,             "Переместить вправо",             Move, VK_RIGHT;
    MoveUp,                "Переместить вверх",              Move, VK_UP;
    MoveDown,              "Переместить вниз",               Move, VK_DOWN;
    StartResize,           "Начать изменение размера",       StartResize, VK_RETURN;
    GrowLeft,              "Сдвинуть к левому краю",         Grow, VK_LEFT;
    GrowRight,             "Сдвинуть к правому краю",        Grow, VK_RIGHT;
    GrowUp,                "Сдвинуть к верхнему краю",       Grow, VK_UP;
    GrowDown,              "Сдвинуть к нижнему краю",        Grow, VK_DOWN;
    ShrinkLeft,            "Сдвинуть от левого края",        Shrink, VK_LEFT;
    ShrinkRight,           "Сдвинуть от правого края",       Shrink, VK_RIGHT;
    ShrinkUp,              "Сдвинуть от верхнего края",      Shrink, VK_UP;
    ShrinkDown,            "Сдвинуть от нижнего края",       Shrink, VK_DOWN;
    Confirm,               "Завершить изменение размера",    Finish, VK_RETURN;
    Cancel,                "Завершить (второе сочетание)",   Finish, VK_ESCAPE;
    QuickLeft,             "Сдвинуть к левому краю",         Quick, VK_LEFT;
    QuickRight,            "Сдвинуть к правому краю",        Quick, VK_RIGHT;
    QuickUp,               "Сдвинуть к верхнему краю",       Quick, VK_UP;
    QuickDown,             "Сдвинуть к нижнему краю",        Quick, VK_DOWN;
    QuickReverseLeft,      "Сдвинуть от левого края",        QuickReverse, VK_LEFT;
    QuickReverseRight,     "Сдвинуть от правого края",       QuickReverse, VK_RIGHT;
    QuickReverseUp,        "Сдвинуть от верхнего края",      QuickReverse, VK_UP;
    QuickReverseDown,      "Сдвинуть от нижнего края",       QuickReverse, VK_DOWN;
    CloseWindow,           "Закрыть окно",                   Window, VK_Q;
    ToggleMaximize,        "Развернуть или восстановить",    Window, VK_F;
    ToggleMosaic,          "Переключить мозаику",            Window, VK_Y;
    PreviousDesktop,       "Предыдущий стол",                Desktop, VK_LEFT;
    NextDesktop,           "Следующий стол",                 Desktop, VK_RIGHT;
    PreviousDesktopAlt,    "Предыдущий стол (вариант 2)",    Desktop, VK_UP;
    NextDesktopAlt,        "Следующий стол (вариант 2)",     Desktop, VK_DOWN;
    PreviousDesktopWheel,  "Предыдущий стол (вариант 3)",    Desktop, WHEEL_UP;
    NextDesktopWheel,      "Следующий стол (вариант 3)",     Desktop, WHEEL_DOWN;
    Desktop1,              "Рабочий стол 1",                 Desktop, VK_1;
    Desktop2,              "Рабочий стол 2",                 Desktop, VK_2;
    Desktop3,              "Рабочий стол 3",                 Desktop, VK_3;
    Desktop4,              "Рабочий стол 4",                 Desktop, VK_4;
    Desktop5,              "Рабочий стол 5",                 Desktop, VK_5;
    Desktop6,              "Рабочий стол 6",                 Desktop, VK_6;
    Desktop7,              "Рабочий стол 7",                 Desktop, VK_7;
    Desktop8,              "Рабочий стол 8",                 Desktop, VK_8;
    Desktop9,              "Рабочий стол 9",                 Desktop, VK_9;
    Desktop10,             "Рабочий стол 10",                Desktop, VK_0;
    MovePreviousDesktop,   "На предыдущий стол",             DesktopMove, VK_LEFT;
    MoveNextDesktop,       "На следующий стол",              DesktopMove, VK_RIGHT;
    MovePreviousDesktopAlt,"На предыдущий стол (вариант 2)", DesktopMove, VK_UP;
    MoveNextDesktopAlt,    "На следующий стол (вариант 2)",  DesktopMove, VK_DOWN;
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

    // These commands reserve their chord in both modes. Window close and
    // maximize also work in resize when no resize binding owns the chord.
    pub fn reserves_both_modes(self) -> bool {
        self == Self::ToggleMosaic || matches!(self.spec().1, Group::Desktop | Group::DesktopMove)
    }

    pub fn default_chord(self) -> Chord {
        self.inherited(&Settings::default())
    }

    fn shares_context(self, other: Self) -> bool {
        self.reserves_both_modes()
            || other.reserves_both_modes()
            || self.is_resize() == other.is_resize()
    }

    fn inherited(self, settings: &Settings) -> Chord {
        let (_, group, key) = self.spec();
        let base = settings.scheme.modifier(SchemeKey::Base);
        let grab = settings.scheme.modifier(SchemeKey::Grab);
        let desktop = settings.scheme.modifier(SchemeKey::Desktop);
        let quick = settings.scheme.modifier(SchemeKey::Resize);
        let direction = match key {
            VK_LEFT => Some(SchemeKey::Left),
            VK_RIGHT => Some(SchemeKey::Right),
            VK_UP => Some(SchemeKey::Up),
            VK_DOWN => Some(SchemeKey::Down),
            _ => None,
        };
        let mods = match group {
            Group::Focus | Group::StartResize | Group::Window => base,
            Group::Move => base | grab,
            Group::Grow | Group::Finish => 0,
            Group::Shrink => grab,
            Group::Quick => base | quick,
            Group::QuickReverse => base | quick | grab,
            Group::DesktopMove => base | desktop | grab,
            Group::Desktop if direction.is_some() => base | desktop,
            Group::Desktop => base,
        };
        let key = direction.map_or(key, |role| settings.scheme.key(role));
        Chord { mods, key }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingOwners {
    One(Shortcut),
    Both(Shortcut, Shortcut),
}

impl From<Shortcut> for BindingOwners {
    fn from(action: Shortcut) -> Self {
        Self::One(action)
    }
}

impl BindingOwners {
    pub(super) fn actions(self) -> impl Iterator<Item = Shortcut> {
        let (first, second) = match self {
            Self::One(a) => (a, None),
            Self::Both(a, b) => (a, Some(b)),
        };
        std::iter::once(first).chain(second)
    }

    pub(crate) fn labels(self) -> String {
        match self {
            Self::One(a) => format!("«{}»", a.label()),
            Self::Both(a, b) => format!("«{}» и «{}»", a.label(), b.label()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Chord {
    pub mods: u32,
    pub key: u16,
}

impl Chord {
    pub fn supports_key(key: u16) -> bool {
        // Accept bindable keyboard keys and the two vertical wheel directions.
        matches!(key,
            VK_CANCEL | VK_BACK | VK_TAB | VK_CLEAR | VK_RETURN | VK_PAUSE..=VK_9
            | VK_A..=VK_Z | VK_APPS | VK_SLEEP..=VK_F24 | VK_NUMLOCK..=VK_OEM_FJ_ROYA
            | VK_BROWSER_BACK..=VK_LAUNCH_APP2 | VK_OEM_1..=VK_ABNT_C2
            | VK_OEM_4..=VK_OEM_8 | VK_OEM_AX..=VK_ICO_00 | VK_ICO_CLEAR
            | VK_OEM_RESET..=VK_OEM_CLEAR | WHEEL_UP | WHEEL_DOWN
        )
    }

    pub fn key_label(key: u16) -> String {
        match key {
            VK_LWIN | VK_RWIN => "Win",
            VK_SHIFT | VK_LSHIFT | VK_RSHIFT => "Shift",
            VK_CONTROL | VK_LCONTROL | VK_RCONTROL => "Ctrl",
            VK_MENU | VK_LMENU | VK_RMENU => "Alt",
            // A binding stores a virtual key. Its legend stays the same when
            // the user switches input languages or edits a saved binding.
            VK_A..=VK_Z | VK_0..=VK_9 => return char::from(key as u8).to_string(),
            VK_F1..=VK_F24 => return format!("F{}", key - VK_F1 + 1),
            VK_NUMPAD0..=VK_NUMPAD9 => return format!("Num {}", key - VK_NUMPAD0),
            VK_LEFT => "Left",
            VK_RIGHT => "Right",
            VK_UP => "Up",
            VK_DOWN => "Down",
            WHEEL_UP => "Wheel Up",
            WHEEL_DOWN => "Wheel Down",
            VK_RETURN => "Enter",
            VK_ESCAPE => "Esc",
            VK_SPACE => "Space",
            VK_TAB => "Tab",
            VK_BACK => "Backspace",
            VK_DELETE => "Delete",
            VK_INSERT => "Insert",
            VK_HOME => "Home",
            VK_END => "End",
            VK_PRIOR => "Page Up",
            VK_NEXT => "Page Down",
            VK_ADD => "Num +",
            VK_SUBTRACT => "Num -",
            VK_MULTIPLY => "Num *",
            VK_DIVIDE => "Num /",
            VK_DECIMAL => "Num .",
            VK_SEPARATOR => "Num Separator",
            VK_APPS => "Menu",
            VK_SNAPSHOT => "Print Screen",
            VK_PAUSE => "Pause",
            VK_SCROLL => "Scroll Lock",
            VK_CAPITAL => "Caps Lock",
            VK_NUMLOCK => "Num Lock",
            VK_CANCEL => "Break",
            VK_CLEAR => "Clear",
            VK_SELECT => "Select",
            VK_PRINT => "Print",
            VK_EXECUTE => "Execute",
            VK_HELP => "Help",
            VK_SLEEP => "Sleep",
            VK_KANA => "Kana/Hangul",
            VK_IME_ON => "IME On",
            VK_IME_OFF => "IME Off",
            VK_JUNJA => "Junja",
            VK_FINAL => "Final",
            VK_HANJA => "Hanja/Kanji",
            VK_CONVERT => "Convert",
            VK_NONCONVERT => "Nonconvert",
            VK_ACCEPT => "Accept",
            VK_MODECHANGE => "Mode Change",
            VK_BROWSER_BACK => "Browser Back",
            VK_BROWSER_FORWARD => "Browser Forward",
            VK_BROWSER_REFRESH => "Browser Refresh",
            VK_BROWSER_STOP => "Browser Stop",
            VK_BROWSER_SEARCH => "Browser Search",
            VK_BROWSER_FAVORITES => "Browser Favorites",
            VK_BROWSER_HOME => "Browser Home",
            VK_VOLUME_MUTE => "Mute",
            VK_VOLUME_DOWN => "Volume Down",
            VK_VOLUME_UP => "Volume Up",
            VK_MEDIA_NEXT_TRACK => "Next Track",
            VK_MEDIA_PREV_TRACK => "Previous Track",
            VK_MEDIA_STOP => "Media Stop",
            VK_MEDIA_PLAY_PAUSE => "Play/Pause",
            VK_LAUNCH_MAIL => "Mail",
            VK_LAUNCH_MEDIA_SELECT => "Media Player",
            VK_LAUNCH_APP1 => "Launch App 1",
            VK_LAUNCH_APP2 => "Launch App 2",
            VK_OEM_1 => ";",
            VK_OEM_PLUS => "+",
            VK_OEM_COMMA => ",",
            VK_OEM_MINUS => "-",
            VK_OEM_PERIOD => ".",
            VK_OEM_2 => "/",
            VK_OEM_3 => "`",
            VK_OEM_4 => "[",
            VK_OEM_5 => "\\",
            VK_OEM_6 => "]",
            VK_OEM_7 => "'",
            VK_OEM_8 => "OEM 8",
            VK_OEM_102 => "ISO \\",
            VK_ABNT_C1 => "ABNT C1",
            VK_ABNT_C2 => "ABNT C2",
            VK_OEM_NEC_EQUAL => "Num =",
            VK_OEM_FJ_MASSHOU => "Dictionary Delete",
            VK_OEM_FJ_TOUROKU => "Dictionary Add",
            VK_OEM_FJ_LOYA => "Left OYAYUBI",
            VK_OEM_FJ_ROYA => "Right OYAYUBI",
            VK_OEM_AX => "AX",
            VK_ICO_HELP => "ICO Help",
            VK_ICO_00 => "ICO 00",
            VK_ICO_CLEAR => "ICO Clear",
            VK_OEM_RESET => "OEM Reset",
            VK_OEM_JUMP => "OEM Jump",
            VK_OEM_PA1 => "OEM PA1",
            VK_OEM_PA2 => "OEM PA2",
            VK_OEM_PA3 => "OEM PA3",
            VK_OEM_WSCTRL => "OEM WS Ctrl",
            VK_OEM_CUSEL => "OEM CuSel",
            VK_OEM_ATTN => "OEM Attn",
            VK_OEM_FINISH => "OEM Finish",
            VK_OEM_COPY => "OEM Copy",
            VK_OEM_AUTO => "OEM Auto",
            VK_OEM_ENLW => "OEM Enlw",
            VK_OEM_BACKTAB => "OEM BackTab",
            VK_ATTN => "Attn",
            VK_CRSEL => "CrSel",
            VK_EXSEL => "ExSel",
            VK_EREOF => "Erase EOF",
            VK_PLAY => "Play",
            VK_ZOOM => "Zoom",
            VK_NONAME => "NoName",
            VK_PA1 => "PA1",
            VK_OEM_CLEAR => "OEM Clear",
            _ => return format!("VK {key:02X}"),
        }
        .into()
    }

    pub fn modifier_labels(mods: u32) -> impl Iterator<Item = &'static str> {
        [
            (MOD_CONTROL, "Ctrl"),
            (MOD_WIN, "Win"),
            (MOD_ALT, "Alt"),
            (MOD_SHIFT, "Shift"),
        ]
        .into_iter()
        .filter_map(move |(bit, name)| (mods & bit != 0).then_some(name))
    }

    pub fn key_labels(mods: u32, key: Option<u16>) -> Vec<String> {
        Self::modifier_labels(mods)
            .map(str::to_owned)
            .chain(key.map(Self::key_label))
            .collect()
    }

    pub fn label(self) -> String {
        Self::key_labels(self.mods, Some(self.key)).join("+")
    }

    pub(crate) fn validate(self, action: Shortcut) -> Result<(), String> {
        if !Self::supports_key(self.key)
            || self.mods & !(MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN) != 0
        {
            return Err("Сочетание содержит неподдерживаемую клавишу или модификатор.".into());
        }
        if !action.is_resize()
            && self.mods & (MOD_CONTROL | MOD_ALT | MOD_WIN) == 0
            && !(VK_F1..=VK_F24).contains(&self.key)
            && !(VK_BROWSER_BACK..=VK_LAUNCH_APP2).contains(&self.key)
        {
            return Err("Добавьте Win, Ctrl или Alt либо выберите функциональную или мультимедийную клавишу.".into());
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "StoredOverrides", into = "BTreeMap<Shortcut, Binding>")]
pub(super) struct Overrides([Option<Binding>; Shortcut::ALL.len()]);

impl Default for Overrides {
    fn default() -> Self {
        Self([None; Shortcut::ALL.len()])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
enum Binding {
    #[serde(rename = "disabled")]
    Disabled,
    // Keep the existing { mods, key } representation for assigned chords.
    #[serde(untagged)]
    Assigned(Chord),
}

// Accept the retired setting independently, even if its chord is malformed.
// Keep normal validation for every other key and omit it when writing the file.
#[derive(Deserialize)]
struct StoredOverrides {
    #[serde(rename = "quit")]
    _legacy_quit: Option<serde::de::IgnoredAny>,
    #[serde(flatten)]
    bindings: BTreeMap<Shortcut, Binding>,
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

impl Settings {
    pub(super) fn import_legacy_bindings(
        &mut self,
        focus: Option<Modifier>,
        movement: Option<Modifier>,
        resize: Option<Modifier>,
    ) {
        let overrides = &mut self.shortcuts;
        // Older profiles may already assign the default chord of a new action.
        // Keep the user's binding and leave the newly introduced action unassigned.
        for action in Shortcut::ALL {
            if !matches!(
                action.spec().1,
                Group::Quick
                    | Group::QuickReverse
                    | Group::Window
                    | Group::Desktop
                    | Group::DesktopMove
            ) || overrides.0[action.index()].is_some()
            {
                continue;
            }
            let chord = Binding::Assigned(action.default_chord());
            if Shortcut::ALL.into_iter().any(|other| {
                action.shares_context(other) && overrides.0[other.index()] == Some(chord)
            }) {
                overrides.0[action.index()] = Some(Binding::Disabled);
            }
        }
        for action in Shortcut::ALL {
            if overrides.0[action.index()].is_some() {
                continue;
            }
            let (_, group, key) = action.spec();
            let legacy = match group {
                Group::Focus => focus,
                Group::Move => movement,
                Group::StartResize => resize,
                _ => None,
            };
            if let Some(modifier) = legacy {
                let chord = Chord {
                    mods: modifier.bits(),
                    key,
                };
                if chord != action.default_chord() {
                    overrides.0[action.index()] = Some(Binding::Assigned(chord));
                }
            }
        }
    }
}

impl From<Overrides> for BTreeMap<Shortcut, Binding> {
    fn from(overrides: Overrides) -> Self {
        Shortcut::ALL
            .into_iter()
            .filter_map(|action| overrides.0[action.index()].map(|chord| (action, chord)))
            .collect()
    }
}

impl Settings {
    pub fn with_resize_override(self, modifier: Modifier) -> Result<Self, String> {
        let chord = Chord {
            mods: modifier.bits(),
            key: VK_RETURN,
        };
        self.changed(Command::Shortcut(Shortcut::StartResize, chord))
    }

    pub fn binding(&self, action: Shortcut) -> Option<Chord> {
        match self.shortcuts.0[action.index()] {
            None => Some(action.inherited(self)),
            Some(Binding::Assigned(chord)) => Some(chord),
            Some(Binding::Disabled) => None,
        }
    }

    pub fn conflict(&self, action: Shortcut, chord: Chord) -> Option<BindingOwners> {
        let owner = |resize| {
            let context = Shortcut::ALL
                .into_iter()
                .filter(|other| other.reserves_both_modes() || other.is_resize() == resize);
            // Explicit bindings win over the variants that keep the resize opener held.
            context
                .clone()
                .find(|&other| self.binding(other) == Some(chord))
                .or_else(|| {
                    context
                        .into_iter()
                        .find(|&other| self.resize_alias(other, chord))
                })
                .filter(|&other| action != other)
        };
        if !action.reserves_both_modes() {
            return owner(action.is_resize()).map(BindingOwners::One);
        }
        match (owner(false), owner(true)) {
            (Some(a), Some(b)) if a != b => Some(BindingOwners::Both(a, b)),
            (one, other) => one.or(other).map(BindingOwners::One),
        }
    }

    pub(super) fn assign(&mut self, action: Shortcut, chord: Chord) -> Result<(), String> {
        chord.validate(action)?;
        if let Some(other) = self.conflict(action, chord) {
            return Err(format!(
                "{} уже назначено на {}.",
                chord.label(),
                other.labels()
            ));
        }
        self.set_assigned(action, chord);
        Ok(())
    }

    pub(super) fn transfer_confirmed(
        &mut self,
        action: Shortcut,
        chord: Chord,
        owners: BindingOwners,
    ) -> Result<(), String> {
        chord.validate(action)?;
        for owner in owners.actions() {
            self.shortcuts.0[owner.index()] = Some(Binding::Disabled);
        }
        self.set_assigned(action, chord);
        Ok(())
    }

    fn set_assigned(&mut self, action: Shortcut, chord: Chord) {
        self.shortcuts.0[action.index()] =
            (chord != action.inherited(self)).then_some(Binding::Assigned(chord));
    }

    // The original resize mode also accepts its default keys while the opener's
    // modifiers remain held. An explicit modal binding replaces those aliases.
    pub fn resize_alias(&self, action: Shortcut, input: Chord) -> bool {
        let (_, group, _) = action.spec();
        let key = action.inherited(self).key;
        if key != input.key || self.shortcuts.0[action.index()].is_some() {
            return false;
        }
        let base = self.scheme.modifier(SchemeKey::Base);
        let grab = self.scheme.modifier(SchemeKey::Grab);
        let opener = self.binding(Shortcut::StartResize).map(|chord| chord.mods);
        match group {
            Group::Grow | Group::Shrink => {
                let inverse = if group == Group::Shrink { grab } else { 0 };
                input.mods == base | inverse
                    || opener.is_some_and(|mods| input.mods == (mods & !grab) | inverse)
            }
            Group::Finish => {
                Some(input.mods) == opener || (action == Shortcut::Cancel && input.mods == base)
            }
            _ => false,
        }
    }

    pub(super) fn validate_shortcuts(&self) -> Result<(), String> {
        for (index, action) in Shortcut::ALL.into_iter().enumerate() {
            let Some(chord) = self.binding(action) else {
                continue;
            };
            chord.validate(action)?;
            for &other in &Shortcut::ALL[..index] {
                if action.shares_context(other) && self.binding(other) == Some(chord) {
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
