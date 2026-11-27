//! Navigate existing desktops using Explorer's registry snapshot and native
//! keyboard shortcut when its private COM layout is unavailable.

use std::collections::HashSet;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows::core::GUID;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetThreadDesktop, OpenInputDesktop,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows_sys::core::{PCWSTR, w};

use super::DesktopDestination;
use super::shortcuts::NATIVE_DESKTOP_INPUT_TAG;
use crate::native::desktops::{DesktopId, object_name};

const REGISTRY_KEY: PCWSTR =
    w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops");
const STEP_TIMEOUT: Duration = Duration::from_secs(2);
const STEP_INTERVAL: Duration = Duration::from_millis(250);

fn registry_guids(name: PCWSTR) -> Option<Vec<GUID>> {
    let mut bytes = 0;
    let read = |buffer, bytes: &mut u32| unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            REGISTRY_KEY,
            name,
            RRF_RT_REG_BINARY,
            null_mut(),
            buffer,
            bytes,
        )
    };
    if read(null_mut(), &mut bytes) != ERROR_SUCCESS
        || bytes == 0
        || bytes > 1024 * 1024
        || !(bytes as usize).is_multiple_of(size_of::<GUID>())
    {
        return None;
    }
    let mut values = vec![GUID::zeroed(); bytes as usize / size_of::<GUID>()];
    let expected = bytes;
    (read(values.as_mut_ptr().cast(), &mut bytes) == ERROR_SUCCESS && bytes == expected)
        .then_some(values)
}

fn desktop_order() -> Option<(DesktopId, Vec<DesktopId>)> {
    let current = || {
        let values = registry_guids(w!("CurrentVirtualDesktop"))?;
        let [id] = values.as_slice() else { return None };
        Some(DesktopId(id.to_u128()))
    };
    let before = current()?;
    let order: Vec<_> = registry_guids(w!("VirtualDesktopIDs"))?
        .iter()
        .map(|id| DesktopId(id.to_u128()))
        .collect();
    let mut seen = HashSet::new();
    (order.iter().all(|&id| id.0 != 0 && seen.insert(id))
        && seen.contains(&before)
        && current()? == before)
        .then_some((before, order))
}

fn interactive_shell() -> Option<(isize, u32)> {
    if !object_name(unsafe { GetProcessWindowStation() })
        .ok()?
        .eq_ignore_ascii_case("WinSta0")
        || !object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })
            .ok()?
            .eq_ignore_ascii_case("Default")
    {
        return None;
    }
    let tray = unsafe { FindWindowW(w!("Shell_TrayWnd"), null()) } as isize;
    let owner = crate::native::pid(tray);
    (tray != 0 && owner != 0).then_some((tray, owner))
}

pub(super) fn snapshot() -> Option<(DesktopId, Vec<DesktopId>)> {
    interactive_shell()?;
    on_input_desktop().then(desktop_order)?
}

fn on_input_desktop() -> bool {
    let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if input.is_null() {
        return false;
    }
    let default = object_name(input).is_ok_and(|name| name.eq_ignore_ascii_case("Default"));
    unsafe { CloseDesktop(input) };
    default
}

fn keys_released() -> bool {
    [
        VK_LWIN, VK_RWIN, VK_CONTROL, VK_MENU, VK_SHIFT, VK_LEFT, VK_RIGHT,
    ]
    .iter()
    .all(|&key| unsafe { GetAsyncKeyState(i32::from(key)) } >= 0)
}

fn send_step(next: bool) -> bool {
    let key = |vk, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                dwExtraInfo: NATIVE_DESKTOP_INPUT_TAG,
                ..Default::default()
            },
        },
    };
    let arrow = if next { VK_RIGHT } else { VK_LEFT };
    let inputs = [
        key(VK_LCONTROL, 0),
        key(VK_LWIN, KEYEVENTF_EXTENDEDKEY),
        key(arrow, KEYEVENTF_EXTENDEDKEY),
        key(arrow, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
        key(VK_LWIN, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
        key(VK_LCONTROL, KEYEVENTF_KEYUP),
    ];
    let send = |batch: &[INPUT]| unsafe {
        SendInput(
            batch.len() as u32,
            batch.as_ptr(),
            size_of::<INPUT>() as i32,
        ) as usize
    };
    let sent = send(&inputs);
    if (1..inputs.len()).contains(&sent) {
        // The ups mirror the downs. Release only keys actually pressed by the partial batch.
        send(&inputs[sent.max(inputs.len() - sent)..]);
    }
    sent == inputs.len()
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Progress {
    Pending,
    Arrived(usize),
    Failed,
}

pub(super) struct Navigation {
    shell: (isize, u32),
    order: Vec<DesktopId>,
    current: usize,
    destination: usize,
    awaiting: Option<usize>,
    next_send: Instant,
    deadline: Instant,
}

impl Navigation {
    pub(super) fn begin(destination: DesktopDestination) -> Option<Self> {
        let shell = interactive_shell()?;
        if !on_input_desktop() {
            return None;
        }
        let (current, order) = desktop_order()?;
        Self::new(shell, current, order, destination, Instant::now())
    }

    fn new(
        shell: (isize, u32),
        current: DesktopId,
        order: Vec<DesktopId>,
        destination: DesktopDestination,
        now: Instant,
    ) -> Option<Self> {
        let origin = order.iter().position(|&id| id == current)?;
        let last = order.len() - 1;
        let destination = match destination {
            DesktopDestination::Number(n) => n.checked_sub(1)?.min(last),
            DesktopDestination::Relative { next: true } => (origin + 1).min(last),
            DesktopDestination::Relative { next: false } => origin.saturating_sub(1),
        };
        Some(Self {
            shell,
            order,
            current: origin,
            destination,
            awaiting: None,
            next_send: now,
            deadline: now + STEP_TIMEOUT,
        })
    }

    pub(super) fn stop_after_current_step(&mut self) {
        self.destination = self.awaiting.unwrap_or(self.current);
    }

    pub(super) fn poll(&mut self) -> Progress {
        if interactive_shell() != Some(self.shell) || !on_input_desktop() {
            return Progress::Failed;
        }
        let Some((current, order)) = desktop_order() else {
            return Progress::Failed;
        };
        self.advance(current, &order, Instant::now(), keys_released, send_step)
    }

    fn advance(
        &mut self,
        current: DesktopId,
        order: &[DesktopId],
        now: Instant,
        ready: impl FnOnce() -> bool,
        send: impl FnOnce(bool) -> bool,
    ) -> Progress {
        if order != self.order {
            return Progress::Failed;
        }
        if let Some(expected) = self.awaiting
            && current == order[expected]
        {
            self.current = expected;
            self.awaiting = None;
            self.deadline = now + STEP_TIMEOUT;
        }
        if current != order[self.current] {
            return Progress::Failed;
        }
        if self.current == self.destination {
            // Give the last native switch the same settling interval as an
            // intermediate step before the queue can start another request.
            return if now >= self.next_send {
                Progress::Arrived(self.current + 1)
            } else {
                Progress::Pending
            };
        }
        if now >= self.deadline {
            return Progress::Failed;
        }
        if self.awaiting.is_some() || now < self.next_send || !ready() {
            return Progress::Pending;
        }

        let next = self.current < self.destination;
        if !send(next) {
            return Progress::Failed;
        }
        self.awaiting = Some(if next {
            self.current + 1
        } else {
            self.current - 1
        });
        self.next_send = now + STEP_INTERVAL;
        self.deadline = now + STEP_TIMEOUT;
        Progress::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_each_adjacent_step_and_never_repeats_an_unconfirmed_arrow() {
        let order = [DesktopId(1), DesktopId(2), DesktopId(3)];
        let now = Instant::now();
        let mut nav = Navigation::new(
            (1, 1),
            order[0],
            order.to_vec(),
            DesktopDestination::Number(9),
            now,
        )
        .unwrap();
        let no_input = |_| panic!("unexpected input");
        assert_eq!(
            nav.advance(order[0], &order, now, || false, no_input),
            Progress::Pending
        );
        assert_eq!(
            nav.advance(order[0], &order, now, || true, |next| next),
            Progress::Pending
        );
        assert_eq!(
            nav.advance(order[0], &order, now + STEP_INTERVAL / 4, || true, no_input),
            Progress::Pending
        );
        assert_eq!(
            nav.advance(order[1], &order, now + STEP_INTERVAL / 2, || true, no_input),
            Progress::Pending
        );
        assert_eq!(
            nav.advance(order[1], &order, now + STEP_INTERVAL, || true, |next| next),
            Progress::Pending
        );
        assert_eq!(
            nav.advance(order[2], &order, now + 2 * STEP_INTERVAL, || true, no_input),
            Progress::Arrived(3)
        );
    }

    #[test]
    fn superseded_jump_stops_after_the_step_already_sent() {
        let now = Instant::now();
        let order = [DesktopId(1), DesktopId(2), DesktopId(3)];
        let start = || {
            Navigation::new(
                (1, 1),
                order[0],
                order.to_vec(),
                DesktopDestination::Number(3),
                now,
            )
            .unwrap()
        };
        let no_input = |_| panic!("superseded jump emitted more input");
        let mut idle = start();
        idle.stop_after_current_step();
        assert_eq!(
            idle.advance(order[0], &order, now, || true, no_input),
            Progress::Arrived(1)
        );
        let mut moving = start();
        assert_eq!(
            moving.advance(order[0], &order, now, || true, |next| next),
            Progress::Pending
        );
        moving.stop_after_current_step();
        assert_eq!(
            moving.advance(order[0], &order, now + STEP_INTERVAL, || true, no_input),
            Progress::Pending
        );
        assert_eq!(
            moving.advance(order[1], &order, now + STEP_INTERVAL, || true, no_input),
            Progress::Arrived(2)
        );
    }

    #[test]
    fn stops_on_reorder_unexpected_selection_failed_input_and_timeout() {
        let order = [DesktopId(1), DesktopId(2), DesktopId(3)];
        let now = Instant::now();
        let start = || {
            Navigation::new(
                (1, 1),
                order[2],
                order.to_vec(),
                DesktopDestination::Relative { next: false },
                now,
            )
            .unwrap()
        };
        let no_input = |_| panic!("unexpected input");
        assert_eq!(
            start().advance(order[0], &order, now, || true, no_input),
            Progress::Failed
        );
        assert_eq!(
            start().advance(
                order[2],
                &[order[1], order[0], order[2]],
                now,
                || true,
                no_input
            ),
            Progress::Failed
        );
        assert_eq!(
            start().advance(order[2], &order, now, || true, |_| false),
            Progress::Failed
        );
        let mut nav = start();
        assert_eq!(
            nav.advance(order[2], &order, now, || true, |next| !next),
            Progress::Pending
        );
        assert_eq!(
            nav.advance(order[2], &order, now + STEP_TIMEOUT, || true, no_input),
            Progress::Failed
        );
    }

    #[test]
    #[ignore = "switches to an existing neighboring desktop and back with native keyboard input"]
    fn native_navigation_round_trip() {
        fn finish(mut navigation: Navigation) -> Progress {
            loop {
                let progress = navigation.poll();
                if progress != Progress::Pending {
                    return progress;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        }

        let (from, order) = snapshot().expect("interactive Explorer");
        assert!(order.len() >= 2, "two existing desktops required");
        let origin = order.iter().position(|&id| id == from).unwrap();
        let neighbor = if origin > 0 { origin - 1 } else { 1 };
        let forward = Navigation::begin(DesktopDestination::Number(neighbor + 1)).unwrap();
        assert_eq!(forward.order, order);
        assert_eq!(forward.current, origin);
        let shell = forward.shell;
        let outward = finish(forward);
        let back = Navigation::begin(DesktopDestination::Number(origin + 1))
            .expect("Explorer still available");
        assert_eq!(back.shell, shell, "Explorer restarted");
        assert_eq!(back.order, order, "desktop order changed");
        if back.current == neighbor {
            assert_eq!(finish(back), Progress::Arrived(origin + 1));
        }
        assert_eq!(snapshot(), Some((from, order)));
        assert_eq!(outward, Progress::Arrived(neighbor + 1));
    }
}
