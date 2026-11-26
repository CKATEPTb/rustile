//! Launch information shared by the downloadable package and its inner executable.
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType};
use windows_sys::Win32::System::Console::{
    FreeConsole, GetConsoleProcessList, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};

/// The downloaded path can appear in an older per-user autostart entry.
/// This hint is only used to match that entry; it is never opened or executed.
pub(crate) const PACKAGE_ORIGIN: &str = "RUSTILE_PACKAGE_ORIGIN";

pub(crate) fn launched_from_terminal() -> bool {
    let mut pid = 0;
    let attached = unsafe { GetConsoleProcessList(&mut pid, 1) };
    let redirected = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].iter().any(|&stream| {
        matches!(
            unsafe { GetFileType(GetStdHandle(stream)) },
            FILE_TYPE_DISK | FILE_TYPE_PIPE
        )
    });
    // Earlier Windows builds can allocate a private console for an Explorer
    // launch. Release only that console; a shell or pipe belongs to the caller.
    if attached == 1 && !redirected {
        unsafe { FreeConsole() };
    }
    attached > 1 || redirected
}
