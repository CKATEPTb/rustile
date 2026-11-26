use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, WAIT_OBJECT_0};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME,
    RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW,
    QS_ALLINPUT,
};
use windows_sys::core::{PCWSTR, w};

// Registry notifications only invalidate the Shell snapshot. Rearm before
// reading that snapshot, including after returning from a native modal loop.
pub(super) struct DesktopWatch {
    key: Key,
    changed: OwnedHandle,
    desktops: bool,
}

impl DesktopWatch {
    pub(super) fn start() -> Option<Self> {
        Self::under(Key::open(
            HKEY_CURRENT_USER,
            w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer"),
        )?)
    }

    fn under(parent: Key) -> Option<Self> {
        let open_child = |parent| Self::subscribe(Key::open(parent, w!("VirtualDesktops"))?, true);
        if let Some(child) = open_child(parent.0) {
            return Some(child);
        }
        // Arm the parent first, then look again: Explorer could create the
        // child between the original open and RegNotifyChangeKeyValue.
        let waiting = Self::subscribe(parent, false)?;
        open_child(waiting.key.0).or(Some(waiting))
    }

    fn subscribe(key: Key, desktops: bool) -> Option<Self> {
        let handle = unsafe { CreateEventW(null(), 0, 0, null()) };
        if handle.is_null() {
            return None;
        }
        let watch = Self {
            key,
            changed: unsafe { OwnedHandle::from_raw_handle(handle) },
            desktops,
        };
        watch.arm().then_some(watch)
    }

    pub(super) fn watching_desktops(&self) -> bool {
        self.desktops
    }

    fn arm(&self) -> bool {
        let filter = if self.desktops {
            REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET
        } else {
            REG_NOTIFY_CHANGE_NAME
        };
        unsafe {
            RegNotifyChangeKeyValue(
                self.key.0,
                self.desktops.into(),
                filter,
                self.changed.as_raw_handle(),
                1,
            ) == ERROR_SUCCESS
        }
    }

    fn renew(watch: &mut Option<Self>, open: impl FnOnce() -> Option<Self>) -> bool {
        let was_active = watch.as_ref().is_some_and(|w| w.desktops);
        if was_active && watch.as_ref().unwrap().arm() {
            return true;
        }
        // Windows can leave a deleted key pending until its last handle closes.
        drop(watch.take());
        *watch = open();
        was_active || watch.as_ref().is_some_and(|w| w.desktops)
    }
}

// Return whether desktop state also changed. PeekMessage dispatches synchronous
// sends and WinEvent callbacks even when there is no posted MSG to remove.
pub(super) fn next_message(watch: &mut Option<DesktopWatch>, msg: &mut MSG) -> io::Result<bool> {
    loop {
        let Some(active) = watch.as_ref() else {
            if unsafe { GetMessageW(msg, null_mut(), 0, 0) } == -1 {
                return Err(io::Error::last_os_error());
            }
            return Ok(false);
        };
        let handle = active.changed.as_raw_handle();
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(1, &handle, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        let changed = match wait {
            WAIT_OBJECT_0 => DesktopWatch::renew(watch, DesktopWatch::start),
            w if w == WAIT_OBJECT_0 + 1 => false,
            _ => return Err(io::Error::last_os_error()),
        };
        if unsafe { PeekMessageW(msg, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            return Ok(changed);
        }
        if changed {
            *msg = MSG::default();
            return Ok(true);
        }
    }
}

struct Key(HKEY);

impl Key {
    fn open(parent: HKEY, name: PCWSTR) -> Option<Self> {
        let mut key = null_mut();
        let status = unsafe { RegOpenKeyExW(parent, name, 0, KEY_NOTIFY, &mut key) };
        (status == ERROR_SUCCESS).then(|| Self(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Registry::{
        KEY_ALL_ACCESS, REG_DWORD, REG_OPTION_VOLATILE, RegCreateKeyExW, RegDeleteKeyW,
        RegDeleteTreeW, RegSetValueExW,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThreadId, WaitForSingleObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, KillTimer, PostQuitMessage,
        PostThreadMessageW, SMTO_BLOCK, SendMessageTimeoutW, SetTimer, WM_APP, WM_GETTEXTLENGTH,
        WM_QUIT, WM_TIMER,
    };

    struct TestKey(Vec<u16>);
    impl Drop for TestKey {
        fn drop(&mut self) {
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, self.0.as_ptr()) };
        }
    }

    fn create(parent: HKEY, name: PCWSTR) -> Key {
        let mut key = null_mut();
        assert_eq!(
            unsafe {
                RegCreateKeyExW(
                    parent,
                    name,
                    0,
                    null(),
                    REG_OPTION_VOLATILE,
                    KEY_ALL_ACCESS,
                    null(),
                    &mut key,
                    null_mut(),
                )
            },
            ERROR_SUCCESS
        );
        Key(key)
    }

    #[test]
    fn waits_for_recreated_desktop_key_and_rearms_before_returning_to_the_pump() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = TestKey(
            format!(
                r"Software\Rustile.Test.{}.{nonce}.DesktopWatch",
                std::process::id()
            )
            .encode_utf16()
            .chain([0])
            .collect(),
        );
        let parent = create(HKEY_CURRENT_USER, path.0.as_ptr());
        let open = || DesktopWatch::under(Key::open(HKEY_CURRENT_USER, path.0.as_ptr()).unwrap());
        let mut watch = open();
        let wait = |watch: &Option<DesktopWatch>, ms| unsafe {
            WaitForSingleObject(watch.as_ref().unwrap().changed.as_raw_handle(), ms)
        };

        assert!(!watch.as_ref().unwrap().watching_desktops());
        let child = create(parent.0, w!("VirtualDesktops"));
        assert_eq!(wait(&watch, 1_000), WAIT_OBJECT_0);
        assert!(DesktopWatch::renew(&mut watch, open));
        assert!(watch.as_ref().unwrap().watching_desktops());
        let mut msg = MSG::default();
        unsafe { PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) };
        let timeout = unsafe { SetTimer(null_mut(), 0, 2000, None) };
        assert_ne!(timeout, 0);
        for value in [1u32, 2] {
            // Even when a WM_APP is queued before retrieving the registry event,
            // service both. Each subsequent write must wake the same UI thread.
            assert_ne!(
                unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP, 0, 0) },
                0
            );
            assert_eq!(
                unsafe {
                    RegSetValueExW(
                        child.0,
                        w!("Desktop"),
                        0,
                        REG_DWORD,
                        value.to_ne_bytes().as_ptr(),
                        4,
                    )
                },
                ERROR_SUCCESS
            );
            let (mut changed, mut posted) = (false, false);
            while !changed || !posted {
                changed |= next_message(&mut watch, &mut msg).unwrap();
                posted |= msg.message == WM_APP;
                assert_ne!(msg.message, WM_TIMER, "missed the change or posted message");
            }
            assert_eq!(wait(&watch, 0), WAIT_TIMEOUT);
        }
        // A synchronous send wakes the queue without posting a MSG. Once
        // dispatched, this same wait must still wake for a registry-only write.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                w!("watch"),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                null(),
            )
        };
        assert!(!hwnd.is_null());
        let window = hwnd as usize;
        let key = child.0 as usize;
        std::thread::scope(|scope| {
            let sender = scope.spawn(move || unsafe {
                let mut length = 0;
                assert_ne!(
                    SendMessageTimeoutW(
                        window as _,
                        WM_GETTEXTLENGTH,
                        0,
                        0,
                        SMTO_BLOCK,
                        1_000,
                        &mut length
                    ),
                    0
                );
                assert_eq!(length, 5);
                assert_eq!(
                    RegSetValueExW(
                        key as HKEY,
                        w!("Desktop"),
                        0,
                        REG_DWORD,
                        3u32.to_ne_bytes().as_ptr(),
                        4
                    ),
                    ERROR_SUCCESS
                );
            });
            while !next_message(&mut watch, &mut msg).unwrap() {
                assert_ne!(msg.message, WM_TIMER, "did not dispatch sent input");
            }
            assert_ne!(
                msg.message, WM_TIMER,
                "registry-only change waited for a message"
            );
            sender.join().unwrap();
        });
        unsafe { DestroyWindow(hwnd) };
        drop(child);
        assert_eq!(
            unsafe { RegDeleteKeyW(parent.0, w!("VirtualDesktops")) },
            ERROR_SUCCESS
        );
        assert_eq!(wait(&watch, 1_000), WAIT_OBJECT_0);
        assert!(DesktopWatch::renew(&mut watch, open));
        assert!(!watch.as_ref().unwrap().watching_desktops());
        let sibling = create(parent.0, w!("Unrelated"));
        assert_eq!(wait(&watch, 1_000), WAIT_OBJECT_0);
        assert!(!DesktopWatch::renew(&mut watch, open));
        let _child = create(parent.0, w!("VirtualDesktops"));
        assert_eq!(wait(&watch, 1_000), WAIT_OBJECT_0);
        assert!(DesktopWatch::renew(&mut watch, open));
        drop(sibling);
        unsafe { KillTimer(null_mut(), timeout) };
        unsafe { PostQuitMessage(0) };
        assert!(!next_message(&mut watch, &mut msg).unwrap());
        assert_eq!(msg.message, WM_QUIT);
    }
}
