use super::*;
use crate::native::desktops::object_name;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop;
use windows_sys::core::w;

#[test]
fn desktop_focus_waits_for_the_requested_desktop_and_ignores_reorders() {
    let (a, b) = (DesktopId(1), DesktopId(2));
    let mut focus = DesktopFocus {
        observed: None,
        pending: None,
    };
    assert!(!focus.observe(Some(a)));
    assert!(!focus.observe(Some(a)));
    assert!(
        focus
            .settle(Some(a), |_| panic!("startup must not activate a window"))
            .is_none()
    );
    focus.requested(a, b);
    assert!(!focus.observe(Some(a)));
    assert!(
        focus
            .settle(Some(a), |_| panic!("still on the source desktop"))
            .is_some()
    );
    assert!(!focus.observe(Some(b)));
    assert!(
        focus
            .settle(Some(b), |id| {
                assert_eq!(id, b);
                true
            })
            .is_none()
    );
    assert!(!focus.observe(Some(b)));
    assert!(
        focus
            .settle(Some(b), |_| panic!("must restore only once"))
            .is_none()
    );
}

#[test]
fn external_switch_supersedes_pending_focus_and_empty_desktops_stop_retrying() {
    let (a, b, c) = (DesktopId(1), DesktopId(2), DesktopId(3));
    let mut focus = DesktopFocus {
        observed: Some(a),
        pending: None,
    };
    focus.requested(a, b);
    assert!(focus.observe(Some(c)));
    for _ in 0..8 {
        focus.settle(Some(c), |id| {
            assert_eq!(id, c);
            false
        });
    }
    assert!(focus.pending.is_none());
    assert!(!focus.observe(Some(c)));
    // A return from a blank desktop must arm recovery again.
    assert!(focus.observe(Some(a)));
    assert!(
        focus
            .settle(Some(a), |id| {
                assert_eq!(id, a);
                true
            })
            .is_none()
    );
}

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
