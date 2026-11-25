use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME,
    RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
use windows_sys::core::w;

// Registry notifications are wake-ups only. The runtime queries Explorer for
// the current desktop/order before making any decision.
pub(super) struct DesktopWatch {
    stop: Arc<OwnedHandle>,
    thread: Option<JoinHandle<()>>,
}

impl DesktopWatch {
    pub(super) fn start(thread_id: u32, message: u32) -> Option<Self> {
        let stop = Arc::new(event(true)?);
        let worker_stop = Arc::clone(&stop);
        let changed = event(false)?;
        let thread = thread::Builder::new()
            .name("rustile-desktop-watch".into())
            .spawn(move || {
                let handles = [worker_stop.as_raw_handle(), changed.as_raw_handle()];
                let mut key = None;
                loop {
                    if key.is_none() {
                        key = DesktopKey::open();
                    }
                    if key.as_ref().is_some_and(|k| k.arm(&changed)) {
                        // Arm first, including at startup and after Explorer
                        // recreates the key. A switch during reconciliation
                        // then causes another wake-up.
                        unsafe { PostThreadMessageW(thread_id, message, 0, 0) };
                        if unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) }
                            != WAIT_OBJECT_0 + 1
                        {
                            return;
                        }
                    } else {
                        key = None;
                        // Explorer can briefly remove this key. Retry opening
                        // it without polling the registry while it is healthy.
                        if unsafe { WaitForSingleObject(handles[0], 2_000) } != WAIT_TIMEOUT {
                            return;
                        }
                    }
                }
            })
            .ok()?;
        Some(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for DesktopWatch {
    fn drop(&mut self) {
        unsafe { SetEvent(self.stop.as_raw_handle()) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn event(manual_reset: bool) -> Option<OwnedHandle> {
    let handle = unsafe { CreateEventW(null(), manual_reset.into(), 0, null()) };
    (!handle.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(handle) })
}

struct DesktopKey(HKEY);

impl DesktopKey {
    fn open() -> Option<Self> {
        let mut key = null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops"),
                0,
                KEY_NOTIFY,
                &mut key,
            )
        };
        if status == ERROR_SUCCESS {
            Some(Self(key))
        } else {
            None
        }
    }

    fn arm(&self, changed: &OwnedHandle) -> bool {
        let filter = REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET;
        unsafe {
            RegNotifyChangeKeyValue(self.0, 1, filter, changed.as_raw_handle(), 1) == ERROR_SUCCESS
        }
    }
}

impl Drop for DesktopKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}
