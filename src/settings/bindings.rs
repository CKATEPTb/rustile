use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

use super::{Modifier, OWNER_CHANGED, SchemeKey, Settings};

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
    DesktopMoveNumber,
}

macro_rules! shortcuts {
    ($( $name:ident, $label:literal, $group:ident, $key:ident; )+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum Shortcut { $( $name, )+ }

        impl Shortcut {
            pub const ALL: [Self; 60] = [$( Self::$name, )+];

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
    ToggleMosaic,          "Включить или выключить мозаику", Window, VK_Y;
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
    MoveDesktop1,          "На рабочий стол 1",              DesktopMoveNumber, VK_1;
    MoveDesktop2,          "На рабочий стол 2",              DesktopMoveNumber, VK_2;
    MoveDesktop3,          "На рабочий стол 3",              DesktopMoveNumber, VK_3;
    MoveDesktop4,          "На рабочий стол 4",              DesktopMoveNumber, VK_4;
    MoveDesktop5,          "На рабочий стол 5",              DesktopMoveNumber, VK_5;
    MoveDesktop6,          "На рабочий стол 6",              DesktopMoveNumber, VK_6;
    MoveDesktop7,          "На рабочий стол 7",              DesktopMoveNumber, VK_7;
    MoveDesktop8,          "На рабочий стол 8",              DesktopMoveNumber, VK_8;
    MoveDesktop9,          "На рабочий стол 9",              DesktopMoveNumber, VK_9;
    MoveDesktop10,         "На рабочий стол 10",             DesktopMoveNumber, VK_0;
}

impl Shortcut {
    pub const fn canonical(self) -> Self {
        use Shortcut::*;
        match self {
            PreviousDesktopAlt | PreviousDesktopWheel => PreviousDesktop,
            NextDesktopAlt | NextDesktopWheel => NextDesktop,
            MovePreviousDesktopAlt => MovePreviousDesktop,
            MoveNextDesktopAlt => MoveNextDesktop,
            action => action,
        }
    }

    pub fn is_editable(self) -> bool {
        self == self.canonical() && !matches!(self, Self::Confirm | Self::Cancel)
    }

    fn legacy_slots(self) -> impl Iterator<Item = Self> {
        use Shortcut::*;
        let others: &[Shortcut] = match self {
            PreviousDesktop => &[PreviousDesktopAlt, PreviousDesktopWheel],
            NextDesktop => &[NextDesktopAlt, NextDesktopWheel],
            MovePreviousDesktop => &[MovePreviousDesktopAlt],
            MoveNextDesktop => &[MoveNextDesktopAlt],
            _ => &[],
        };
        std::iter::once(self).chain(others.iter().copied())
    }

    pub fn factory_bindings(self) -> Vec<Chord> {
        self.inherited_bindings(&Settings::default())
    }

    fn inherited_bindings(self, settings: &Settings) -> Vec<Chord> {
        self.legacy_slots()
            .map(|slot| slot.inherited(settings))
            .collect()
    }

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
        self == Self::ToggleMosaic
            || matches!(
                self.spec().1,
                Group::Desktop | Group::DesktopMove | Group::DesktopMoveNumber
            )
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
            Group::Move | Group::DesktopMoveNumber => base | grab,
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
    pub(crate) fn actions(self) -> impl Iterator<Item = Shortcut> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Assignment {
    Add(Shortcut, Chord),
    Edit(Shortcut, Chord, Chord),
    Reset(Shortcut),
}

impl Assignment {
    pub fn action(self) -> Shortcut {
        match self {
            Self::Add(a, _) | Self::Edit(a, _, _) | Self::Reset(a) => a,
        }
    }

    fn requested(self) -> Vec<Chord> {
        match self {
            Self::Add(_, chord) | Self::Edit(_, _, chord) => vec![chord],
            Self::Reset(action) => action.factory_bindings(),
        }
    }
}

// A single edit requests one chord. Only a factory reset can request several
// simultaneously, and the widest factory set is the three desktop bindings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BindingConflicts([Option<(Chord, BindingOwners)>; 3]);

impl BindingConflicts {
    pub fn iter(self) -> impl Iterator<Item = (Chord, BindingOwners)> {
        self.0.into_iter().flatten()
    }

    pub fn is_empty(self) -> bool {
        self.0[0].is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Overrides(Option<Arc<BTreeMap<Shortcut, Vec<Chord>>>>);

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    fn get(&self, action: Shortcut) -> Option<&[Chord]> {
        self.0.as_ref()?.get(&action).map(Vec::as_slice)
    }

    fn set(&mut self, action: Shortcut, keys: Option<Vec<Chord>>) {
        if let Some(keys) = keys {
            Arc::make_mut(self.0.get_or_insert_with(Arc::default)).insert(action, keys);
        } else if let Some(map) = &mut self.0 {
            Arc::make_mut(map).remove(&action);
            if map.is_empty() {
                self.0 = None;
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Disabled {
    Disabled,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum StoredBinding {
    Disabled(Disabled),
    Single(Chord),
    List(Vec<Chord>),
}

impl StoredBinding {
    fn keys(&self) -> &[Chord] {
        match self {
            Self::Disabled(_) => &[],
            Self::Single(chord) => std::slice::from_ref(chord),
            Self::List(chords) => chords,
        }
    }
}

#[derive(Default, Deserialize)]
pub(super) struct StoredOverrides {
    #[serde(rename = "quit")]
    _quit: Option<serde::de::IgnoredAny>,
    #[serde(rename = "confirm")]
    _confirm: Option<serde::de::IgnoredAny>,
    #[serde(rename = "cancel")]
    _cancel: Option<serde::de::IgnoredAny>,
    #[serde(flatten)]
    entries: BTreeMap<Shortcut, StoredBinding>,
}

impl Serialize for Overrides {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        for (action, chords) in self.0.iter().flat_map(|map| map.iter()) {
            // An array denotes the entire logical action, including its former
            // alternate slots. A legacy singleton for a desktop is just one slot.
            if action.legacy_slots().count() > 1 || chords.len() > 1 {
                map.serialize_entry(action, chords)?;
            } else if let [chord] = chords.as_slice() {
                map.serialize_entry(action, chord)?;
            } else {
                map.serialize_entry(action, &Disabled::Disabled)?;
            }
        }
        map.end()
    }
}

impl Settings {
    pub(super) fn import_bindings(
        &mut self,
        stored: StoredOverrides,
        legacy_groups: bool,
        focus: Option<Modifier>,
        movement: Option<Modifier>,
        resize: Option<Modifier>,
    ) {
        let explicit = &stored.entries;
        let missing_transfer = |action: Shortcut| {
            matches!(action.spec().1, Group::DesktopMoveNumber) && !explicit.contains_key(&action)
        };
        for action in Shortcut::ALL.into_iter().filter(|a| a.is_editable()) {
            // Install new defaults only after loading both modes and all old
            // scheme roles. Until then a digit might still belong to either.
            if missing_transfer(action) {
                self.set_bindings(action, Vec::new());
                continue;
            }
            let mut chords = Vec::new();
            if let Some(StoredBinding::List(all)) = explicit.get(&action) {
                chords.extend_from_slice(all);
            } else {
                for slot in action.legacy_slots() {
                    if let Some(value) = explicit.get(&slot) {
                        chords.extend_from_slice(value.keys());
                        continue;
                    }
                    let (_, group, _) = slot.spec();
                    let modifier = match group {
                        Group::Focus => focus,
                        Group::Move => movement,
                        Group::StartResize => resize,
                        _ => None,
                    };
                    let mut chord = slot.inherited(self);
                    if legacy_groups && let Some(modifier) = modifier {
                        chord.mods = modifier.bits();
                    }
                    // Older profiles could already use newly introduced defaults.
                    // Omit only that slot, preserving the other desktop alternatives.
                    if legacy_groups
                        && matches!(
                            group,
                            Group::Quick
                                | Group::QuickReverse
                                | Group::Window
                                | Group::Desktop
                                | Group::DesktopMove
                        )
                        && explicit.iter().any(|(&other, value)| {
                            other.canonical() != action
                                && action.shares_context(other)
                                && value.keys().contains(&chord)
                        })
                    {
                        continue;
                    }
                    chords.push(chord);
                }
            }
            if action.is_resize() || action.reserves_both_modes() {
                chords.retain(|c| !c.fixed_exit());
            }
            self.set_bindings(action, chords);
        }
        for action in Shortcut::ALL.into_iter().filter(|&a| missing_transfer(a)) {
            let chord = action.inherited(self);
            if self.conflict(action, chord).is_none() {
                self.set_bindings(action, vec![chord]);
            }
        }
    }

    pub fn bindings(&self, action: Shortcut) -> Vec<Chord> {
        if action != action.canonical() {
            return Vec::new();
        }
        self.shortcuts
            .get(action)
            .map_or_else(|| action.inherited_bindings(self), <[Chord]>::to_vec)
    }

    pub(super) fn set_bindings(&mut self, action: Shortcut, chords: Vec<Chord>) {
        // If an old profile uses this digit as a direction, the same key can
        // also be a held-opener resize alias. Keep its explicit transfer even
        // when it happens to match the new default.
        let override_needed = chords != action.inherited_bindings(self)
            || (matches!(action.spec().1, Group::DesktopMoveNumber)
                && [
                    SchemeKey::Left,
                    SchemeKey::Right,
                    SchemeKey::Up,
                    SchemeKey::Down,
                ]
                .iter()
                .any(|&role| self.scheme.key(role) == action.spec().2));
        self.shortcuts
            .set(action, override_needed.then_some(chords));
    }

    pub fn with_resize_override(&self, modifier: Modifier) -> Result<Self, String> {
        let action = Shortcut::StartResize;
        let chord = Chord {
            mods: modifier.bits(),
            key: VK_RETURN,
        };
        let keys = self.bindings(action);
        if keys.contains(&chord) {
            return Ok(self.clone());
        }
        let assignment = keys.first().map_or(Assignment::Add(action, chord), |&old| {
            Assignment::Edit(action, old, chord)
        });
        self.changed(super::Command::Assign(assignment))
    }

    fn owns(&self, action: Shortcut, chord: Chord) -> bool {
        self.shortcuts.get(action).map_or_else(
            || {
                action
                    .legacy_slots()
                    .any(|slot| slot.inherited(self) == chord)
            },
            |chords| chords.contains(&chord),
        )
    }

    pub fn conflict(&self, action: Shortcut, chord: Chord) -> Option<BindingOwners> {
        let owner = |resize| {
            let context = Shortcut::ALL.into_iter().filter(|&other| {
                other == other.canonical()
                    && (other.reserves_both_modes() || other.is_resize() == resize)
            });
            context
                .clone()
                .find(|&other| self.owns(other, chord))
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

    pub fn conflicts(&self, assignment: Assignment) -> Result<BindingConflicts, String> {
        let action = assignment.action();
        if !action.is_editable() {
            return Err("Это системное сочетание Rustile.".into());
        }
        let current = self.bindings(action);
        if let Assignment::Edit(_, old, _) = assignment
            && !current.contains(&old)
        {
            return Err(OWNER_CHANGED.into());
        }
        let mut result = BindingConflicts::default();
        for chord in assignment.requested() {
            chord.validate(action)?;
            if (action.is_resize() || action.reserves_both_modes()) && chord.fixed_exit() {
                return Err("Enter и Esc завершают изменение размера.".into());
            }
            if current.contains(&chord) {
                match assignment {
                    Assignment::Reset(_) => continue,
                    Assignment::Edit(_, old, _) if old == chord => continue,
                    _ => return Err("Это сочетание уже назначено этому действию.".into()),
                }
            }
            if let Some(owners) = self.conflict(action, chord) {
                let slot = result
                    .0
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .expect("factory chord count");
                *slot = Some((chord, owners));
            }
        }
        Ok(result)
    }

    pub(super) fn apply_assignment(
        &mut self,
        assignment: Assignment,
        conflicts: BindingConflicts,
    ) -> Result<(), String> {
        let actual = self.conflicts(assignment)?;
        if actual != conflicts {
            if conflicts.is_empty()
                && let Some((chord, owners)) = actual.iter().next()
            {
                return Err(format!(
                    "{} уже назначено на {}.",
                    chord.label(),
                    owners.labels()
                ));
            }
            return Err(OWNER_CHANGED.into());
        }
        for (chord, owners) in conflicts.iter() {
            for owner in owners.actions() {
                // An explicit chord takes priority over an inherited held-opener
                // alias. Transferring an alias must preserve the owner's real keys.
                if self.owns(owner, chord) {
                    self.remove_chord(owner, chord)?;
                }
            }
        }
        let action = assignment.action();
        let mut chords = self.bindings(action);
        match assignment {
            Assignment::Add(_, chord) => chords.push(chord),
            Assignment::Edit(_, old, new) => {
                *chords
                    .iter_mut()
                    .find(|c| **c == old)
                    .ok_or(OWNER_CHANGED)? = new;
            }
            Assignment::Reset(_) => chords = action.factory_bindings(),
        }
        self.set_bindings(action, chords);
        Ok(())
    }

    pub(super) fn remove_chord(&mut self, action: Shortcut, old: Chord) -> Result<(), String> {
        if !action.is_editable() {
            return Err("Это системное сочетание Rustile.".into());
        }
        let mut keys = self.bindings(action);
        let index = keys.iter().position(|c| *c == old).ok_or(OWNER_CHANGED)?;
        keys.remove(index);
        self.set_bindings(action, keys);
        Ok(())
    }

    // The command line may temporarily replace the first resize opener, even
    // if the stored action was empty. Map just that slot when it is edited.
    pub(super) fn stored_source(
        &self,
        live: &Self,
        action: Shortcut,
        chord: Chord,
    ) -> Option<Chord> {
        if self.owns(action, chord) {
            return Some(chord);
        }
        if action == Shortcut::StartResize {
            let visible = live.bindings(action);
            if visible.first() == Some(&chord) {
                return self
                    .bindings(action)
                    .first()
                    .copied()
                    .filter(|c| !visible.contains(c));
            }
        }
        None
    }

    pub(super) fn copy_confirmed_bindings(
        &mut self,
        live: &Self,
        assignment: Assignment,
        confirmed: BindingConflicts,
    ) -> Result<(), String> {
        let action = assignment.action();
        for chord in assignment.requested() {
            if let Some(stored) = self.conflict(action, chord) {
                let allowed = confirmed.iter().find(|(c, _)| *c == chord);
                if stored.actions().any(|owner| {
                    !allowed.is_some_and(|(_, owners)| owners.actions().any(|a| a == owner))
                }) {
                    return Err(OWNER_CHANGED.into());
                }
            }
        }
        live.changed(super::Command::Transfer(assignment, confirmed))?;
        for (chord, owners) in confirmed.iter() {
            for owner in owners.actions() {
                if let Some(stored) = self.stored_source(live, owner, chord) {
                    self.remove_chord(owner, stored)?;
                }
            }
        }
        let mut saved = self.bindings(action);
        match assignment {
            Assignment::Add(_, chord) => {
                if !saved.contains(&chord) {
                    saved.push(chord);
                }
            }
            Assignment::Edit(_, old, new) if old != new => {
                let index = self
                    .stored_source(live, action, old)
                    .and_then(|c| saved.iter().position(|s| *s == c));
                if let Some(i) = index {
                    saved.remove(i);
                }
                if !saved.contains(&new) {
                    saved.insert(index.unwrap_or(0), new);
                }
            }
            Assignment::Edit(..) => {}
            Assignment::Reset(_) => saved = action.factory_bindings(),
        }
        self.set_bindings(action, saved);
        Ok(())
    }

    pub fn resize_alias(&self, action: Shortcut, input: Chord) -> bool {
        let (_, group, _) = action.spec();
        if !matches!(group, Group::Grow | Group::Shrink | Group::Finish) {
            return false;
        }
        let inherited = action.inherited(self);
        if inherited.key != input.key || !self.owns(action, inherited) {
            return false;
        }
        let base = self.scheme.modifier(SchemeKey::Base);
        let grab = self.scheme.modifier(SchemeKey::Grab);
        let openers = self.bindings(Shortcut::StartResize);
        match group {
            Group::Grow | Group::Shrink => {
                let inverse = if group == Group::Shrink { grab } else { 0 };
                input.mods == base | inverse
                    || openers
                        .iter()
                        .any(|c| input.mods == (c.mods & !grab) | inverse)
            }
            Group::Finish => {
                openers.iter().any(|c| c.mods == input.mods)
                    || (action == Shortcut::Cancel && input.mods == base)
            }
            _ => false,
        }
    }

    pub(super) fn validate_shortcuts(&self) -> Result<(), String> {
        let mut modes = [BTreeMap::new(), BTreeMap::new()];
        for action in Shortcut::ALL.into_iter().filter(|a| *a == a.canonical()) {
            for chord in self.bindings(action) {
                chord.validate(action)?;
                for (mode, used) in modes.iter_mut().enumerate() {
                    if (action.reserves_both_modes() || action.is_resize() == (mode == 1))
                        && let Some(other) = used.insert(chord, action)
                    {
                        return Err(format!(
                            "{} уже назначено на «{}» и «{}».",
                            chord.label(),
                            other.label(),
                            action.label()
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Chord {
    fn fixed_exit(self) -> bool {
        self.mods == 0 && matches!(self.key, VK_RETURN | VK_ESCAPE)
    }
}

#[cfg(test)]
impl Settings {
    pub(crate) fn binding(&self, action: Shortcut) -> Option<Chord> {
        self.bindings(action).first().copied()
    }

    fn edit_primary(&self, action: Shortcut, chord: Chord) -> Assignment {
        self.binding(action)
            .map_or(Assignment::Add(action, chord), |old| {
                Assignment::Edit(action, old, chord)
            })
    }

    pub(super) fn assign_primary(&self, action: Shortcut, chord: Chord) -> Result<Self, String> {
        self.changed(super::Command::Assign(self.edit_primary(action, chord)))
    }

    pub(super) fn transfer_primary(
        &self,
        action: Shortcut,
        chord: Chord,
        owners: BindingOwners,
    ) -> Result<Self, String> {
        self.changed(super::Command::Transfer(
            self.edit_primary(action, chord),
            BindingConflicts([Some((chord, owners)), None, None]),
        ))
    }
}
