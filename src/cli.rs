use crate::layout::{Direction, Id};
use crate::settings::Modifier;

const USAGE: &str = "Rustile window manager for Windows

rustilectl focus                            Print the focused window's title
rustilectl focus DIR                        Focus the neighboring window
rustilectl window list                      List windows on the current desktop
rustilectl window move DIR                  Move the focused tile
rustilectl window resize DIR [--shrink]     Resize the focused tile by one step
rustilectl window to-desktop next|prev|N    Move the focused window and follow it
rustilectl window close|maximize            Close or maximize/restore the focused window
rustilectl mosaic on|off|toggle|status      Control tiling or print its status
rustilectl desktop current|list|next|prev   Inspect or switch virtual desktops
rustilectl desktop go N                     Switch to desktop N (numbered from 1)
rustilectl [start]                          Start Rustile or connect to the running manager
rustilectl status                           Print the manager's status
rustilectl quit                             Shut down the manager
rustilectl --resize-shortcut win+enter|ctrl+alt+enter
rustilectl --help | --version

DIR is left, right, up, or down. Run rustilectl help COMMAND for details
on focus, window, desktop, or mosaic.";

const FOCUS_HELP: &str = "rustilectl focus                     Print the focused window's title
rustilectl focus left|right|up|down  Focus the next tiled window in that direction";
const MOSAIC_HELP: &str = "rustilectl mosaic status           Print on or off
rustilectl mosaic on|off|toggle    Enable, disable, or toggle tiling";
const DESKTOP_HELP: &str = "rustilectl desktop current         Print the current desktop number
rustilectl desktop list            List desktops in Windows order
rustilectl desktop next|prev       Switch to the next or previous desktop
rustilectl desktop go N            Switch to desktop N (numbered from 1)

If N exceeds the desktop count, Rustile switches to the last desktop.";
const WINDOW_HELP: &str =
    "rustilectl window current                   Print the focused window's title
rustilectl window list                      List windows on the current desktop
rustilectl window focus DIR                  Focus the neighboring window
rustilectl window move DIR                   Move the focused tile
rustilectl window resize DIR [--shrink]      Resize the tile by one step
rustilectl window to-desktop next|prev|N     Move the focused window and follow it
rustilectl window close|maximize             Close or maximize/restore the focused window

DIR is left, right, up, or down. Use --shrink to move the same boundary in reverse.
Window actions use the focused window by default. To target a specific window,
pass --hwnd ID from window list. For window focus, that window is the starting tile.";

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
    WindowCurrent,
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
            "focus" => Ok(Invocation::Print(FOCUS_HELP)),
            "mosaic" => Ok(Invocation::Print(MOSAIC_HELP)),
            "desktop" => Ok(Invocation::Print(DESKTOP_HELP)),
            "window" => Ok(Invocation::Print(WINDOW_HELP)),
            _ => Err(format!(
                "Unknown help topic '{group}'. See rustilectl --help."
            )),
        },
        ["--resize-shortcut", chord] | ["start", "--resize-shortcut", chord] => {
            let modifier = if chord.eq_ignore_ascii_case("win+enter") {
                Modifier::Win
            } else if chord.eq_ignore_ascii_case("ctrl+alt+enter") {
                Modifier::CtrlAlt
            } else {
                return Err("--resize-shortcut expects win+enter or ctrl+alt+enter.".into());
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
        ["focus"] | ["window", "current"] => Ok(Command::WindowCurrent),
        ["focus", dir] => Ok(Command::Window {
            action: WindowAction::Focus(direction(dir)?),
            hwnd: None,
        }),
        ["focus", ..] => Err(FOCUS_HELP.into()),
        ["window", "list"] => Ok(Command::WindowList),
        ["window", args @ ..] => parse_window(args),
        _ => Err("Unknown command or unexpected arguments. See rustilectl --help.".into()),
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
        _ if shrink => return Err("Only window resize accepts --shrink.".into()),
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
            "Unknown direction '{text}'. Use left, right, up, or down."
        )),
    }
}

fn desktop_number(text: &str) -> Result<Destination, String> {
    text.parse::<usize>()
        .ok()
        .filter(|&n| n > 0 && text.bytes().all(|c| c.is_ascii_digit()))
        .map(Destination::Number)
        .ok_or_else(|| "Desktop numbers must be positive integers.".into())
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
            "Use a nonzero window handle in decimal or hexadecimal (e.g. 0x1234). See rustilectl window list."
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
            Self::WindowCurrent => "window current".into(),
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
            .ok_or("Unsupported Rustile protocol version.")?;
        if body.is_empty()
            || body.len() > 128
            || !body.bytes().all(|c| c.is_ascii_graphic() || c == b' ')
        {
            return Err("Invalid Rustile command.".into());
        }
        let words: Vec<_> = body.split(' ').collect();
        if let ["resize-shortcut", bits] = words.as_slice() {
            let bits = bits.parse::<u32>().ok();
            return [Modifier::Win, Modifier::CtrlAlt]
                .into_iter()
                .find(|m| Some(m.bits()) == bits)
                .map(Command::ResizeShortcut)
                .ok_or_else(|| "Invalid resize shortcut.".into());
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
            "focus",
            "focus right",
            "window current",
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
    fn focus_commands_use_the_foreground_window_without_an_id() {
        assert_eq!(
            invocation("focus"),
            Ok(Invocation::Request(Command::WindowCurrent))
        );
        assert_eq!(invocation("window current"), invocation("focus"));
        assert_eq!(Command::WindowCurrent.to_wire(), "Rustile/1 window current");
        for (name, dir) in [
            ("left", Direction::Left),
            ("right", Direction::Right),
            ("up", Direction::Up),
            ("down", Direction::Down),
        ] {
            let cmd = Command::Window {
                action: WindowAction::Focus(dir),
                hwnd: None,
            };
            assert_eq!(
                invocation(&format!("focus {name}")),
                Ok(Invocation::Request(cmd))
            );
            assert_eq!(cmd.to_wire(), format!("Rustile/1 window focus {name}"));
        }
    }

    #[test]
    fn client_only_options_are_never_remote_requests() {
        assert_eq!(parse(&[]), Ok(Invocation::Start(None)));
        assert_eq!(invocation("start"), parse(&[]));
        for topic in ["mosaic", "desktop", "window", "focus"] {
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
            "window current --hwnd 7",
            "focus sideways",
            "focus left right",
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
