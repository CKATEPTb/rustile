#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(windows)]
mod hud;
#[cfg(any(windows, test))]
mod layout;
#[cfg(windows)]
mod manager;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod runtime;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
use manager::Manager;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    // Explorer launches Rustile without a console. A command-line launch can
    // still print help and accept Ctrl+C from the existing parent console.
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    native::init_dpi();
    match runtime::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Rustile: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("Rustile requires Windows 11.");
    std::process::ExitCode::FAILURE
}
