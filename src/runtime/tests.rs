use super::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::core::w;

#[test]
#[ignore = "driven by desktop-smoke on a private Win32 desktop"]
fn isolated_startup_tray() {
    let desktop = object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) }).unwrap();
    assert_eq!(desktop, std::env::var("RUSTILE_SMOKE_DESKTOP").unwrap());
    let uuid = desktop.strip_prefix("rustile-smoke-").unwrap();
    assert!(uuid.len() == 32 && uuid.bytes().all(|c| c.is_ascii_hexdigit()));

    // This test runs alone in a dedicated process. Even if it crashes, its
    // fake Run key is outside the paths Windows processes at logon.
    let scratch = format!(r"Software\{desktop}")
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
    let mut root = null_mut();
    let mut disposition = 0;
    unsafe {
        assert_eq!(
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                scratch.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut root,
                &mut disposition
            ),
            0
        );
        assert_eq!(disposition, REG_CREATED_NEW_KEY);
        // The app can open an existing Run key regardless of its volatility.
        let mut sub = null_mut();
        assert_eq!(
            RegCreateKeyExW(
                root,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
                0,
                std::ptr::null(),
                REG_OPTION_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut sub,
                null_mut()
            ),
            0
        );
        RegCloseKey(sub);
        assert_eq!(RegOverridePredefKey(HKEY_CURRENT_USER, root), 0);
    }
    crate::startup::mock_task_for_smoke();
    let result = std::panic::catch_unwind(|| run_with_args(&[]));
    unsafe {
        assert_eq!(RegOverridePredefKey(HKEY_CURRENT_USER, null_mut()), 0);
        RegCloseKey(root);
        assert_eq!(RegDeleteTreeW(HKEY_CURRENT_USER, scratch.as_ptr()), 0);
    }
    result.unwrap().unwrap();
}
