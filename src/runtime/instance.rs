//! Elect one manager for a Windows session's window station and Win32 desktop.
//! Explorer virtual desktops share that manager. A separate installation mutex
//! serializes competing replacements of the installed executable.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, WAIT_ABANDONED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::StationsAndDesktops::{GetProcessWindowStation, GetThreadDesktop};
use windows_sys::Win32::System::Threading::{
    CREATE_MUTEX_INITIAL_OWNER, CreateMutexExW, CreateProcessW, DETACHED_PROCESS,
    GetCurrentThreadId, MUTEX_MODIFY_STATE, OpenMutexW, PROCESS_INFORMATION, ReleaseMutex,
    STARTUPINFOW, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
};

use crate::{cli_error, native::desktops::object_name};

fn desktop_names() -> Result<(String, String), String> {
    let desktop =
        object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) }).map_err(|err| {
            format!(
                "Could not identify the Windows desktop: {}",
                cli_error::io(err)
            )
        })?;
    let station = object_name(unsafe { GetProcessWindowStation() }).map_err(|err| {
        format!(
            "Could not identify the window station: {}",
            cli_error::io(err)
        )
    })?;
    Ok((station, desktop))
}

// Keep the existing mutex name, including across an upgrade from a pre-CLI build.
fn name() -> Result<Vec<u16>, String> {
    let (station, desktop) = desktop_names()?;
    let name = if desktop.eq_ignore_ascii_case("Default") && station.eq_ignore_ascii_case("WinSta0")
    {
        "Local\\RustileWM".to_owned()
    } else {
        format!("Local\\RustileWM.{}.{station}.{desktop}", station.len())
    };
    Ok(name.encode_utf16().chain([0]).collect())
}

pub(super) fn launch_background() -> Result<OwnedHandle, String> {
    let exe = std::env::current_exe().map_err(cli_error::io)?;
    let path: Vec<_> = exe.as_os_str().encode_wide().chain([0]).collect();
    let mut args: Vec<_> = "rustile.exe --background"
        .encode_utf16()
        .chain([0])
        .collect();
    let (station, desktop) = desktop_names()?;
    let mut desktop: Vec<_> = format!("{station}\\{desktop}")
        .encode_utf16()
        .chain([0])
        .collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: desktop.as_mut_ptr(),
        ..Default::default()
    };
    let mut child = PROCESS_INFORMATION::default();
    // A long-lived manager must never keep the calling shell's output pipes
    // open. The parent uses the local command channel to confirm readiness.
    if unsafe {
        CreateProcessW(
            path.as_ptr(),
            args.as_mut_ptr(),
            null(),
            null(),
            0,
            DETACHED_PROCESS,
            null(),
            null(),
            &startup,
            &mut child,
        )
    } == 0
    {
        return Err(format!(
            "Could not start Rustile: {}",
            cli_error::io(io::Error::last_os_error())
        ));
    }
    drop(unsafe { OwnedHandle::from_raw_handle(child.hThread) });
    Ok(unsafe { OwnedHandle::from_raw_handle(child.hProcess) })
}

pub(crate) struct Instance(OwnedHandle);

// Keep independent external launches from replacing the owner midway through
// each other's handoff. Installed CLI clients still reach the owner directly.
pub(crate) fn installation_lock() -> Result<Instance, String> {
    let mut key = name()?;
    key.pop();
    key.extend(".Install".encode_utf16().chain([0]));
    let raw = unsafe {
        CreateMutexExW(
            null(),
            key.as_ptr(),
            0,
            SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
        )
    };
    if raw.is_null() {
        return Err(cli_error::io(io::Error::last_os_error()));
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 120_000) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Instance(handle)),
        WAIT_TIMEOUT => Err("Timed out waiting for another Rustile installation.".into()),
        _ => Err(cli_error::io(io::Error::last_os_error())),
    }
}

impl Instance {
    pub(super) fn acquire() -> Result<Option<Self>, String> {
        Self::elect(&name()?).map_err(|err| {
            format!(
                "Could not check the Rustile instance: {}",
                cli_error::io(err)
            )
        })
    }

    fn elect(name: &[u16]) -> io::Result<Option<Self>> {
        let mutex = unsafe {
            CreateMutexExW(
                null(),
                name.as_ptr(),
                CREATE_MUTEX_INITIAL_OWNER,
                SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
            )
        };
        if mutex.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                // A differently elevated primary is still the owner.
                Ok(None)
            } else {
                Err(error)
            };
        }
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = unsafe { OwnedHandle::from_raw_handle(mutex) };
        if exists {
            match unsafe { WaitForSingleObject(mutex.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => {}
                WAIT_TIMEOUT => return Ok(None),
                _ => return Err(io::Error::last_os_error()),
            }
        }
        Ok(Some(Self(mutex)))
    }

    pub(super) fn running() -> Result<bool, String> {
        let name = name()?;
        let handle = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            return match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_FILE_NOT_FOUND) => Ok(false),
                Some(ERROR_ACCESS_DENIED) => Ok(true),
                _ => Err(format!(
                    "Could not check the Rustile instance: {}",
                    cli_error::io(error)
                )),
            };
        }
        drop(unsafe { OwnedHandle::from_raw_handle(handle) });
        Ok(true)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.0.as_raw_handle()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additional_launches_leave_the_primary_running_until_it_exits() {
        let name: Vec<_> = format!("Local\\Rustile.test.{}", std::process::id())
            .encode_utf16()
            .chain([0])
            .collect();
        let primary = Instance::elect(&name).unwrap().unwrap();
        std::thread::scope(|scope| {
            let launches: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        assert!(Instance::elect(&name).unwrap().is_none());
                    })
                })
                .collect();
            for launch in launches {
                launch.join().unwrap();
            }
        });
        drop(primary);
        assert!(Instance::elect(&name).unwrap().is_some());
    }
}
