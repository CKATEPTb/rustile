use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null;

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::StationsAndDesktops::{GetProcessWindowStation, GetThreadDesktop};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, GetCurrentThreadId, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW,
    ReleaseMutex, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowThreadProcessId, PostThreadMessageW, WM_QUIT,
};
use windows_sys::core::w;

use crate::native::{access::ProcessAccess, desktops::object_name};

const DUPLICATE: &str = "Rustile уже работает в этом сеансе Windows.";

pub(super) struct Instance(OwnedHandle);

impl Instance {
    pub(super) fn acquire() -> Result<Self, String> {
        let desktop = object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })?;
        let name = if desktop.eq_ignore_ascii_case("Default") {
            "Local\\RustileWM".to_owned()
        } else {
            let station = object_name(unsafe { GetProcessWindowStation() })?;
            format!("Local\\RustileWM.{}.{station}.{desktop}", station.len())
        };
        let name: Vec<_> = name.encode_utf16().chain([0]).collect();
        let (mutex, exists) = open_mutex(&name)?;
        if exists {
            let (process, thread) = lower_instance().ok_or(DUPLICATE)?;
            // The normal message-loop exit unregisters hooks and restores the
            // windows before we enumerate them. Also works with older builds.
            if unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0) } == 0 {
                return Err(format!(
                    "Не удалось закрыть Rustile: {}",
                    io::Error::last_os_error()
                ));
            }
            match unsafe { WaitForSingleObject(process.as_raw_handle(), 5000) } {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => {
                    return Err("Предыдущий экземпляр Rustile ещё завершает работу.".into());
                }
                _ => return Err(format!("Ожидание Rustile: {}", io::Error::last_os_error())),
            }
            // Keep the named object alive through the handoff. If two
            // elevated launches observed the old process, only one can own it.
            if !try_own(&mutex) {
                return Err(DUPLICATE.into());
            }
        }
        Ok(Self(mutex))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.0.as_raw_handle()) };
    }
}

fn open_mutex(name: &[u16]) -> Result<(OwnedHandle, bool), String> {
    let handle = unsafe { CreateMutexW(null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return Err(format!(
            "Проверка экземпляра Rustile: {}",
            io::Error::last_os_error()
        ));
    }
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    Ok((unsafe { OwnedHandle::from_raw_handle(handle) }, exists))
}

fn try_own(mutex: &OwnedHandle) -> bool {
    matches!(
        unsafe { WaitForSingleObject(mutex.as_raw_handle(), 0) },
        WAIT_OBJECT_0 | WAIT_ABANDONED
    )
}

fn image_path(process: HANDLE) -> Option<Vec<u16>> {
    let mut path = vec![0; 32_768];
    let mut len = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    path.truncate(len as usize);
    Some(path)
}

fn lower_instance() -> Option<(OwnedHandle, u32)> {
    // FindWindow sees the caller's interactive desktop. Verify the owning
    // process as well: any application can register a window of this class.
    let tray = unsafe { FindWindowW(w!("Rustile.Tray"), w!("Rustile")) };
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(tray, &mut pid) };
    if thread == 0 {
        return None;
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(handle) };
    let incumbent = ProcessAccess::query(handle)?;
    if !ProcessAccess::current()?.can_replace(&incumbent)
        || image_path(handle)? != image_path(unsafe { GetCurrentProcess() })?
    {
        return None;
    }
    let mut verified_pid = 0;
    let verified_thread = unsafe { GetWindowThreadProcessId(tray, &mut verified_pid) };
    (verified_pid == pid && verified_thread == thread).then_some((process, thread))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_replacements_can_claim_the_incumbents_mutex_only_once() {
        let name: Vec<_> = format!("Local\\Rustile.handoff.test.{}", std::process::id())
            .encode_utf16()
            .chain([0])
            .collect();
        let (original, existed) = open_mutex(&name).unwrap();
        assert!(!existed);
        let incumbent = Instance(original);
        let (first, existed) = open_mutex(&name).unwrap();
        assert!(existed);
        let (second, existed) = open_mutex(&name).unwrap();
        assert!(existed);
        assert!(!std::thread::scope(|s| s
            .spawn(|| try_own(&first))
            .join()
            .unwrap()));
        drop(incumbent);
        assert!(try_own(&first));
        let winner = Instance(first);
        assert!(!std::thread::scope(|s| s
            .spawn(|| try_own(&second))
            .join()
            .unwrap()));
        drop(winner);
        assert!(try_own(&second));
        drop(Instance(second));
    }
}
