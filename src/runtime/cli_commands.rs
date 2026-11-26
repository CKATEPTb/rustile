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
        "Проводник Windows не сообщил текущий рабочий стол или порядок столов.".into()
    } else {
        "Управление виртуальными столами недоступно на этом рабочем столе или сборке Windows."
            .into()
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
                .ok_or("Окно недоступно для переноса на другой рабочий стол.")?;
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
            Err("Windows не приняла команду для этого окна на текущем рабочем столе.".into())
        };
    }
    if !mosaic {
        return Err("Мозаика выключена. Для этой команды включите её через mosaic on.".into());
    }
    if !manager.cli_tile_available(id) || !still_target() {
        return Err("Целевое окно недоступно или не входит в мозаику текущего стола.".into());
    }
    let applied = match action {
        WindowAction::Focus(dir) => manager.focus_neighbor(id, dir, still_target),
        WindowAction::Move(dir) => manager.move_window(id, dir),
        WindowAction::Resize { direction, shrink } => manager.quick_resize(id, direction, shrink),
        _ => unreachable!(),
    };
    if applied {
        Ok(Outcome::Done("ok".into()))
    } else {
        Err("Не удалось выполнить команду в этом направлении в текущей раскладке.".into())
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
                preferences.apply(manager, hook, tray, Command::Toggle)?;
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
        CliCommand::WindowList => window_list(manager),
        CliCommand::Window { action, hwnd } => {
            return window_command(manager, workspaces, preferences.live.enabled, action, hwnd);
        }
        CliCommand::ResizeShortcut(modifier) => {
            let updated = preferences.live.with_resize_override(modifier)?;
            manager.apply_settings_with(updated.clone(), || Ok(()))?;
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
