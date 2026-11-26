//! Rustile is a Windows 11 tiling window manager and its command-line client.
//!
//! Both roles share one executable. At startup it handles installation and
//! logon helpers first, then hands the invocation to the installed copy.
//! The long-lived process owns the Win32 event loop and the layout. Subsequent
//! clients can address that process without creating another manager.

#[cfg(windows)]
mod cli;
#[cfg(windows)]
mod cli_error;
#[cfg(windows)]
mod hud;
#[cfg(windows)]
mod i18n;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod launch_context;
#[cfg(any(windows, test))]
mod layout;
#[cfg(windows)]
mod manager;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod profile_io;
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
    // Both the consent helper and the scheduled GUI use only system DLLs.
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
        };
        if SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) == 0 {
            return std::process::ExitCode::FAILURE;
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let from_terminal = launch_context::launched_from_terminal();
    if let Some(exit) = install::handle_helper(&args) {
        return exit;
    }
    if let Some(exit) = startup::consent::handle_helper(&args) {
        return exit;
    }
    native::init_dpi();
    match install::launch(&args, from_terminal) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("Rustile: {message}");
            if !from_terminal {
                runtime::show_error(&message);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("Rustile requires Windows 11.");
    std::process::ExitCode::FAILURE
}
