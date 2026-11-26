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
    assert!(
        focus.awaiting_switch(),
        "the next wheel step must wait for Explorer"
    );
    assert!(!focus.observe(Some(a)));
    assert!(focus.awaiting_switch());
    assert!(
        focus
            .settle(Some(a), |_| panic!("still on the source desktop"))
            .is_some()
    );
    assert!(!focus.observe(Some(b)));
    assert!(
        !focus.awaiting_switch(),
        "navigation can continue before focus settles"
    );
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

fn smoke_desktop() -> String {
    let desktop = object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) }).unwrap();
    assert_eq!(desktop, std::env::var("RUSTILE_SMOKE_DESKTOP").unwrap());
    let uuid = desktop.strip_prefix("rustile-smoke-").unwrap();
    assert!(uuid.len() == 32 && uuid.bytes().all(|c| c.is_ascii_hexdigit()));
    desktop
}

#[test]
#[ignore = "driven by desktop-smoke on a private Win32 desktop"]
fn isolated_shortcut_ownership() {
    use crate::settings::{Chord, Shortcut};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_WIN, VK_F13, VK_F14, VK_F15, VK_F16, VK_F17, VK_LSHIFT,
    };

    smoke_desktop();
    assert!(unsafe { GetForegroundWindow() }.is_null());
    let mut settings = Settings::default();
    for (action, key) in [
        (Shortcut::FocusLeft, VK_F13),
        (Shortcut::ToggleMosaic, VK_F14),
        (Shortcut::Desktop10, VK_F16),
        (Shortcut::MoveNextDesktop, VK_F17),
    ] {
        settings = settings
            .changed(Command::Shortcut(action, Chord { key, mods: 0 }))
            .unwrap();
    }
    let hook = shortcuts::Hook::install(settings).unwrap();
    let owner = unsafe { GetCurrentThreadId() };
    let key = |vk, down| shortcuts::test_keyboard_event(vk, down, owner);
    let next_command = || {
        let mut msg = MSG::default();
        let found =
            unsafe { PeekMessageW(&mut msg, null_mut(), KEY_COMMAND, KEY_COMMAND, PM_REMOVE) };
        (found != 0).then_some(((msg.wParam & 0xff) as i32, msg.wParam >> 8, msg.lParam))
    };
    // This also creates the receiving thread's queue. The callback is invoked
    // directly, so no input enters the user's session or the Shell.
    assert_eq!(next_command(), None);
    assert_eq!(key(VK_F13, true), 1, "window command without a tile");
    hook.set_settings(Settings::default());
    assert_eq!(key(VK_F13, true), 1, "held repeat after unbinding");
    assert_eq!(key(VK_F13, false), 1, "release after unbinding");
    assert_eq!(key(VK_F13, true), 0, "next press uses the new bindings");
    assert_eq!(key(VK_F13, false), 0);
    assert_eq!(next_command(), None);

    hook.set_settings(settings);
    assert_eq!(key(VK_F14, true), 1);
    assert!(matches!(next_command(), Some((TOGGLE_MOSAIC, ticket, 0)) if ticket != 0));
    assert_eq!(key(VK_F14, true), 1);
    assert_eq!(key(VK_F14, false), 1);
    assert_eq!(next_command(), None);

    // Even a failed PostThreadMessage must consume the entire assigned press.
    assert_eq!(shortcuts::test_keyboard_event(VK_F14, true, u32::MAX), 1);
    assert_eq!(key(VK_F14, true), 1);
    assert_eq!(key(VK_F14, false), 1);
    assert_eq!(next_command(), None);
    assert_eq!(key(VK_F15, true), 0);
    assert_eq!(key(VK_F15, false), 0);

    assert_eq!(key(VK_F16, true), 1);
    assert_eq!(key(VK_F16, true), 1);
    assert_eq!(key(VK_F16, false), 1);
    assert_eq!(next_command(), Some((DESKTOP_TENTH, 0, 0)));
    assert_eq!(next_command(), None);

    assert_eq!(key(VK_F17, true), 1);
    assert_eq!(key(VK_F17, false), 1);
    assert_eq!(next_command(), None, "transfer needs a foreground window");

    shortcuts::watch_transfer_focus();
    key(VK_LSHIFT, true);
    key(VK_LSHIFT, false);
    key(0xff, true); // Rustile's Win/Alt usage marker.
    key(0xff, false);
    assert!(shortcuts::transfer_focus_uninterrupted());
    assert_eq!(key(VK_F17, true), 1);
    assert_eq!(key(VK_F17, true), 1);
    assert_eq!(key(VK_F17, false), 1);
    assert!(matches!(next_command(), Some((MOVE_DESKTOP_NEXT, ticket, 0)) if ticket != 0));
    assert_eq!(next_command(), None);
    assert!(shortcuts::transfer_focus_uninterrupted());

    assert_eq!(key(VK_F15, true), 0);
    assert!(!shortcuts::transfer_focus_uninterrupted());
    assert_eq!(key(VK_F15, false), 0);
    assert_eq!(key(VK_F17, true), 1);
    assert_eq!(key(VK_F17, false), 1);
    assert_eq!(
        next_command(),
        None,
        "ordinary input ends the follow gesture"
    );

    let wheel = |message, delta, mods| shortcuts::test_wheel_event(message, delta, mods, owner);
    assert!(!wheel(WM_MOUSEWHEEL, 120, 0));
    assert!(!wheel(WM_MOUSEHWHEEL, 120, MOD_WIN));
    assert!(wheel(WM_MOUSEWHEEL, 60, MOD_WIN));
    assert_eq!(next_command(), None);
    assert!(wheel(WM_MOUSEWHEEL, 60, MOD_WIN));
    assert_eq!(next_command(), Some((DESKTOP_PREVIOUS, 0, 0)));
    assert!(wheel(WM_MOUSEWHEEL, -240, MOD_WIN));
    assert_eq!(next_command(), Some((DESKTOP_NEXT, 0, 0)));
    assert_eq!(next_command(), Some((DESKTOP_NEXT, 0, 0)));
    assert_eq!(next_command(), None);
}

#[test]
#[ignore = "driven by desktop-smoke on a private Win32 desktop"]
fn isolated_startup_tray() {
    let desktop = smoke_desktop();

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
