#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(windows)]
mod cli;
#[cfg(windows)]
mod cli_error;
#[cfg(windows)]
mod hud;
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
    if let Some(exit) = startup::consent::handle_helper(&args) {
        return exit;
    }
    // Explorer launches Rustile without a console. A command-line launch can
    // still print help and accept Ctrl+C from the existing parent console.
    if args
        .first()
        .is_none_or(|arg| !matches!(arg.as_str(), "--cli" | "--background"))
    {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
    native::init_dpi();
    match runtime::run_with_args(&args) {
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
