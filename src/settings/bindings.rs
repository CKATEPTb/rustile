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
    Quick,
    QuickReverse,
    Window,
}

macro_rules! shortcuts {
    ($( $name:ident, $label:literal, $group:ident, $key:ident; )+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum Shortcut { $( $name, )+ }

        impl Shortcut {
            pub const ALL: [Self; 29] = [$( Self::$name, )+];

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
    GrowLeft,    "Влево",              Grow, VK_LEFT;
    GrowRight,   "Вправо",             Grow, VK_RIGHT;
    GrowUp,      "Вверх",              Grow, VK_UP;
    GrowDown,    "Вниз",               Grow, VK_DOWN;
    ShrinkLeft,  "Влево (обратно)",    Shrink, VK_LEFT;
    ShrinkRight, "Вправо (обратно)",   Shrink, VK_RIGHT;
    ShrinkUp,    "Вверх (обратно)",    Shrink, VK_UP;
    ShrinkDown,  "Вниз (обратно)",     Shrink, VK_DOWN;
    Confirm,     "Выйти из режима",    Finish, VK_RETURN;
    Cancel,      "Выйти из режима (доп.)", Finish, VK_ESCAPE;
    QuickLeft,         "Влево",              Quick, VK_LEFT;
    QuickRight,        "Вправо",             Quick, VK_RIGHT;
    QuickUp,           "Вверх",              Quick, VK_UP;
    QuickDown,         "Вниз",               Quick, VK_DOWN;
    QuickReverseLeft,  "Влево (обратно)",    QuickReverse, VK_LEFT;
    QuickReverseRight, "Вправо (обратно)",   QuickReverse, VK_RIGHT;
    QuickReverseUp,    "Вверх (обратно)",    QuickReverse, VK_UP;
    QuickReverseDown,  "Вниз (обратно)",     QuickReverse, VK_DOWN;
    CloseWindow,    "Закрыть окно",              Window, VK_Q;
    ToggleMaximize, "Развернуть или восстановить", Window, VK_F;
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

    pub fn default_chord(self) -> Chord {
        self.inherited(&Settings::default())
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
            Group::Quick => MOD_WIN | MOD_ALT,
            Group::QuickReverse => MOD_WIN | MOD_ALT | MOD_SHIFT,
            Group::Window => MOD_WIN,
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
    pub fn supports_key(key: u16) -> bool {
        // Keyboard virtual keys only: mouse, controller, modifier, Unicode packet,
        // IME processing markers and unassigned virtual keys never form a chord.
        matches!(key,
            VK_CANCEL | VK_BACK | VK_TAB | VK_CLEAR | VK_RETURN | VK_PAUSE..=VK_9
            | VK_A..=VK_Z | VK_APPS | VK_SLEEP..=VK_F24 | VK_NUMLOCK..=VK_OEM_FJ_ROYA
            | VK_BROWSER_BACK..=VK_LAUNCH_APP2 | VK_OEM_1..=VK_ABNT_C2
            | VK_OEM_4..=VK_OEM_8 | VK_OEM_AX..=VK_ICO_00 | VK_ICO_CLEAR
            | VK_OEM_RESET..=VK_OEM_CLEAR
        )
    }

    pub fn key_label(key: u16) -> String {
        match key {
            VK_A..=VK_Z | VK_0..=VK_9 => {
                return layout_key_name(key).unwrap_or_else(|| char::from(key as u8).to_string());
            }
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
            VK_SEPARATOR => "Num Separator",
            VK_APPS => "Меню",
            VK_SNAPSHOT => "Print Screen",
            VK_PAUSE => "Pause",
            VK_SCROLL => "Scroll Lock",
            VK_CAPITAL => "Caps Lock",
            VK_NUMLOCK => "Num Lock",
            VK_BROWSER_BACK => "Браузер назад",
            VK_BROWSER_FORWARD => "Браузер вперёд",
            VK_BROWSER_REFRESH => "Обновить страницу",
            VK_BROWSER_STOP => "Остановить загрузку",
            VK_BROWSER_SEARCH => "Поиск в браузере",
            VK_BROWSER_FAVORITES => "Избранное",
            VK_BROWSER_HOME => "Домашняя страница",
            VK_VOLUME_MUTE => "Без звука",
            VK_VOLUME_DOWN => "Тише",
            VK_VOLUME_UP => "Громче",
            VK_MEDIA_NEXT_TRACK => "Следующий трек",
            VK_MEDIA_PREV_TRACK => "Предыдущий трек",
            VK_MEDIA_STOP => "Стоп",
            VK_MEDIA_PLAY_PAUSE => "Пуск / пауза",
            VK_LAUNCH_MAIL => "Почта",
            VK_LAUNCH_MEDIA_SELECT => "Медиаплеер",
            VK_LAUNCH_APP1 => "Приложение 1",
            VK_LAUNCH_APP2 => "Приложение 2",
            _ => return layout_key_name(key).unwrap_or_else(|| format!("VK {key:02X}")),
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

    pub fn label(self) -> String {
        let mut text = String::new();
        for modifier in Self::modifier_labels(self.mods) {
            text.push_str(modifier);
            text.push('+');
        }
        text.push_str(&Self::key_label(self.key));
        text
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

// Called by the settings UI for a key legend. Use a neutral synthetic state
// and the editor's active layout, without consuming or setting any dead key.
fn layout_key_name(key: u16) -> Option<String> {
    let layout = unsafe { GetKeyboardLayout(0) };
    let scan = unsafe { MapVirtualKeyExW(key.into(), MAPVK_VK_TO_VSC_EX, layout) };
    let mut buf = [0u16; 64];
    if matches!(key, VK_A..=VK_Z | VK_0..=VK_9 | VK_OEM_1..=VK_ABNT_C2 | VK_OEM_4..=VK_OEM_8 | VK_OEM_102)
    {
        let len = unsafe {
            ToUnicodeEx(
                key.into(),
                scan,
                [0u8; 256].as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as i32,
                4,
                layout,
            )
        };
        if len != 0 {
            // For dead keys Windows returns a negative value of unspecified
            // magnitude and places the spacing form in the first UTF-16 unit.
            let used = if len < 0 { 1 } else { len as usize };
            let text = String::from_utf16_lossy(&buf[..used]);
            if let Some(c) = text.chars().next()
                && !c.is_control()
                && !c.is_whitespace()
            {
                let mut upper = c.to_uppercase();
                let first = upper.next()?;
                return Some(if upper.next().is_none() {
                    first.to_string()
                } else {
                    c.to_string()
                });
            }
        }
    }
    if scan == 0 {
        return None;
    }
    let extended = i32::from(scan >> 8 == 0xe0) << 24;
    let param = ((scan as i32 & 0xff) << 16) | extended;
    let len = unsafe { GetKeyNameTextW(param, buf.as_mut_ptr(), buf.len() as i32) };
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

// In memory, a fixed table keeps the hook allocation-free. The file contains
// only overrides, keyed by action name so adding an action preserves old files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "StoredOverrides", into = "BTreeMap<Shortcut, Binding>")]
pub(super) struct Overrides([Option<Binding>; Shortcut::ALL.len()]);

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
        // Older profiles may already assign the default chord of a new action.
        // Keep the user's binding and leave the newly introduced action unassigned.
        for action in Shortcut::ALL {
            if !matches!(
                action.spec().1,
                Group::Quick | Group::QuickReverse | Group::Window
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
        overrides
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
    pub fn binding(&self, action: Shortcut) -> Option<Chord> {
        match self.shortcuts.0[action.index()] {
            None => Some(action.inherited(self)),
            Some(Binding::Assigned(chord)) => Some(chord),
            Some(Binding::Disabled) => None,
        }
    }

    pub fn conflict(&self, action: Shortcut, chord: Chord) -> Option<Shortcut> {
        let context = Shortcut::ALL
            .into_iter()
            .filter(|&other| action.shares_context(other));
        // Explicit bindings win over the variants that keep the resize opener held.
        let owner = context
            .clone()
            .find(|&other| self.binding(other) == Some(chord))
            .or_else(|| {
                context
                    .into_iter()
                    .find(|&other| self.resize_alias(other, chord))
            });
        owner.filter(|&other| action != other)
    }

    pub(super) fn assign(&mut self, action: Shortcut, chord: Chord) -> Result<(), String> {
        chord.validate(action)?;
        if let Some(other) = self.conflict(action, chord) {
            return Err(format!(
                "{} уже назначено на «{}».",
                chord.label(),
                other.label()
            ));
        }
        self.set_assigned(action, chord);
        Ok(())
    }

    pub(super) fn transfer_confirmed(
        &mut self,
        action: Shortcut,
        chord: Chord,
        owner: Shortcut,
    ) -> Result<(), String> {
        chord.validate(action)?;
        self.shortcuts.0[owner.index()] = Some(Binding::Disabled);
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
        let (_, group, key) = action.spec();
        if key != input.key || self.shortcuts.0[action.index()].is_some() {
            return false;
        }
        let opener = self.binding(Shortcut::StartResize).map(|chord| chord.mods);
        match group {
            Group::Grow | Group::Shrink => {
                let shift = if group == Group::Shrink { MOD_SHIFT } else { 0 };
                input.mods == MOD_WIN | shift
                    || opener.is_some_and(|mods| input.mods == (mods & !MOD_SHIFT) | shift)
            }
            Group::Finish => {
                Some(input.mods) == opener || (action == Shortcut::Cancel && input.mods == MOD_WIN)
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
