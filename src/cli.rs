use crate::layout::{Direction, Id};
use crate::settings::Modifier;

const USAGE: &str = "Rustile — управление мозаикой Windows

rustilectl [start]                         Запустить менеджер или обратиться к работающему
rustilectl status                          Показать состояние менеджера
rustilectl mosaic on|off|toggle|status      Управлять мозаикой
rustilectl desktop current|list|next|prev   Узнать или сменить рабочий стол
rustilectl desktop go N                    Перейти на стол по номеру
rustilectl window list                     Показать окна и их HWND
rustilectl window focus|move DIR           Перевести фокус или переместить плитку
rustilectl window resize DIR [--shrink]    Изменить размер плитки на один шаг
rustilectl window to-desktop next|prev|N    Перенести окно и перейти вместе с ним
rustilectl window close|maximize           Закрыть окно или развернуть/восстановить
rustilectl quit                            Завершить основной экземпляр
rustilectl --resize-shortcut win+enter|ctrl+alt+enter
rustilectl --help | --version

Направления DIR — left, right, up, down. Номера рабочих столов начинаются с 1.
К команде управления окном можно добавить --hwnd ID (например, 0x1234).
Справка по группе — rustilectl mosaic --help, rustilectl desktop --help, rustilectl window --help.";

const MOSAIC_HELP: &str = "rustilectl mosaic status           Узнать, включена ли мозаика
rustilectl mosaic on|off|toggle    Включить, выключить или переключить мозаику";
const DESKTOP_HELP: &str =
    "rustilectl desktop current         Вывести номер текущего рабочего стола
rustilectl desktop list            Перечислить рабочие столы в порядке Windows
rustilectl desktop next|prev       Перейти на соседний рабочий стол
rustilectl desktop go N            Перейти на стол N (нумерация с 1)

Если N больше числа столов, выбирается последний существующий стол.";
const WINDOW_HELP: &str = "rustilectl window list
rustilectl window focus left|right|up|down [--hwnd ID]
rustilectl window move left|right|up|down [--hwnd ID]
rustilectl window resize left|right|up|down [--shrink] [--hwnd ID]
rustilectl window to-desktop next|prev|N [--hwnd ID]
rustilectl window close|maximize [--hwnd ID]

По умолчанию команда действует на активное окно. Чтобы выбрать другое окно,
передайте его HWND из window list в десятичной записи или с префиксом 0x.
Для focus указанный HWND служит исходной плиткой при выборе соседа.
Для resize параметр --shrink меняет направление движения того же разделителя.";

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Invocation {
    Print(&'static str),
    Start(Option<Modifier>),
    Request(Command),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Command {
    Status,
    Mosaic(Mosaic),
    DesktopCurrent,
    DesktopList,
    DesktopGo(Destination),
    WindowList,
    Window {
        action: WindowAction,
        hwnd: Option<Id>,
    },
    ResizeShortcut(Modifier),
    Quit,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Mosaic {
    Status,
    On,
    Off,
    Toggle,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Destination {
    Next,
    Previous,
    Number(usize),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum WindowAction {
    Focus(Direction),
    Move(Direction),
    Resize { direction: Direction, shrink: bool },
    ToDesktop(Destination),
    Close,
    Maximize,
}

pub(crate) fn parse(args: &[String]) -> Result<Invocation, String> {
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["start"] => Ok(Invocation::Start(None)),
        ["--version" | "-V"] => Ok(Invocation::Print(concat!(
            "rustile ",
            env!("CARGO_PKG_VERSION")
        ))),
        ["--help" | "-h" | "help"] => Ok(Invocation::Print(USAGE)),
        ["help", group] | [group, "--help" | "-h"] => match *group {
            "mosaic" => Ok(Invocation::Print(MOSAIC_HELP)),
            "desktop" => Ok(Invocation::Print(DESKTOP_HELP)),
            "window" => Ok(Invocation::Print(WINDOW_HELP)),
            _ => Err(format!(
                "Неизвестная группа «{group}». См. rustilectl --help."
            )),
        },
        ["--resize-shortcut", chord] | ["start", "--resize-shortcut", chord] => {
            let modifier = if chord.eq_ignore_ascii_case("win+enter") {
                Modifier::Win
            } else if chord.eq_ignore_ascii_case("ctrl+alt+enter") {
                Modifier::CtrlAlt
            } else {
                return Err("Для --resize-shortcut укажите win+enter или ctrl+alt+enter.".into());
            };
            Ok(Invocation::Start(Some(modifier)))
        }
        _ => parse_command(&words).map(Invocation::Request),
    }
}

fn parse_command(words: &[&str]) -> Result<Command, String> {
    match words {
        ["status"] => Ok(Command::Status),
        ["quit"] => Ok(Command::Quit),
        ["mosaic", verb] => match *verb {
            "status" => Ok(Command::Mosaic(Mosaic::Status)),
            "on" => Ok(Command::Mosaic(Mosaic::On)),
            "off" => Ok(Command::Mosaic(Mosaic::Off)),
            "toggle" => Ok(Command::Mosaic(Mosaic::Toggle)),
            _ => Err(MOSAIC_HELP.into()),
        },
        ["mosaic", ..] => Err(MOSAIC_HELP.into()),
        ["desktop", "current"] => Ok(Command::DesktopCurrent),
        ["desktop", "list"] => Ok(Command::DesktopList),
        ["desktop", "next"] => Ok(Command::DesktopGo(Destination::Next)),
        ["desktop", "prev"] => Ok(Command::DesktopGo(Destination::Previous)),
        ["desktop", "go", n] => desktop_number(n).map(Command::DesktopGo),
        ["desktop", ..] => Err(DESKTOP_HELP.into()),
        ["window", "list"] => Ok(Command::WindowList),
        ["window", args @ ..] => parse_window(args),
        _ => Err("Неизвестная команда или лишние аргументы. См. rustilectl --help.".into()),
    }
}

fn parse_window(args: &[&str]) -> Result<Command, String> {
    let mut words = Vec::new();
    let mut hwnd = None;
    let mut shrink = false;
    let mut args = args.iter().copied();
    while let Some(arg) = args.next() {
        match arg {
            "--hwnd" if hwnd.is_none() => {
                hwnd = Some(window_id(args.next().unwrap_or_default())?);
            }
            "--shrink" if !shrink => shrink = true,
            _ => words.push(arg),
        }
    }
    let action = match words.as_slice() {
        ["resize", dir] => WindowAction::Resize {
            direction: direction(dir)?,
            shrink,
        },
        _ if shrink => return Err("Параметр --shrink применим только к window resize.".into()),
        ["focus", dir] => WindowAction::Focus(direction(dir)?),
        ["move", dir] => WindowAction::Move(direction(dir)?),
        ["close"] => WindowAction::Close,
        ["maximize"] => WindowAction::Maximize,
        ["to-desktop", target] => WindowAction::ToDesktop(match *target {
            "next" => Destination::Next,
            "prev" => Destination::Previous,
            n => desktop_number(n)?,
        }),
        _ => return Err(WINDOW_HELP.into()),
    };
    Ok(Command::Window { action, hwnd })
}

fn direction(text: &str) -> Result<Direction, String> {
    match text {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        _ => Err(format!(
            "Неизвестное направление «{text}». Укажите left, right, up или down."
        )),
    }
}

fn desktop_number(text: &str) -> Result<Destination, String> {
    text.parse::<usize>()
        .ok()
        .filter(|&n| n > 0 && text.bytes().all(|c| c.is_ascii_digit()))
        .map(Destination::Number)
        .ok_or_else(|| "Номер рабочего стола должен быть целым положительным числом.".into())
}

fn window_id(text: &str) -> Result<Id, String> {
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"));
    let parsed = match digits {
        Some(hex) if hex.bytes().all(|c| c.is_ascii_hexdigit()) => {
            usize::from_str_radix(hex, 16).ok()
        }
        None if text.bytes().all(|c| c.is_ascii_digit()) => text.parse().ok(),
        _ => None,
    };
    parsed
        .filter(|&id| id != 0)
        .map(|id| id as Id)
        .ok_or_else(|| {
            "Укажите ненулевой HWND в десятичной записи или в виде 0x1234. См. rustilectl window list."
                .into()
        })
}

impl Command {
    pub(crate) fn to_wire(self) -> String {
        let dir = |d| match d {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        };
        let target = |d| match d {
            Destination::Next => "next".into(),
            Destination::Previous => "prev".into(),
            Destination::Number(n) => n.to_string(),
        };
        let body = match self {
            Self::Status => "status".into(),
            Self::Quit => "quit".into(),
            Self::WindowList => "window list".into(),
            Self::DesktopList => "desktop list".into(),
            Self::DesktopCurrent => "desktop current".into(),
            Self::DesktopGo(d @ Destination::Number(_)) => format!("desktop go {}", target(d)),
            Self::DesktopGo(d) => format!("desktop {}", target(d)),
            Self::Mosaic(m) => format!(
                "mosaic {}",
                match m {
                    Mosaic::On => "on",
                    Mosaic::Off => "off",
                    Mosaic::Toggle => "toggle",
                    Mosaic::Status => "status",
                }
            ),
            Self::ResizeShortcut(modifier) => format!("resize-shortcut {}", modifier.bits()),
            Self::Window { action, hwnd } => {
                let action = match action {
                    WindowAction::Focus(d) => format!("focus {}", dir(d)),
                    WindowAction::Move(d) => format!("move {}", dir(d)),
                    WindowAction::Resize { direction, shrink } => format!(
                        "resize {}{}",
                        dir(direction),
                        if shrink { " --shrink" } else { "" }
                    ),
                    WindowAction::ToDesktop(d) => format!("to-desktop {}", target(d)),
                    WindowAction::Close => "close".into(),
                    WindowAction::Maximize => "maximize".into(),
                };
                match hwnd {
                    Some(id) => format!("window {action} --hwnd {:#x}", id as usize),
                    None => format!("window {action}"),
                }
            }
        };
        format!("Rustile/1 {body}")
    }

    pub(crate) fn from_wire(request: &str) -> Result<Self, String> {
        let body = request
            .strip_prefix("Rustile/1 ")
            .ok_or("Неподдерживаемая версия протокола Rustile.")?;
        if body.is_empty()
            || body.len() > 128
            || !body.bytes().all(|c| c.is_ascii_graphic() || c == b' ')
        {
            return Err("Некорректная команда Rustile.".into());
        }
        let words: Vec<_> = body.split(' ').collect();
        if let ["resize-shortcut", bits] = words.as_slice() {
            let bits = bits.parse::<u32>().ok();
            return [Modifier::Win, Modifier::CtrlAlt]
                .into_iter()
                .find(|m| Some(m.bits()) == bits)
                .map(Command::ResizeShortcut)
                .ok_or_else(|| "Недопустимое сочетание для изменения размера.".into());
        }
        parse_command(&words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(text: &str) -> Result<Invocation, String> {
        parse(
            &text
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn public_commands_round_trip_to_typed_requests() {
        for text in [
            "status",
            "mosaic on",
            "mosaic off",
            "mosaic toggle",
            "mosaic status",
            "desktop current",
            "desktop list",
            "desktop next",
            "desktop prev",
            "desktop go 15",
            "window list",
            "window move left",
            "window focus down --hwnd 537",
            "window --hwnd 0xFE resize right --shrink",
            "window resize up",
            "window to-desktop next",
            "window to-desktop prev",
            "window to-desktop 21",
            "window close --hwnd 0XF4",
            "window maximize",
            "quit",
        ] {
            let Invocation::Request(cmd) = invocation(text).unwrap() else {
                panic!("{text}")
            };
            assert_eq!(Command::from_wire(&cmd.to_wire()), Ok(cmd), "{text}");
        }
        assert_eq!(
            invocation("window --hwnd 0xfe resize right --shrink"),
            Ok(Invocation::Request(Command::Window {
                action: WindowAction::Resize {
                    direction: Direction::Right,
                    shrink: true
                },
                hwnd: Some(254)
            }))
        );
    }

    #[test]
    fn client_only_options_are_never_remote_requests() {
        assert_eq!(parse(&[]), Ok(Invocation::Start(None)));
        assert_eq!(invocation("start"), parse(&[]));
        for topic in ["mosaic", "desktop", "window"] {
            assert!(matches!(
                invocation(&format!("{topic} --help")),
                Ok(Invocation::Print(_))
            ));
            assert_eq!(
                invocation(&format!("help {topic}")),
                invocation(&format!("{topic} -h"))
            );
        }
        assert!(matches!(invocation("--version"), Ok(Invocation::Print(_))));
        for (key, m) in [
            ("Win+Enter", Modifier::Win),
            ("CTRL+ALT+ENTER", Modifier::CtrlAlt),
        ] {
            assert_eq!(
                invocation(&format!("--resize-shortcut {key}")),
                Ok(Invocation::Start(Some(m)))
            );
            let cmd = Command::ResizeShortcut(m);
            assert_eq!(Command::from_wire(&cmd.to_wire()), Ok(cmd));
        }
        for req in [
            "status",
            "Rustile/2 status",
            "Rustile/1 --help",
            "Rustile/1 start",
            "Rustile/1 status\nquit",
            "Rustile/1 quit 1",
            "Rustile/1 resize-shortcut 0",
        ] {
            assert!(Command::from_wire(req).is_err(), "{req}");
        }
    }

    #[test]
    fn invalid_targets_and_modifiers_are_rejected_before_dispatch() {
        for text in [
            "desktop go 0",
            "desktop go -1",
            "desktop go +1",
            "desktop go 123456789012345678901",
            "window to-desktop 0",
            "window move left --hwnd 0",
            "window move right --hwnd 0x",
            "window move right --hwnd 0x+1",
            "window focus down --hwnd -1",
            "window list --hwnd 7",
            "window close --hwnd 2 --hwnd 3",
            "window move right --shrink",
            "window resize right --shrink --shrink",
            "window resize sideways",
            "--resize-shortcut Alt+Enter",
            "mosaic on toggle",
        ] {
            assert!(invocation(text).is_err(), "{text}");
        }
    }
}
