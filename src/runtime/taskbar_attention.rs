//! Keep a taskbar attention request on the virtual desktop that owns its window.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FLASHW_STOP, FLASHWINFO, FlashWindowEx, GA_ROOT, GetAncestor, GetWindowThreadProcessId,
};

use crate::layout::Id;
use crate::native::desktops::{DesktopMembership, occupancy::is_pinned};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlashWindow {
    pub(crate) id: Id,
    process: u32,
    thread: u32,
}

impl FlashWindow {
    pub(crate) fn capture(id: Id) -> Option<Self> {
        let hwnd = id as HWND;
        if hwnd.is_null() || unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
            return None;
        }
        let mut process = 0;
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
        (thread != 0 && process != 0).then_some(Self {
            id,
            process,
            thread,
        })
    }

    pub(super) fn stop(self) {
        // Explorer's membership query can pump messages. Recheck the handle
        // before changing a window that may have closed in the meantime.
        if Self::capture(self.id) == Some(self) {
            let info = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd: self.id as HWND,
                dwFlags: FLASHW_STOP,
                ..Default::default()
            };
            unsafe { FlashWindowEx(&info) };
        }
    }
}

#[derive(Default)]
pub(super) struct TaskbarAttention {
    // Keep reported windows until departure or destruction. Foreground events
    // can lag a switch, and FLASHW_TIMER can outlive a foreground change.
    outstanding: Vec<FlashWindow>,
}

impl TaskbarAttention {
    pub(super) fn forget(&mut self, id: Id) {
        self.outstanding.retain(|window| window.id != id);
    }

    pub(super) fn update(
        &mut self,
        flashes: Vec<FlashWindow>,
        recheck: bool,
        mut membership: impl FnMut(FlashWindow) -> Option<DesktopMembership>,
        mut stop: impl FnMut(FlashWindow),
    ) -> bool {
        if flashes.is_empty() && !recheck {
            return false;
        }
        let mut retry = false;
        let mut keep = |window: FlashWindow, fresh: bool| match membership(window) {
            None => false, // destroyed or reused HWND
            Some(DesktopMembership::Current(id) | DesktopMembership::Other(id))
                if is_pinned(id) =>
            {
                false
            }
            Some(DesktopMembership::Other(_)) => {
                stop(window);
                false
            }
            Some(DesktopMembership::Current(_)) => true,
            Some(DesktopMembership::Unknown) => {
                // Give a new shell view one chance to settle even on a quiet
                // desktop. An unresolved retry waits for a real desktop event.
                retry |= fresh;
                true
            }
        };
        self.outstanding.retain(|window| {
            !flashes.iter().any(|new| new.id == window.id) && (!recheck || keep(*window, false))
        });
        self.outstanding
            .extend(flashes.into_iter().filter(|w| keep(*w, true)));
        retry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::desktops::{DesktopId, DesktopMembership::*};

    fn window(id: Id) -> FlashWindow {
        FlashWindow {
            id,
            process: 100,
            thread: 200,
        }
    }

    #[test]
    fn stops_only_foreign_requests_and_rechecks_outgoing_windows() {
        let mut attention = TaskbarAttention::default();
        let home = DesktopId(1);
        let away = DesktopId(2);
        let pin = DesktopId(0xc2ddea68_66f2_4cf9_8264_1bfd00fbbbac);
        let mut stopped = Vec::new();
        attention.update(
            (1..=6).map(window).collect(),
            false,
            |w| match w.id {
                1 => Some(Current(home)),
                2 => Some(Other(away)),
                3 => Some(Other(pin)),
                4 => Some(Unknown),
                5 => None,
                _ => Some(Current(pin)),
            },
            |w| stopped.push(w.id),
        );
        assert_eq!(stopped, [2]);
        assert_eq!(attention.outstanding, [window(1), window(4)]);
        attention.update(
            vec![],
            false,
            |_| panic!("queried while idle"),
            |_| unreachable!(),
        );
        // A late request from the outgoing window is handled once. An unknown
        // window that turns out to be on the new desktop can still alert there.
        attention.update(
            vec![window(1)],
            true,
            |w| {
                Some(if w.id == 1 {
                    Other(home)
                } else {
                    Current(away)
                })
            },
            |w| stopped.push(w.id),
        );
        assert_eq!(stopped, [2, 1]);
        assert_eq!(attention.outstanding, [window(4)]);
    }

    #[test]
    fn forgets_destroyed_and_reused_windows_before_rechecking() {
        let mut attention = TaskbarAttention::default();
        attention.update(
            (2..=3).map(window).collect(),
            false,
            |_| Some(Unknown),
            |_| unreachable!(),
        );
        attention.forget(2);
        let replacement = FlashWindow {
            process: 500,
            ..window(3)
        };
        attention.update(
            vec![replacement],
            true,
            |w| {
                assert_eq!(w, replacement);
                Some(Current(DesktopId(1)))
            },
            |_| unreachable!(),
        );
        attention.update(vec![], true, |_| None, |_| unreachable!());
        assert!(attention.outstanding.is_empty());
    }

    #[test]
    fn unresolved_membership_retries_once_then_waits_for_a_desktop_event() {
        let mut attention = TaskbarAttention::default();
        assert!(attention.update(
            vec![window(1)],
            false,
            |_| Some(Unknown),
            |_| unreachable!()
        ));
        assert!(!attention.update(vec![], true, |_| Some(Unknown), |_| unreachable!()));
        let mut stopped = Vec::new();
        assert!(!attention.update(
            vec![],
            true,
            |_| Some(Other(DesktopId(2))),
            |w| stopped.push(w)
        ));
        assert_eq!(stopped, [window(1)]);
    }
}
