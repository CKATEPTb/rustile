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
}

pub(super) fn reset() {
    SCROLL.set(Scroll::default());
}

pub(super) fn handle(delta: i16, modifiers: impl FnOnce() -> u32, mask: impl FnOnce()) -> bool {
    if delta == 0 {
        return false;
    }
    let key = if delta > 0 { WHEEL_UP } else { WHEEL_DOWN };
    let (state, row, revision) = route_snapshot(usize::from(key));
    let Some(row) = row else {
        reset();
        return false;
    };
    let mods = modifiers();
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
