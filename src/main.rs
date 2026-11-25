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
use manager::Manager;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
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
