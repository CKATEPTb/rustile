//! Deliver Rustile desktop commands via WM_HOTKEY when the chord is available.
//! The keyboard hook handles Windows' native desktop arrows.

use super::*;
use std::collections::VecDeque;

fn can_register(chord: Chord, command: i32) -> bool {
    // Letting these physical keys through can start Explorer's slide animation.
    native_desktop_command(chord).is_none()
        && chord.key != VK_F12
        && (DesktopDestination::from_command(command).is_some() || is_desktop_move(command))
}

// Unique per chord, within RegisterHotKey's application ID range (0..0xbfff).
fn hotkey_id(chord: Chord) -> i32 {
    0x1000 + i32::from(chord.key) * 16 + chord.mods as i32
}

struct Captured {
    chord: Chord,
    command: usize,
    target: isize,
    time: u32,
}

#[derive(Default)]
pub(super) struct Hotkeys {
    owned: Vec<Chord>,
    pending: VecDeque<Captured>,
}

impl Hotkeys {
    pub(super) fn configure(&mut self, routes: &Routes) {
        let mut wanted = Vec::new();
        for key in 0..=u8::MAX {
            let Some(row) = routes.get(usize::from(key)) else {
                continue;
            };
            for (mods, &action) in row.normal.iter().enumerate() {
                let id = i32::from(action);
                let chord = Chord {
                    key: key.into(),
                    mods: mods as u32,
                };
                if can_register(chord, id) {
                    wanted.push(chord);
                }
            }
        }
        self.owned.retain(|chord| {
            if wanted.contains(chord) {
                return true;
            }
            unsafe { UnregisterHotKey(null_mut(), hotkey_id(*chord)) };
            false
        });
        self.pending.retain(|c| self.owned.contains(&c.chord));
        for chord in wanted {
            if !self.owned.contains(&chord)
                && unsafe {
                    RegisterHotKey(
                        null_mut(),
                        hotkey_id(chord),
                        chord.mods | MOD_NOREPEAT,
                        chord.key.into(),
                    )
                } != 0
            {
                self.owned.push(chord);
            }
        }
    }

    pub(super) fn translate(&mut self, msg: &mut MSG) -> bool {
        let chord = Chord {
            key: ((msg.lParam as usize >> 16) & 0xffff) as u16,
            mods: (msg.lParam as u32 & 0xffff) & !MOD_NOREPEAT,
        };
        if msg.wParam != hotkey_id(chord) as usize || !self.owned.contains(&chord) {
            return false;
        }
        // A nested Windows modal loop can drain a thread message. Use the
        // input timestamps so a later tap cannot move the old tap's window.
        let Some(age) = self
            .pending
            .iter()
            .filter(|c| c.chord == chord)
            .map(|c| msg.time.wrapping_sub(c.time))
            .filter(|&age| age <= i32::MAX as u32)
            .min()
        else {
            return false;
        };
        self.pending.retain(|c| {
            let other_age = msg.time.wrapping_sub(c.time);
            c.chord != chord || other_age > i32::MAX as u32 || other_age <= age
        });
        let index = self
            .pending
            .iter()
            .position(|c| c.chord == chord && msg.time.wrapping_sub(c.time) == age)
            .unwrap();
        let captured = self.pending.remove(index).unwrap();
        msg.message = KEY_COMMAND;
        msg.wParam = captured.command;
        msg.lParam = captured.target;
        true
    }

    fn capture(&mut self, press: Captured) {
        // A nested modal loop may consume WM_HOTKEY without entering the
        // manager pump. Keep the backlog bounded for a long-lived dialog.
        if self.pending.len() == 256 {
            self.pending.pop_front();
        }
        self.pending.push_back(press);
    }
}

pub(super) fn owned_chord(key: u16, binding: BoundCommand) -> Option<Chord> {
    let chord = Chord {
        key,
        mods: binding.mods,
    };
    if !can_register(chord, binding.id) {
        return None;
    }
    let routing = ROUTING.lock().unwrap_or_else(|e| e.into_inner());
    routing.registered.owned.contains(&chord).then_some(chord)
}

pub(super) fn accept(chord: Chord, binding: BoundCommand, pending: Pending, time: u32) {
    let mut routing = ROUTING.lock().unwrap_or_else(|e| e.into_inner());
    if routing.registered.owned.contains(&chord)
        && let Some(target) = binding.target
    {
        binding.record_gesture();
        let (command, transition) = binding.message(pending);
        routing.registered.capture(Captured {
            chord,
            command,
            target,
            time,
        });
        if let Some(next) = transition {
            PENDING.set(next);
        }
    }
}

#[cfg(test)]
pub(in crate::runtime) fn test_registered_ownership(hook: &Hook, settings: &Settings, owner: u32) {
    // Caller verified its private Win32 desktop and released the deliberately
    // conflicting registration. Probe User32 ownership without SendInput.
    let chord = Chord {
        key: VK_F17,
        mods: 0,
    };
    let probe = || unsafe { RegisterHotKey(null_mut(), 0x7000, 0, chord.key.into()) };
    let message = || MSG {
        message: WM_HOTKEY,
        wParam: hotkey_id(chord) as usize,
        lParam: (chord.key as isize) << 16,
        ..Default::default()
    };
    hook.set_settings(settings);
    assert_eq!(
        probe(),
        0,
        "Rustile owns the available desktop chord in User32"
    );
    assert!(!hook.translate_hotkey(&mut message()), "no recorded input");
    let before = desktop_gesture_generation();
    assert_eq!(test_keyboard_event(chord.key, true, owner), 0);
    assert_eq!(desktop_gesture_generation(), before.wrapping_add(1));
    assert_eq!(
        test_keyboard_event(chord.key, true, owner),
        1,
        "suppress repeat"
    );
    let mut raw = message();
    assert!(hook.translate_hotkey(&mut raw));
    assert_eq!(raw.message, KEY_COMMAND);
    assert_eq!((raw.wParam & 0xff) as i32, MOVE_DESKTOP_NEXT);
    assert_ne!(raw.wParam >> 8, 0, "preserve resize-mode ticket");
    assert_eq!(raw.lParam, 0, "windowless transfer");
    assert!(!hook.translate_hotkey(&mut message()), "single delivery");
    assert_eq!(
        unsafe { PeekMessageW(&mut raw, null_mut(), KEY_COMMAND, KEY_COMMAND, PM_REMOVE) },
        0,
        "no duplicate hook command"
    );
    hook.set_settings(&Settings::default());
    assert_eq!(
        test_keyboard_event(chord.key, true, owner),
        1,
        "held across unbind"
    );
    assert_eq!(
        test_keyboard_event(chord.key, false, owner),
        0,
        "matching release reaches Windows"
    );
    assert_ne!(probe(), 0, "unbinding releases the system registration");
    unsafe { UnregisterHotKey(null_mut(), 0x7000) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_arrows_post_one_command_and_never_forward_the_assigned_press() {
        use crate::settings::{Assignment, Command};

        let mods = MOD_CONTROL | MOD_WIN;
        let state = Snapshot {
            mode: Mode::Normal,
            target: None,
            focus_epoch: 0,
            handled: 0,
            desktops_available: true,
        };
        for (key, action, id) in [
            (VK_LEFT, Shortcut::PreviousDesktop, DESKTOP_PREVIOUS),
            (VK_RIGHT, Shortcut::NextDesktop, DESKTOP_NEXT),
        ] {
            let chord = Chord { key, mods };
            let settings = Settings::default();
            let routes = Routes::new(&settings);
            let row = routes.get(usize::from(key)).unwrap();
            let binding = row.binding(state, Pending::NEW, mods, || 0).unwrap();
            let mut press = Press::Up;
            assert_eq!(press.route(true, Some(binding)), Action::Command(id));
            assert!(!can_register(chord, id));
            assert!(
                owned_chord(key, binding).is_none(),
                "use the hook's consuming path"
            );
            let numbered = Chord {
                key: VK_1,
                mods: MOD_WIN,
            };
            assert!(can_register(numbered, DESKTOP_FIRST));

            let settings = settings
                .changed(Command::Assign(Assignment::Edit(
                    action,
                    chord,
                    Chord {
                        key: VK_F8,
                        mods: MOD_ALT,
                    },
                )))
                .unwrap();
            let routes = Routes::new(&settings);
            let binding = routes
                .get(usize::from(key))
                .and_then(|row| row.binding(state, Pending::NEW, mods, || 0));
            assert!(binding.is_none(), "old shortcut was unbound");
            assert_eq!(press.route(true, binding), Action::Consume, "held repeat");
            assert_eq!(
                press.route(false, None),
                Action::Consume,
                "matching release"
            );
            assert_eq!(
                press.route(true, binding),
                Action::Pass,
                "fresh unbound press"
            );
            assert_eq!(press.route(false, None), Action::Pass);
        }
    }

    #[test]
    fn a_registered_transfer_uses_the_matching_press_after_a_modal_loop_or_a_clock_wrap() {
        let chord = Chord {
            key: VK_1,
            mods: MOD_WIN | MOD_SHIFT,
        };
        let captured = |time, target| Captured {
            chord,
            time,
            target,
            command: MOVE_DESKTOP_FIRST as usize,
        };
        let mut keys = Hotkeys {
            owned: vec![chord],
            pending: VecDeque::from([
                captured(u32::MAX - 200, 1),
                captured(u32::MAX - 5, 2),
                captured(40, 3),
            ]),
        };
        let message = |time| MSG {
            message: WM_HOTKEY,
            wParam: hotkey_id(chord) as usize,
            lParam: ((chord.key as isize) << 16) | chord.mods as isize,
            time,
            ..Default::default()
        };
        let mut first = message(2);
        assert!(keys.translate(&mut first));
        assert_eq!(first.lParam, 2);
        assert!(!keys.translate(&mut message(2)));
        let mut next = message(40);
        assert!(keys.translate(&mut next));
        assert_eq!(next.lParam, 3);
    }
}
