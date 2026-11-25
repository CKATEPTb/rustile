use crate::settings::{Binding, Command, Modifier, Settings};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Page {
    Main,
    Shortcuts,
    Binding(Binding),
    Outline,
    Gap,
    Padding,
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
            Self::Outline => "Толщина обводки",
            Self::Gap => "Между окнами",
            Self::Padding => "По краям экрана",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Action {
    Back,
    Open(Page),
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

fn measure(page: Page, n: u8) -> String {
    match (page, n) {
        (Page::Outline, 0) => "Выкл.".to_owned(),
        _ => format!("{n} px"),
    }
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
            for (page, glyph, n) in [
                (Page::Outline, "\u{e739}", s.outline),
                (Page::Gap, "\u{e8ab}", s.gap),
                (Page::Padding, "\u{e9a6}", s.padding),
            ] {
                let mut row =
                    Row::new(page.title(), glyph, Action::Open(page)).value(measure(page, n));
                row.separator = page == Page::Outline;
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
        Page::Outline | Page::Gap | Page::Padding => {
            let (choices, current): (&[u8], _) = match page {
                Page::Outline => (&[0, 1, 2, 3, 4], s.outline),
                Page::Gap => (&[0, 4, 8, 12, 16, 24, 32], s.gap),
                _ => (&[0, 4, 8, 12, 16, 24, 32], s.padding),
            };
            // A hand-edited setting can use any whole value in the supported range.
            let mut choices = choices.to_vec();
            if let Err(at) = choices.binary_search(&current) {
                choices.insert(at, current);
            }
            for n in choices {
                let command = match page {
                    Page::Outline => Command::Outline(n),
                    Page::Gap => Command::Gap(n),
                    _ => Command::Padding(n),
                };
                let mut row = Row::new(measure(page, n), "", Action::Apply(command));
                row.checked = n == current;
                rows.push(row);
            }
        }
    }
    rows
}
