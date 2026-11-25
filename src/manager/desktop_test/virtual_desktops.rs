use windows_sys::Win32::{
    Foundation::RECT,
    Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
    UI::WindowsAndMessaging::{
        EVENT_OBJECT_CLOAKED, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
        EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
        EVENT_SYSTEM_MINIMIZESTART, GetWindowRect, IsIconic, SMTO_ABORTIFHUNG, SMTO_ERRORONEXIT,
        SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        SendMessageTimeoutW, SetWindowPos, ShowWindowAsync, WM_APP,
    },
};

use super::{Direction, Id, Manager, Rect, frame, set_visible, settle, until};
use crate::native::desktops::{DesktopId, DesktopMembership};

const A: DesktopId = DesktopId(1);
const B: DesktopId = DesktopId(2);

fn select_desktop(
    manager: &mut Manager,
    homes: &[(Id, DesktopId)],
    current: DesktopId,
    unknown: Option<Id>,
) {
    manager
        .desktops
        .set_memberships(homes.iter().map(|&(id, home)| {
            let membership = if Some(id) == unknown {
                DesktopMembership::Unknown
            } else if home == current {
                DesktopMembership::Current(home)
            } else {
                DesktopMembership::Other(home)
            };
            (id, membership)
        }));
}

fn fixture_message(id: Id, message: u32, argument: usize) -> usize {
    let mut result = 0;
    assert_ne!(
        unsafe {
            SendMessageTimeoutW(
                id as _,
                message,
                argument,
                0,
                SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
                1000,
                &mut result,
            )
        },
        0
    );
    result
}

fn geometry_changes(id: Id) -> usize {
    fixture_message(id, WM_APP + 3, 0)
}

// Only the fixture can cloak its own window. This leaves WS_VISIBLE set, as
// during a shell desktop switch. Membership still comes from the test adapter.
fn cloak(id: Id, cloaked: bool) {
    assert_eq!(fixture_message(id, WM_APP + 4, usize::from(cloaked)), 0);
    until("fixture DWM cloak", || {
        let mut flags = 0u32;
        let hr = unsafe {
            DwmGetWindowAttribute(
                id as _,
                DWMWA_CLOAKED as _,
                (&raw mut flags).cast(),
                size_of_val(&flags) as _,
            )
        };
        hr >= 0 && (flags != 0) == cloaked
    });
}

fn shift_frame(id: Id) {
    let before = frame(id);
    let mut outer = RECT::default();
    assert_ne!(unsafe { GetWindowRect(id as _, &mut outer) }, 0);
    assert_ne!(
        unsafe {
            SetWindowPos(
                id as _,
                std::ptr::null_mut(),
                outer.left + 13,
                outer.top,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            )
        },
        0
    );
    until("fixture shifted by Windows", || {
        frame(id).x == before.x + 13
    });
}

fn first_show_after_all_hide(
    manager: &mut Manager,
    previous: [Id; 3],
    fresh: Id,
    outside: [Id; 2],
) {
    let home = |manager: &mut Manager, visible: &[Id]| {
        manager.desktops.set_memberships(
            previous
                .into_iter()
                .chain([fresh])
                .map(|id| {
                    let membership = if visible.contains(&id) {
                        DesktopMembership::Current(B)
                    } else {
                        DesktopMembership::Unknown
                    };
                    (id, membership)
                })
                .chain(outside.map(|id| (id, DesktopMembership::Other(A)))),
        );
    };
    let untouched = outside.map(frame);
    set_visible(fresh, false);
    home(manager, &[]);
    for id in previous {
        set_visible(id, false);
        manager.event(EVENT_OBJECT_HIDE, id);
    }
    // No visible HWND identifies B yet. These events are indistinguishable
    // from the beginning of a desktop switch, so preserve its layout.
    manager.reconcile_desktop();
    assert!(
        previous
            .iter()
            .all(|&id| manager.workspace.layout.contains(id))
    );
    // First return a retained tile at exactly its last requested bounds. Then
    // try a brand-new HWND with no visible anchor. In either case the only
    // incoming event is SHOW; no timer is delivered unless the manager asks.
    for first in [previous[0], fresh] {
        home(manager, &[first]);
        set_visible(first, true);
        if first != fresh {
            assert_eq!(manager.windows[&first].requested, Some(frame(first)));
        }
        manager.take_desktop_retry();
        manager.event(EVENT_OBJECT_SHOW, first);
        if manager.take_desktop_retry() {
            manager.reconcile_desktop();
        }
        settle(manager, 1);
        assert_eq!(frame(first), manager.area.unwrap());
        if first != fresh {
            home(manager, &[]);
            set_visible(first, false);
            manager.event(EVENT_OBJECT_HIDE, first);
            manager.reconcile_desktop();
            assert!(manager.workspace.layout.contains(first));
        }
    }

    home(manager, &[previous[0], previous[1], previous[2], fresh]);
    for id in previous {
        set_visible(id, true);
        manager.event(EVENT_OBJECT_SHOW, id);
    }
    manager.reconcile_desktop();
    settle(manager, 4);
    set_visible(fresh, false);
    home(manager, &previous);
    manager.event(EVENT_OBJECT_HIDE, fresh);
    manager.reconcile_desktop();
    settle(manager, 3);
    let [a, b, c] = previous.map(frame);
    assert!(
        [(a, b, c), (a, c, b), (b, c, a)]
            .into_iter()
            .any(|(a, b, c)| {
                super::join_frames(a, b).and_then(|joined| super::join_frames(joined, c))
                    == manager.area
            }),
        "the three real frames must fill the area after the fourth window hides"
    );
    assert_eq!(outside.map(frame), untouched);
    set_visible(fresh, true);
}

pub(super) fn round_trip(manager: &mut Manager, windows: [Id; 6], _area: Rect) {
    let mut a = [windows[0], windows[1]];
    let b = [windows[2], windows[3]];
    let homes = [(a[0], A), (a[1], A), (b[0], B), (b[1], B)];
    let spare = [windows[4], windows[5]];
    let untouched = spare.map(frame);
    select_desktop(manager, &homes, A, None);
    manager.sync();
    settle(manager, a.len());

    a.sort_by_key(|&id| frame(id).x);
    assert!(frame(a[0]).x < frame(a[1]).x);
    let starting = a.map(frame);
    assert!(manager.begin_resize(a[0]));
    manager.resize(a[0], Direction::Right, false);
    manager.finish_mode(true);
    settle(manager, a.len());
    let resized = a.map(frame);
    assert!(resized[0].w > starting[0].w);
    // Leave a two-press move halfway through. Remembering only rectangles
    // loses the destination slots for its second step.
    manager.move_window(a[0], Direction::Right);
    settle(manager, a.len());
    let shared = a.map(frame);
    assert_eq!(shared[0].x, shared[1].x);
    assert_ne!(shared[0].y, shared[1].y);
    let a_changes = a.map(geometry_changes);
    assert!(a_changes.iter().all(|count| *count > 0));

    // A late cloak, a temporary membership failure and a new foreground can
    // arrive in one burst. Neither inaccessible A window relinquishes its tile.
    cloak(a[0], true);
    manager.event(EVENT_OBJECT_HIDE, a[0]); // shell membership still says A is current
    select_desktop(manager, &homes, B, Some(a[0]));
    manager.event(EVENT_OBJECT_CLOAKED, a[0]);
    manager.event(EVENT_OBJECT_UNCLOAKED, b[0]);
    manager.event(EVENT_SYSTEM_FOREGROUND, b[0]);
    select_desktop(manager, &homes, B, None);
    manager.event(EVENT_OBJECT_CLOAKED, a[1]);
    manager.reconcile_desktop();
    settle(manager, b.len());
    assert_eq!(a.map(geometry_changes), a_changes, "A moved while B opened");
    let b_tiles = b.map(frame);
    let b_changes = b.map(geometry_changes);

    // The same real SW_HIDE with an unknown membership must not release an A
    // slot while a B peer confirms that B is the active workspace.
    set_visible(a[1], false);
    select_desktop(manager, &homes, B, Some(a[1]));
    manager.event(EVENT_OBJECT_HIDE, a[1]);
    manager.reconcile_desktop();
    assert_eq!(b.map(frame), b_tiles);
    assert_eq!(b.map(geometry_changes), b_changes);
    set_visible(a[1], true);
    select_desktop(manager, &homes, B, None);
    manager.event(EVENT_OBJECT_SHOW, a[1]);

    // The shell now reports all A handles as current while DWM still cloaks
    // the first one. Drawing its sibling at full width would visibly flicker.
    select_desktop(manager, &homes, A, None);
    manager.event(EVENT_SYSTEM_FOREGROUND, a[1]);
    manager.event(EVENT_OBJECT_UNCLOAKED, a[1]);
    manager.event(EVENT_OBJECT_LOCATIONCHANGE, b[1]);
    manager.reconcile_desktop();
    assert_eq!(a.map(geometry_changes), a_changes);
    cloak(a[0], false);
    manager.event(EVENT_OBJECT_UNCLOAKED, a[0]);
    manager.event(EVENT_OBJECT_CLOAKED, b[0]);
    manager.reconcile_desktop();
    settle(manager, a.len());
    assert_eq!(a.map(frame), shared, "first return to A");
    assert_eq!(a.map(geometry_changes), a_changes, "A visibly reflowed");
    assert_eq!(b.map(frame), b_tiles);
    assert_eq!(
        b.map(geometry_changes),
        b_changes,
        "an inactive B window moved"
    );

    manager.move_window(a[0], Direction::Right);
    settle(manager, a.len());
    assert_eq!(a.map(frame), [resized[1], resized[0]]);

    select_desktop(manager, &homes, B, None);
    manager.event(EVENT_OBJECT_CLOAKED, a[1]);
    manager.event(EVENT_OBJECT_CLOAKED, a[0]);
    manager.event(EVENT_OBJECT_UNCLOAKED, b[1]);
    manager.reconcile_desktop();
    settle(manager, b.len());
    assert_eq!(b.map(frame), b_tiles, "first return to B");
    assert_eq!(b.map(geometry_changes), b_changes);
    assert_eq!(spare.map(frame), untouched);

    // An application can change its own bounds while its desktop is inactive.
    // Only returning to that desktop should correct the stale physical frame.
    shift_frame(a[0]);
    let changed = frame(a[0]);
    manager.event(EVENT_OBJECT_LOCATIONCHANGE, a[0]);
    manager.reconcile_desktop();
    assert_eq!(frame(a[0]), changed);
    select_desktop(manager, &homes, A, None);
    manager.event(EVENT_OBJECT_UNCLOAKED, a[1]);
    manager.reconcile_desktop();
    settle(manager, a.len());
    assert_eq!(a.map(frame), [resized[1], resized[0]]);

    // Minimizing a window on the current desktop does free its tile.
    assert_ne!(unsafe { ShowWindowAsync(a[0] as _, SW_SHOWMINNOACTIVE) }, 0);
    until("fixture minimized", || unsafe { IsIconic(a[0] as _) != 0 });
    manager.event(EVENT_SYSTEM_MINIMIZESTART, a[0]);
    manager.reconcile_desktop();
    settle(manager, 1);
    assert_eq!(frame(a[1]), manager.area.unwrap());
    assert_ne!(unsafe { ShowWindowAsync(a[0] as _, SW_SHOWNOACTIVATE) }, 0);
    until("fixture restored", || unsafe { IsIconic(a[0] as _) == 0 });
    manager.event(EVENT_SYSTEM_MINIMIZEEND, a[0]);
    manager.reconcile_desktop();
    settle(manager, a.len());

    // A tray app can keep its HWND but lose its virtual-desktop membership
    // while hidden. Its visible peer still identifies A unambiguously.
    set_visible(a[0], false);
    select_desktop(manager, &homes, A, Some(a[0]));
    manager.event(EVENT_OBJECT_HIDE, a[0]);
    if manager.workspace.layout.contains(a[0]) {
        assert!(
            manager.take_desktop_retry(),
            "a deferred HIDE must wake the normal settle path"
        );
    }
    until(
        "hidden tray HWND with unknown membership still occupies an A tile",
        || {
            manager.reconcile_desktop();
            !manager.workspace.layout.contains(a[0])
        },
    );
    settle(manager, 1);
    assert_eq!(
        frame(a[1]),
        manager.area.unwrap(),
        "the visible peer must reclaim the tray slot"
    );
    assert_eq!(b.map(geometry_changes), b_changes);
    set_visible(a[0], true);
    select_desktop(manager, &homes, A, None);
    manager.event(EVENT_OBJECT_SHOW, a[0]);
    manager.reconcile_desktop();
    settle(manager, 2);
    assert_eq!(super::join_frames(frame(a[0]), frame(a[1])), manager.area);
    assert_eq!(b.map(geometry_changes), b_changes);

    // Move one HWND to B using the shell. It leaves A immediately and receives
    // one tile on B when visited; B's previous frames hold in the meantime.
    let transferred = [(a[0], B), (a[1], A), (b[0], B), (b[1], B)];
    select_desktop(manager, &transferred, A, None);
    manager.event(EVENT_OBJECT_CLOAKED, a[0]);
    manager.reconcile_desktop();
    settle(manager, 1);
    assert_eq!(frame(a[1]), manager.area.unwrap());
    assert_eq!(b.map(frame), b_tiles);
    select_desktop(manager, &transferred, B, None);
    manager.event(EVENT_OBJECT_UNCLOAKED, a[0]);
    manager.reconcile_desktop();
    settle(manager, 3);
    assert!(manager.workspace.layout.contains(a[0]));
    assert!(b.iter().all(|id| manager.workspace.layout.contains(*id)));

    first_show_after_all_hide(manager, [a[0], b[0], b[1]], spare[0], [a[1], spare[1]]);

    // Return the injected adapter to the private test desktop before the
    // harness restores and verifies the originals of every fixture.
    manager
        .desktops
        .set_memberships(windows.map(|id| (id, DesktopMembership::Current(B))));
}
