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
fn launched_from_terminal() -> bool {
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType};
    use windows_sys::Win32::System::Console::{
        FreeConsole, GetConsoleProcessList, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    let mut pid = 0;
    let attached = unsafe { GetConsoleProcessList(&mut pid, 1) };
    let redirected = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].iter().any(|&stream| {
        matches!(
            unsafe { GetFileType(GetStdHandle(stream)) },
            FILE_TYPE_DISK | FILE_TYPE_PIPE
        )
    });
    // Earlier Windows builds may allocate a private console for an Explorer
    // launch. Release only that console; a shell or pipe belongs to the caller.
    if attached == 1 && !redirected {
        unsafe { FreeConsole() };
    }
    attached > 1 || redirected
}

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
    let from_terminal = launched_from_terminal();
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
