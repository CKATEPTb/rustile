use super::*;
use crate::settings::{WHEEL_DOWN, WHEEL_UP};

#[derive(Clone, Copy)]
struct Gesture {
    key: u16,
    binding: BoundCommand,
    foreground: isize,
    revision: usize,
}

impl Gesture {
    fn continues(self, previous: Self) -> bool {
        if self.key != previous.key || self.revision != previous.revision {
            return false;
        }
        // A desktop step itself changes the foreground window. The rest of
        // its detent still belongs to the same held navigation gesture.
        if DesktopDestination::from_command(self.binding.id).is_some() {
            return self.binding.id == previous.binding.id
                && self.binding.mods == previous.binding.mods;
        }
        self.binding == previous.binding && self.foreground == previous.foreground
    }
}

#[derive(Clone, Copy, Default)]
struct Scroll {
    gesture: Option<Gesture>,
    remainder: u32,
}

impl Scroll {
    fn advance(&mut self, delta: i16, gesture: Gesture) -> (u32, bool) {
        let first = self
            .gesture
            .is_none_or(|previous| !gesture.continues(previous));
        if first {
            self.gesture = Some(gesture);
            self.remainder = 0;
        }
        let total = self.remainder + u32::from(delta.unsigned_abs());
        self.remainder = total % WHEEL_DELTA;
        (total / WHEEL_DELTA, first)
    }
}

thread_local! {
    static SCROLL: Cell<Scroll> = const { Cell::new(Scroll { gesture: None, remainder: 0 }) };
    // A wheel detent can switch desktops or reset while the same Win press is held.
    static USED_WIN: Cell<u8> = const { Cell::new(0) };
}

const LEFT_WIN: u8 = 1;
const RIGHT_WIN: u8 = 2;
const WIN_RELEASE_TAG: usize = u32::from_be_bytes(*b"RsWl") as usize;

pub(super) fn reset() {
    SCROLL.set(Scroll::default());
}

pub(super) fn win_key(
    event: &KBDLLHOOKSTRUCT,
    down: bool,
    mut send: impl FnMut(&[INPUT]) -> u32,
) -> bool {
    if !down {
        if event.dwExtraInfo == WIN_RELEASE_TAG && event.flags & LLKHF_INJECTED != 0 {
            return false;
        }
        reset();
    }
    let used = USED_WIN.get();
    if used == 0 {
        return false;
    }
    let side = match event.vkCode as u16 {
        VK_LWIN => LEFT_WIN,
        VK_RWIN => RIGHT_WIN,
        _ => return false,
    };
    if down {
        USED_WIN.set(used | side);
        // A new press of the other Win key must still reach Windows. Typematic
        // repeats of the held key can otherwise re-arm Start after the detent.
        return used & side != 0;
    }
    if used & side == 0 {
        return false;
    }
    USED_WIN.set(used & !side);
    let release = keyboard_input(KEYBDINPUT {
        wVk: event.vkCode as u16,
        wScan: event.scanCode as u16,
        dwFlags: KEYEVENTF_KEYUP
            | if event.flags & LLKHF_EXTENDED != 0 {
                KEYEVENTF_EXTENDEDKEY
            } else {
                0
            },
        dwExtraInfo: WIN_RELEASE_TAG,
        ..Default::default()
    });
    // Deliver the mask and the matching Win-up together on the destination
    // desktop. Forward the physical release if Windows didn't queue its copy.
    let batch = [menu_marker(0), menu_marker(KEYEVENTF_KEYUP), release];
    match send(&batch) {
        3 => true,
        1 => {
            send(&batch[1..2]);
            false
        }
        _ => false,
    }
}

pub(super) fn handle(
    delta: i16,
    modifiers: impl FnOnce() -> (u32, u8),
    mask: impl FnOnce(),
) -> bool {
    if delta == 0 {
        return false;
    }
    let key = if delta > 0 { WHEEL_UP } else { WHEEL_DOWN };
    let (state, row, revision) = route_snapshot(usize::from(key));
    let Some(row) = row else {
        reset();
        return false;
    };
    let (mods, win_keys) = modifiers();
    let mut foreground = 0;
    let pending = PENDING.get();
    let Some(binding) = row.binding(state, pending, mods, || {
        foreground = unsafe { GetForegroundWindow() } as isize;
        foreground
    }) else {
        reset();
        return false;
    };
    let gesture = Gesture {
        key,
        binding,
        foreground,
        revision,
    };
    let mut scroll = SCROLL.get();
    let (steps, first) = scroll.advance(delta, gesture);
    SCROLL.set(scroll);
    if first && mods & (MOD_WIN | MOD_ALT) != 0 {
        USED_WIN.set(USED_WIN.get() | win_keys);
        mask();
    }
    // One input packet may contain several detents. Resolve mode-changing
    // bindings against the preceding detent's ticket, keeping the same HWND.
    for _ in 0..steps {
        let next = PENDING.get();
        let command = if next.ticket == pending.ticket {
            binding
        } else if let Some(command) = row.binding(state, next, mods, || foreground) {
            command
        } else {
            break;
        };
        command.post(next);
    }
    true
}

pub(super) unsafe extern "system" fn on_mouse(code: i32, message: usize, data: isize) -> isize {
    // Ordinary pointer movement takes the fast path without even reading the
    // mouse structure. The high word of mouseData is a signed wheel delta.
    if code == HC_ACTION as i32
        && matches!(
            message as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        end_transfer_focus_watch();
        end_desktop_focus_watch();
    }
    if code == HC_ACTION as i32 && message as u32 == WM_MOUSEWHEEL {
        let delta = (unsafe { (*(data as *const MSLLHOOKSTRUCT)).mouseData } >> 16) as i16;
        if handle(delta, modifiers, mark_modifier_used) {
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win_event(key: u16, scan: u32) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: key.into(),
            scanCode: scan,
            flags: LLKHF_EXTENDED,
            ..Default::default()
        }
    }

    #[test]
    fn win_repeats_and_overlapping_sides_only_mask_the_used_hold() {
        let left = win_event(VK_LWIN, 0x5b);
        let right = win_event(VK_RWIN, 0x5c);
        let no_input = |_: &[INPUT]| panic!("unexpected replacement input");
        USED_WIN.set(RIGHT_WIN);
        assert!(win_key(&right, true, no_input));
        assert!(
            !win_key(&left, true, no_input),
            "first down of the other side"
        );
        assert!(win_key(&right, false, |batch| {
            let release = unsafe { batch[2].Anonymous.ki };
            assert_eq!((release.wVk, release.wScan), (VK_RWIN, 0x5c));
            assert_eq!(release.dwFlags, KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY);
            assert_eq!(release.dwExtraInfo, WIN_RELEASE_TAG);
            batch.len() as u32
        }));
        let echo = KBDLLHOOKSTRUCT {
            dwExtraInfo: WIN_RELEASE_TAG,
            flags: LLKHF_INJECTED | LLKHF_EXTENDED | LLKHF_UP,
            ..right
        };
        // A new physical press can arrive before the queued release is delivered.
        assert!(!win_key(&right, true, no_input));
        assert!(!win_key(&echo, false, no_input));
        assert!(win_key(&left, false, |_| 3));
        assert!(win_key(&right, false, |_| 3));
        assert!(!win_key(&right, true, no_input));
        assert!(!win_key(&right, false, no_input));
    }

    #[test]
    fn failed_or_partial_injection_forwards_the_physical_win_release() {
        let right = win_event(VK_RWIN, 0x5c);
        for inserted in 0..3 {
            USED_WIN.set(RIGHT_WIN);
            let mut cleanup = false;
            assert!(!win_key(&right, false, |batch| {
                if batch.len() == 3 {
                    return inserted;
                }
                assert_eq!(batch.len(), 1);
                let key = unsafe { batch[0].Anonymous.ki };
                assert_eq!((key.wVk, key.dwFlags), (0xff, KEYEVENTF_KEYUP));
                cleanup = true;
                1
            }));
            assert_eq!(cleanup, inserted == 1);
            assert_eq!(USED_WIN.get(), 0);
        }
    }

    fn gesture(key: u16) -> Gesture {
        Gesture {
            key,
            binding: BoundCommand {
                id: if key == WHEEL_UP {
                    DESKTOP_PREVIOUS
                } else {
                    DESKTOP_NEXT
                },
                target: Some(0),
                focus_epoch: 1,
                mods: MOD_WIN,
                predict_resize: false,
            },
            foreground: 100,
            revision: 0,
        }
    }

    #[test]
    fn wheel_accumulates_detents_and_starts_fresh_when_direction_changes() {
        let mut scroll = Scroll::default();
        let up = gesture(WHEEL_UP);
        let down = gesture(WHEEL_DOWN);
        assert_eq!(scroll.advance(40, up), (0, true));
        assert_eq!(scroll.advance(40, up), (0, false));
        assert_eq!(scroll.advance(70, up), (1, false));
        assert_eq!(scroll.advance(240, up), (2, false));
        assert_eq!(scroll.advance(-90, down), (0, true));
        assert_eq!(scroll.advance(-30, down), (1, false));
    }

    #[test]
    fn partial_detents_do_not_cross_focus_or_settings_changes() {
        let mut up = gesture(WHEEL_UP);
        up.binding.id = FOCUS;
        up.binding.target = Some(up.foreground);
        let contexts = [
            Gesture {
                foreground: 200,
                ..up
            },
            Gesture { revision: 1, ..up },
            Gesture {
                binding: BoundCommand {
                    mods: MOD_ALT,
                    ..up.binding
                },
                ..up
            },
        ];
        for changed in contexts {
            let mut scroll = Scroll::default();
            assert_eq!(scroll.advance(100, up), (0, true));
            assert_eq!(scroll.advance(20, changed), (0, true));
            assert_eq!(scroll.advance(100, changed), (1, false));
        }
    }

    #[test]
    fn scrolling_across_desktops_keeps_partial_detents_as_the_destination_takes_focus() {
        let down = gesture(WHEEL_DOWN);
        let mut scroll = Scroll::default();
        assert_eq!(scroll.advance(-90, down), (0, true));
        assert_eq!(scroll.advance(-90, down), (1, false));
        let arrived = Gesture {
            foreground: 200,
            binding: BoundCommand {
                focus_epoch: 2,
                predict_resize: true,
                ..down.binding
            },
            ..down
        };
        assert_eq!(scroll.advance(-60, arrived), (1, false));
        assert_eq!(scroll.advance(-60, arrived), (0, false));
        assert_eq!(
            scroll.advance(
                -60,
                Gesture {
                    revision: 1,
                    ..arrived
                }
            ),
            (0, true)
        );
    }
}
