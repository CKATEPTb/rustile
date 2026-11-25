use crate::settings::{Binding, Command, Modifier, Settings};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Page {
    Main,
    Shortcuts,
    Binding(Binding),
}

impl Page {
    pub(super) fn parent(self) -> Option<Self> {
        match self {
            Self::Main => None,
            Self::Binding(_) => Some(Self::Shortcuts),
            _ => Some(Self::Main),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Main => "Rustile",
            Self::Shortcuts => "Горячие клавиши",
            Self::Binding(b) => binding_label(b),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Measure {
    Outline,
    Gap,
    Padding,
}

impl Measure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Outline => "Толщина обводки",
            Self::Gap => "Между окнами",
            Self::Padding => "По краям экрана",
        }
    }

    pub fn value(self, settings: Settings) -> u8 {
        match self {
            Self::Outline => settings.outline,
            Self::Gap => settings.gap,
            Self::Padding => settings.padding,
        }
    }

    pub fn max(self) -> u8 {
        match self {
            Self::Outline => Settings::MAX_OUTLINE,
            Self::Gap | Self::Padding => Settings::MAX_SPACING,
        }
    }

    pub fn command(self, value: u8) -> Command {
        match self {
            Self::Outline => Command::Outline(value),
            Self::Gap => Command::Gap(value),
            Self::Padding => Command::Padding(value),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Action {
    Back,
    Open(Page),
    Adjust(Measure),
    Apply(Command),
    None,
}

#[derive(Clone)]
pub(super) struct Row {
    pub label: String,
    pub detail: String,
    pub glyph: &'static str,
    pub action: Action,
    pub checked: bool,
    pub enabled: bool,
    pub separator: bool,
}

impl Row {
    fn new(label: impl Into<String>, glyph: &'static str, action: Action) -> Self {
        Self {
            label: label.into(),
            detail: String::new(),
            glyph,
            enabled: !matches!(action, Action::None),
            action,
            checked: false,
            separator: false,
        }
    }

    fn value(mut self, value: impl Into<String>) -> Self {
        self.detail = value.into();
        self
    }

    fn group(mut self) -> Self {
        self.separator = true;
        self
    }

    pub fn accessible_name(&self) -> Vec<u16> {
        let mut name = self.label.clone();
        if !self.detail.is_empty() {
            name.push_str(", ");
            name.push_str(&self.detail);
        }
        if self.checked {
            name.push_str(", выбрано");
        }
        if matches!(self.action, Action::Back) {
            name.push_str(", назад");
        }
        name.encode_utf16().chain([0]).collect()
    }
}

fn binding_label(binding: Binding) -> &'static str {
    match binding {
        Binding::Focus => "Перевести фокус",
        Binding::Move => "Переместить окно",
        Binding::Resize => "Изменить размер",
    }
}

fn binding_value(settings: Settings, binding: Binding) -> Modifier {
    match binding {
        Binding::Focus => settings.focus,
        Binding::Move => settings.movement,
        Binding::Resize => settings.resize,
    }
}

fn chord(binding: Binding, modifier: Modifier) -> String {
    let key = if binding == Binding::Resize {
        "Enter"
    } else {
        "стрелки"
    };
    format!("{}+{key}", modifier.label())
}

pub(super) fn rows(page: Page, s: Settings) -> Vec<Row> {
    let mut rows = vec![Row::new(
        page.title(),
        "\u{e72b}",
        if page == Page::Main {
            Action::None
        } else {
            Action::Back
        },
    )];
    match page {
        Page::Main => {
            rows.push(
                Row::new("Мозаика", "\u{e8a9}", Action::Apply(Command::Toggle)).value(
                    if s.enabled {
                        "Включена"
                    } else {
                        "Выключена"
                    },
                ),
            );
            rows.push(Row::new(
                Page::Shortcuts.title(),
                "\u{e765}",
                Action::Open(Page::Shortcuts),
            ));
            for (measure, glyph) in [
                (Measure::Outline, "\u{e739}"),
                (Measure::Gap, "\u{e8ab}"),
                (Measure::Padding, "\u{e9a6}"),
            ] {
                let mut row = Row::new(measure.label(), glyph, Action::Adjust(measure))
                    .value(format!("{} px", measure.value(s)));
                row.separator = measure == Measure::Outline;
                rows.push(row);
            }
            rows.push(
                Row::new(
                    "Вернуть исходные позиции",
                    "\u{e7a7}",
                    Action::Apply(Command::Restore),
                )
                .group(),
            );
            rows.push(Row::new(
                "Выйти из Rustile",
                "\u{e7e8}",
                Action::Apply(Command::Exit),
            ));
        }
        Page::Shortcuts => {
            for b in [Binding::Focus, Binding::Move, Binding::Resize] {
                rows.push(
                    Row::new(binding_label(b), "", Action::Open(Page::Binding(b)))
                        .value(chord(b, binding_value(s, b))),
                );
            }
            rows.push(
                Row::new("Сдвиг границы", "", Action::None)
                    .value("Стрелки")
                    .group(),
            );
            rows.push(Row::new("Сужение со стороны", "", Action::None).value("Shift+стрелки"));
            rows.push(Row::new("Применить размер", "", Action::None).value("Enter"));
            rows.push(Row::new("Отменить изменение", "", Action::None).value("Esc"));
            rows.push(
                Row::new("Выйти из Rustile", "", Action::None)
                    .value("Ctrl+Alt+Shift+Q")
                    .group(),
            );
        }
        Page::Binding(b) => {
            for modifier in Modifier::ALL {
                let mut row = Row::new(
                    chord(b, modifier),
                    "",
                    Action::Apply(Command::Shortcut(b, modifier)),
                );
                row.checked = binding_value(s, b) == modifier;
                row.enabled = match b {
                    Binding::Focus => modifier != s.movement,
                    Binding::Move => modifier != s.focus,
                    Binding::Resize => true,
                };
                rows.push(row);
            }
        }
    }
    rows
}
