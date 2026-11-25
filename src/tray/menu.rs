use crate::settings::{Command, Settings};

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
    EditShortcuts,
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
        name.encode_utf16().chain([0]).collect()
    }
}

pub(super) fn rows(s: Settings) -> Vec<Row> {
    let mut rows = vec![Row::new("Rustile", "", Action::None)];
    rows.push(
        Row::new("Мозаика", "\u{e8a9}", Action::Apply(Command::Toggle)).value(if s.enabled {
            "Включена"
        } else {
            "Выключена"
        }),
    );
    rows.push(Row::new(
        "Горячие клавиши",
        "\u{e765}",
        Action::EditShortcuts,
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
    rows
}
