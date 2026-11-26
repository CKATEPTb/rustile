use super::*;
use crate::cli::{Command as CliCommand, Destination, Mosaic, WindowAction};
use std::fmt::Write;

pub(super) enum Outcome {
    Done(String),
    Navigate {
        step: DesktopStep,
        explicit_window: bool,
    },
    Quit,
}

impl From<Destination> for DesktopDestination {
    fn from(destination: Destination) -> Self {
        match destination {
            Destination::Next => Self::Relative { next: true },
            Destination::Previous => Self::Relative { next: false },
            Destination::Number(n) => Self::Number(n),
        }
    }
}

fn enabled(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn desktop_error(workspaces: &dynamic_desktops::Workspaces) -> String {
    if workspaces.supported {
        "Windows Explorer did not report the current desktop or desktop order.".into()
    } else {
        "Virtual desktop management is unavailable on this desktop or Windows build.".into()
    }
}

fn desktop_status(
    workspaces: &dynamic_desktops::Workspaces,
) -> Result<(usize, Vec<DesktopId>), String> {
    let (current, order) = workspaces
        .snapshot()
        .ok_or_else(|| desktop_error(workspaces))?;
    let index = order
        .iter()
        .position(|&id| id == current)
        .ok_or_else(|| desktop_error(workspaces))?;
    Ok((index + 1, order))
}

fn desktop_table(current: usize, order: &[DesktopId]) -> String {
    let mut lines = String::from("INDEX\tCURRENT\tID");
    for (index, id) in order.iter().enumerate() {
        let number = index + 1;
        let mark = if number == current { "*" } else { "" };
        write!(lines, "\n{number}\t{mark}\t{:032x}", id.0).unwrap();
    }
    lines
}

fn window_list(manager: &Manager) -> String {
    let mut lines = String::from("HWND\tPID\tTITLE");
    for id in manager.current_control_targets() {
        let pid = native::pid(id);
        let title = native::window_title(id).replace(char::is_control, " ");
        if pid != 0 && native::pid(id) == pid {
            write!(lines, "\n0x{:X}\t{pid}\t{title}", id as usize).unwrap();
        }
    }
    lines
}

fn focused_title(
    manager: &Manager,
    workspaces: &dynamic_desktops::Workspaces,
) -> Result<String, String> {
    let id = native::foreground();
    let desktop = workspaces
        .current_desktop()
        .or_else(|| manager.layout_desktop());
    let state = desktop.map_or_else(
        || native::desktop_foreground(id),
        |d| manager.desktop_foreground_on(id, d),
    );
    if state != native::DesktopForeground::Preserve {
        return Err("No window is focused.".into());
    }
    let pid = native::pid(id);
    let title = native::window_title(id).replace(char::is_control, " ");
    if pid == 0 || native::foreground() != id || native::pid(id) != pid {
        return Err("The focused window changed. Try again.".into());
    }
    Ok(if title.trim().is_empty() {
        format!("Untitled window (PID {pid})")
    } else {
        title
    })
}

fn window_command(
    manager: &mut Manager,
    workspaces: &dynamic_desktops::Workspaces,
    mosaic: bool,
    action: WindowAction,
    hwnd: Option<isize>,
) -> Result<Outcome, String> {
    let id = hwnd.unwrap_or_else(native::foreground);
    let still_target = || hwnd.is_some() || native::foreground() == id;
    if let WindowAction::ToDesktop(destination) = action {
        let current = workspaces
            .current_desktop()
            .ok_or_else(|| desktop_error(workspaces))?;
        let window = if hwnd.is_none()
            && native::desktop_foreground(id) == native::DesktopForeground::Vacant
        {
            None
        } else {
            let window = TransferIdentity::capture(id)
                .filter(|_| manager.transferable_on(id, current) && still_target())
                .ok_or("This window cannot be moved to another desktop.")?;
            Some(Rc::new(window))
        };
        return Ok(Outcome::Navigate {
            step: DesktopStep {
                destination: destination.into(),
                window,
            },
            explicit_window: hwnd.is_some(),
        });
    }
    if matches!(action, WindowAction::Close | WindowAction::Maximize) {
        let control = if action == WindowAction::Close {
            WindowControl::Close
        } else {
            WindowControl::ToggleMaximize
        };
        return if manager.request_window_control(id, control, still_target) {
            // The app can veto WM_CLOSE or defer its maximize/restore request.
            Ok(Outcome::Done("requested".into()))
        } else {
            Err("Windows rejected the command for this window on the current desktop.".into())
        };
    }
    if !mosaic {
        return Err("Tiling is off. Run rustilectl enable to turn it on.".into());
    }
    if !manager.cli_tile_available(id) || !still_target() {
        return Err("The target window is unavailable or is not tiled on this desktop.".into());
    }
    let (applied, error) = match action {
        WindowAction::Focus(dir) => (
            manager.focus_neighbor(id, dir, still_target),
            "Could not focus a neighboring tile in that direction.",
        ),
        WindowAction::Move(dir) => (
            manager.move_window(id, dir),
            "The tile cannot move in that direction.",
        ),
        WindowAction::Resize { direction, shrink } => (
            manager.quick_resize(id, direction, shrink),
            "The tile cannot resize any further in that direction.",
        ),
        WindowAction::ResizeEdge { edge, direction } => (
            manager.resize_edge_step(id, edge, direction),
            "The selected edge cannot move in that direction.",
        ),
        _ => unreachable!(),
    };
    if applied {
        Ok(Outcome::Done("ok".into()))
    } else {
        Err(error.into())
    }
}

/// All Shell, window layout and settings mutations run serially on the owner
/// thread. Desktop steps join the same switch/focus queue as keyboard commands.
pub(super) fn execute(
    command: CliCommand,
    manager: &mut Manager,
    workspaces: &dynamic_desktops::Workspaces,
    preferences: &mut Preferences<'_>,
    hook: &shortcuts::Hook,
    tray: &mut Tray,
) -> Result<Outcome, String> {
    let response = match command {
        CliCommand::Status => {
            let mut status = format!(
                "pid={}\nmosaic={}",
                std::process::id(),
                enabled(preferences.live.enabled)
            );
            match desktop_status(workspaces) {
                Ok((current, order)) => {
                    write!(status, "\ndesktop={current}\ndesktops={}", order.len()).unwrap()
                }
                Err(_) => status.push_str("\ndesktop=unavailable\ndesktops=unavailable"),
            }
            status
        }
        CliCommand::Mosaic(verb) => {
            let before = preferences.live.enabled;
            let wanted = match verb {
                Mosaic::On => true,
                Mosaic::Off => false,
                Mosaic::Toggle => !before,
                Mosaic::Status => before,
            };
            if before != wanted {
                preferences.apply(manager, hook, tray, Command::Toggle)
                    .map_err(|_| "Could not apply or save the tiling setting. Check your Rustile settings file.")?;
            }
            enabled(preferences.live.enabled).to_owned()
        }
        CliCommand::DesktopCurrent => desktop_status(workspaces)?.0.to_string(),
        CliCommand::DesktopList => {
            let (current, order) = desktop_status(workspaces)?;
            desktop_table(current, &order)
        }
        CliCommand::DesktopGo(destination) => {
            if !workspaces.available() {
                return Err(desktop_error(workspaces));
            }
            return Ok(Outcome::Navigate {
                step: DesktopStep {
                    destination: destination.into(),
                    window: None,
                },
                explicit_window: false,
            });
        }
        CliCommand::WindowCurrent => focused_title(manager, workspaces)?,
        CliCommand::WindowList => window_list(manager),
        CliCommand::Window { action, hwnd } => {
            return window_command(manager, workspaces, preferences.live.enabled, action, hwnd);
        }
        CliCommand::ResizeShortcut(modifier) => {
            let updated = preferences.live.with_resize_override(modifier).map_err(
                |_| "Could not assign the resize shortcut. Check for a conflicting binding.",
            )?;
            manager
                .apply_settings_with(updated.clone(), || Ok(()))
                .map_err(|_| "Could not apply the resize shortcut.")?;
            hook.set_settings(&updated);
            tray.set_settings(updated.clone());
            // Just like an override passed at startup, it lasts for this session.
            preferences.live = updated;
            "ok".into()
        }
        CliCommand::Quit => return Ok(Outcome::Quit),
    };
    Ok(Outcome::Done(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_list_preserves_shell_order_and_marks_the_one_based_current_index() {
        let order = [0xaff, 0x123, 0xde].map(DesktopId);
        assert_eq!(
            desktop_table(2, &order),
            "INDEX\tCURRENT\tID\n1\t\t00000000000000000000000000000aff\n2\t*\t00000000000000000000000000000123\n3\t\t000000000000000000000000000000de"
        );
    }
}
