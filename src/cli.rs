//! English command grammar for both terminal clients and the running manager.
//!
//! A client can print help locally, start the manager, or send a typed request.
//! Public aliases map to the same [`Command`]. Its canonical wire form lets
//! the server validate the request again before touching a window.

use crate::layout::{Direction, Id};
use crate::settings::Modifier;

const USAGE: &str = "Rustile window manager for Windows

rustile enable | disable | toggle  Enable, disable, or toggle tiling
rustile focus                      Print the focused window's title
rustile focus DIR                  Focus the neighboring tile
rustile move DIR                   Move the focused tile
rustile resize EDGE DIR            Move the selected edge of the focused tile
rustile close                      Close the focused window
rustile maximize                   Maximize or restore the focused window
rustile desktop                    Print the current desktop number
rustile desktop next|previous|N    Switch desktops (numbered from 1)
rustile transfer next|previous|N   Move the focused window and follow it
rustile desktop list               List desktops in Windows order
rustile window list                List windows on the current desktop
rustile [start]                    Start Rustile or connect to the running manager
rustile status                     Print the manager's status
rustile quit                       Shut down the manager
rustile --resize-shortcut win+enter|ctrl+alt+enter
rustile --help | --version

DIR is left, right, up, or down. For example, resize left right moves the left
edge to the right. Run rustile help resize for all edge and direction pairs.";

const FOCUS_HELP: &str = "rustile focus                     Print the focused window's title
rustile focus left|right|up|down  Focus the next tiled window in that direction";
const MOSAIC_HELP: &str = "rustile mosaic status           Print on or off
rustile mosaic on|off|toggle    Enable, disable, or toggle tiling";
const DESKTOP_HELP: &str = "rustile desktop                 Print the current desktop number
rustile desktop list            List desktops in Windows order
rustile desktop next|previous   Switch to the next or previous desktop
rustile desktop N               Switch to desktop N (numbered from 1)

If N exceeds the desktop count, Rustile switches to the last desktop.";
const TRANSFER_HELP: &str = "rustile transfer next|previous|N

Move the focused window to the destination and follow it. Desktop numbers start
at 1. Numbers above the desktop count select the last desktop.
On desktop 1, transfer previous creates a new first desktop for the window.
Without a focused window, transfer previous acts like desktop previous.";
const RESIZE_HELP: &str = "rustile resize left|right left|right
rustile resize top|bottom up|down

Move the selected tile edge one step in the given direction. The edge names
up and down also select the top and bottom edges. A screen boundary stays fixed.";
const WINDOW_HELP: &str =
    "rustile window current                   Print the focused window's title
rustile window list                      List windows on the current desktop
rustile window focus DIR                  Focus the neighboring window
rustile window move DIR                   Move the focused tile
rustile window resize DIR [--shrink]      Resize the tile by one step
rustile window to-desktop next|prev|N     Move the focused window and follow it
rustile window close|maximize             Close or maximize/restore the focused window

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
    Resize {
        direction: Direction,
        shrink: bool,
    },
    ResizeEdge {
        edge: Direction,
        direction: Direction,
    },
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
            "move" => Ok(Invocation::Print("rustile move left|right|up|down")),
            "resize" => Ok(Invocation::Print(RESIZE_HELP)),
            "transfer" => Ok(Invocation::Print(TRANSFER_HELP)),
            "enable" => Ok(Invocation::Print("rustile enable   Enable tiling")),
            "disable" => Ok(Invocation::Print("rustile disable  Disable tiling")),
            "toggle" => Ok(Invocation::Print("rustile toggle   Toggle tiling")),
            "close" => Ok(Invocation::Print(
                "rustile close   Close the focused window",
            )),
            "maximize" => Ok(Invocation::Print(
                "rustile maximize   Maximize or restore the focused window",
            )),
            "mosaic" => Ok(Invocation::Print(MOSAIC_HELP)),
            "desktop" => Ok(Invocation::Print(DESKTOP_HELP)),
            "window" => Ok(Invocation::Print(WINDOW_HELP)),
            _ => Err(format!("Unknown help topic '{group}'. See rustile --help.")),
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
        ["enable"] | ["mosaic", "on"] => Ok(Command::Mosaic(Mosaic::On)),
        ["disable"] | ["mosaic", "off"] => Ok(Command::Mosaic(Mosaic::Off)),
        ["toggle"] | ["mosaic", "toggle"] => Ok(Command::Mosaic(Mosaic::Toggle)),
        ["mosaic", "status"] => Ok(Command::Mosaic(Mosaic::Status)),
        ["mosaic", ..] => Err(MOSAIC_HELP.into()),
        ["desktop"] | ["desktop", "current"] => Ok(Command::DesktopCurrent),
        ["desktop", "list"] => Ok(Command::DesktopList),
        ["desktop", target] => destination(target).map(Command::DesktopGo),
        ["desktop", "go", n] => desktop_number(n).map(Command::DesktopGo),
        ["desktop", ..] => Err(DESKTOP_HELP.into()),
        ["focus"] | ["window", "current"] => Ok(Command::WindowCurrent),
        ["window", "list"] => Ok(Command::WindowList),
        ["window", args @ ..] => parse_window(args, true),
        [
            "focus" | "move" | "resize" | "transfer" | "close" | "maximize",
            ..,
        ] => parse_window(words, false),
        _ => Err("Unknown command or unexpected arguments. See rustile --help.".into()),
    }
}

fn parse_window(args: &[&str], grouped: bool) -> Result<Command, String> {
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
        ["resize", dir] if grouped => WindowAction::Resize {
            direction: direction(dir)?,
            shrink,
        },
        _ if shrink => return Err("Use --shrink only with window resize DIR.".into()),
        ["resize", edge, dir] => {
            let edge = match *edge {
                "top" => Direction::Up,
                "bottom" => Direction::Down,
                text => direction(text)?,
            };
            let direction = direction(dir)?;
            if !edge.same_axis(direction) {
                return Err("Horizontal movement requires a left or right edge. Vertical movement requires a top or bottom edge.".into());
            }
            WindowAction::ResizeEdge { edge, direction }
        }
        ["resize", ..] if !grouped => return Err(RESIZE_HELP.into()),
        ["focus", dir] => WindowAction::Focus(direction(dir)?),
        ["move", dir] => WindowAction::Move(direction(dir)?),
        ["close"] => WindowAction::Close,
        ["maximize"] => WindowAction::Maximize,
        ["transfer" | "to-desktop", target] => WindowAction::ToDesktop(destination(target)?),
        ["transfer", ..] => return Err(TRANSFER_HELP.into()),
        ["focus", ..] => return Err(FOCUS_HELP.into()),
        _ => return Err(WINDOW_HELP.into()),
    };
    Ok(Command::Window { action, hwnd })
}

fn destination(text: &str) -> Result<Destination, String> {
    match text {
        "next" => Ok(Destination::Next),
        "prev" | "previous" => Ok(Destination::Previous),
        n => desktop_number(n),
    }
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
            "Use a nonzero window handle in decimal or hexadecimal (e.g. 0x1234). See rustile window list."
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
                    WindowAction::Focus(d) => format!("window focus {}", dir(d)),
                    WindowAction::Move(d) => format!("window move {}", dir(d)),
                    WindowAction::Resize { direction, shrink } => format!(
                        "window resize {}{}",
                        dir(direction),
                        if shrink { " --shrink" } else { "" }
                    ),
                    WindowAction::ResizeEdge { edge, direction } => {
                        format!("resize {} {}", dir(edge), dir(direction))
                    }
                    WindowAction::ToDesktop(d) => format!("window to-desktop {}", target(d)),
                    WindowAction::Close => "window close".into(),
                    WindowAction::Maximize => "window maximize".into(),
                };
                match hwnd {
                    Some(id) => format!("{action} --hwnd {:#x}", id as usize),
                    None => action,
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
    fn flat_commands_preserve_the_existing_requests_and_wire_format() {
        for (input, canonical) in [
            ("enable", "mosaic on"),
            ("disable", "mosaic off"),
            ("toggle", "mosaic toggle"),
            ("focus up --hwnd 9", "window focus up --hwnd 0x9"),
            ("move right", "window move right"),
            ("move down --hwnd 8", "window move down --hwnd 0x8"),
            ("close", "window close"),
            ("maximize", "window maximize"),
            ("desktop", "desktop current"),
            ("desktop next", "desktop next"),
            ("desktop previous", "desktop prev"),
            ("desktop 5", "desktop go 5"),
            ("transfer next", "window to-desktop next"),
            ("transfer previous", "window to-desktop prev"),
            ("transfer 12 --hwnd 3", "window to-desktop 12 --hwnd 0x3"),
        ] {
            let Invocation::Request(cmd) = invocation(input).unwrap() else {
                panic!("{input}")
            };
            let wire = format!("Rustile/1 {canonical}");
            assert_eq!(cmd.to_wire(), wire, "{input}");
            assert_eq!(Command::from_wire(&wire), Ok(cmd), "{input}");
        }
    }

    #[test]
    fn edge_resize_addresses_a_specific_edge_and_motion() {
        use Direction::*;
        for (edge_name, edge, motion_name, direction) in [
            ("left", Left, "left", Left),
            ("left", Left, "right", Right),
            ("right", Right, "left", Left),
            ("right", Right, "right", Right),
            ("up", Up, "up", Up),
            ("top", Up, "down", Down),
            ("down", Down, "down", Down),
            ("bottom", Down, "up", Up),
        ] {
            let input = format!("resize {edge_name} {motion_name}");
            let action = WindowAction::ResizeEdge { edge, direction };
            let cmd = Command::Window { action, hwnd: None };
            assert_eq!(invocation(&input), Ok(Invocation::Request(cmd)));
            assert_eq!(Command::from_wire(&cmd.to_wire()), Ok(cmd));
            let cmd = Command::Window {
                action,
                hwnd: Some(16),
            };
            assert_eq!(
                invocation(&format!("{input} --hwnd 0x10")),
                Ok(Invocation::Request(cmd))
            );
            assert_eq!(Command::from_wire(&cmd.to_wire()), Ok(cmd));
        }
    }

    #[test]
    fn client_only_options_are_never_remote_requests() {
        assert_eq!(parse(&[]), Ok(Invocation::Start(None)));
        assert_eq!(invocation("start"), parse(&[]));
        for topic in [
            "enable", "disable", "toggle", "focus", "move", "resize", "close", "maximize",
            "desktop", "transfer", "mosaic", "window",
        ] {
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
            "desktop 0",
            "desktop -1",
            "transfer 0",
            "transfer prev next",
            "resize left",
            "resize left down",
            "resize top right",
            "resize bottom down --shrink",
            "resize up --shrink",
            "resize left right --hwnd",
            "transfer next --hwnd 1 --hwnd 2",
            "enable extra",
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
