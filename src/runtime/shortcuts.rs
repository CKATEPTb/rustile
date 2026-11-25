use std::cell::Cell;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::{Mutex, mpsc};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::StationsAndDesktops::{GetThreadDesktop, HDESK, SetThreadDesktop};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{CANCEL, CONFIRM, EXPAND, FOCUS, KEY_COMMAND, MOVE, SHRINK, START_RESIZE};
use crate::manager::Mode;

// Only potential shortcuts have a slot. Modifiers themselves always pass
// through, so tapping Win still opens Start.
const KEYS: [u16; 6] = [VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_RETURN, VK_ESCAPE];
const ENTER: usize = 4;
const ESC: usize = 5;
const WIN_SHIFT: u32 = MOD_WIN | MOD_SHIFT;

#[derive(Debug, PartialEq)]
enum Action {
    Pass,
    Consume,
    Command(i32),
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Press {
    #[default]
    Up,
    Passed,
    Consumed,
    ResizeArrow,
}

#[derive(Clone, Copy, Default)]
struct Held([Press; KEYS.len()]);

impl Held {
    fn route(
        &mut self,
        slot: usize,
        down: bool,
        mods: u32,
        mode: Option<Mode>,
        opener: u32,
    ) -> Action {
        let press = &mut self.0[slot];
        let consumed = matches!(press, Press::Consumed | Press::ResizeArrow);
        if !down {
            *press = Press::Up;
            return if consumed {
                Action::Consume
            } else {
                Action::Pass
            };
        }
        if *press == Press::Passed {
            return Action::Pass;
        }
        let id = match (mode, slot, mods) {
            (Some(Mode::Normal), ENTER, m) if m == opener => Some(START_RESIZE),
            (Some(Mode::Normal), 0..=3, MOD_WIN) => Some(FOCUS + slot as i32),
            (Some(Mode::Normal), 0..=3, WIN_SHIFT) => Some(MOVE + slot as i32),
            (Some(Mode::Resize), 0..=3, 0 | MOD_WIN) => Some(EXPAND + slot as i32),
            (Some(Mode::Resize), 0..=3, MOD_SHIFT | WIN_SHIFT) => Some(SHRINK + slot as i32),
            (Some(Mode::Resize), ENTER, m) if m == 0 || m == opener => Some(CONFIRM),
            (Some(Mode::Resize), ESC, 0 | MOD_WIN) => Some(CANCEL),
            _ => None,
        };
        let repeatable = slot < ENTER && mode == Some(Mode::Resize);
        if let Some(id) = id
            && (!consumed || (*press == Press::ResizeArrow && repeatable))
        {
            *press = if repeatable {
                Press::ResizeArrow
            } else {
                Press::Consumed
            };
            return Action::Command(id);
        }
        if consumed {
            Action::Consume
        } else {
            *press = Press::Passed;
            Action::Pass
        }
    }
}

// The manager publishes only after foreground and layout validation. The lock
// protects a handful of scalar values and is never held across a Win32 call.
#[derive(Clone, Copy)]
struct Snapshot {
    target: isize,
    mode: Mode,
    focus_epoch: usize,
    handled: usize,
}

static SNAPSHOT: Mutex<Snapshot> = Mutex::new(Snapshot {
    target: 0,
    mode: Mode::Normal,
    focus_epoch: 0,
    handled: 0,
});

pub(super) fn update(target: Option<isize>, mode: Mode, handled: usize) {
    let mut state = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    let target = target.unwrap_or(0);
    if state.target != target {
        state.focus_epoch = state.focus_epoch.wrapping_add(1);
        state.target = target;
    }
    state.mode = mode;
    if handled != 0 {
        state.handled = handled;
    }
}

// A following key can arrive before the manager has even opened its HUD.
// Predict both opening and closing the mode until that exact command is
// acknowledged. An earlier acknowledgement cannot undo a later keypress.
#[derive(Clone, Copy)]
struct Pending {
    ticket: usize,
    focus_epoch: usize,
    mode: Mode,
}

impl Pending {
    const NEW: Self = Self {
        ticket: 0,
        focus_epoch: 0,
        mode: Mode::Normal,
    };

    fn mode(self, state: Snapshot) -> Mode {
        if self.ticket != 0 && self.ticket != state.handled && self.focus_epoch == state.focus_epoch
        {
            self.mode
        } else {
            state.mode
        }
    }

    fn transition(self, command: i32, focus_epoch: usize) -> Option<Self> {
        let mode = match command {
            START_RESIZE => Mode::Resize,
            CONFIRM | CANCEL => Mode::Normal,
            _ => return None,
        };
        // Low eight bits of WPARAM carry the command. Ticket zero denotes a
        // command that does not change mode.
        let ticket = (self.ticket % (usize::MAX >> 8)) + 1;
        Some(Self {
            ticket,
            focus_epoch,
            mode,
        })
    }
}

thread_local! {
    static OWNER: Cell<u32> = const { Cell::new(0) };
    static OPENER: Cell<u32> = const { Cell::new(MOD_WIN) };
    static HELD: Cell<Held> = const { Cell::new(Held([Press::Up; KEYS.len()])) };
    static PENDING: Cell<Pending> = const { Cell::new(Pending::NEW) };
}

fn modifiers() -> u32 {
    // These are modifier states preceding an arrow/Enter/Esc event. Do not
    // query the state of the key whose down/up transition is being delivered.
    let down = |vk: u16| unsafe { GetAsyncKeyState(vk.into()) } < 0;
    let mut mods = if down(VK_LWIN) || down(VK_RWIN) {
        MOD_WIN
    } else {
        0
    };
    for (key, flag) in [
        (VK_SHIFT, MOD_SHIFT),
        (VK_CONTROL, MOD_CONTROL),
        (VK_MENU, MOD_ALT),
    ] {
        if down(key) {
            mods |= flag;
        }
    }
    mods
}

fn mark_win_chord() {
    // The shell otherwise observes Win down/up alone and opens Start after a
    // consumed chord. This unused VK is also used by PowerToys Keyboard Manager.
    let dummy = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0xff,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let marker = [dummy(0), dummy(KEYEVENTF_KEYUP)];
    unsafe { SendInput(2, marker.as_ptr(), size_of::<INPUT>() as i32) };
}

unsafe extern "system" fn on_key(code: i32, message: usize, data: isize) -> isize {
    if code == HC_ACTION as i32 {
        // Read only the virtual key. No translations, strings or input logs.
        let vk = unsafe { (*(data as *const KBDLLHOOKSTRUCT)).vkCode };
        if let Some(slot) = KEYS.iter().position(|&k| u32::from(k) == vk) {
            let down = matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let state = *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
            let target = state.target;
            let active = target != 0 && target == unsafe { GetForegroundWindow() } as isize;
            let pending = PENDING.get();
            let mode = active.then(|| pending.mode(state));
            let mods = if down && active { modifiers() } else { 0 };
            let mut held = HELD.get();
            let previous = held.0[slot];
            match held.route(slot, down, mods, mode, OPENER.get()) {
                Action::Consume => {
                    HELD.set(held);
                    return 1;
                }
                Action::Command(id) => {
                    let transition = pending.transition(id, state.focus_epoch);
                    let ticket = transition.map_or(0, |next| next.ticket);
                    let command = (ticket << 8) | id as usize;
                    if unsafe { PostThreadMessageW(OWNER.get(), KEY_COMMAND, command, target) } != 0
                    {
                        HELD.set(held);
                        if let Some(next) = transition {
                            PENDING.set(next);
                        }
                        if previous == Press::Up && mods & MOD_WIN != 0 {
                            mark_win_chord();
                        }
                        return 1;
                    }
                    // Balance a consumed press even if the queue fills up.
                    if previous != Press::Up {
                        return 1;
                    }
                    held.0[slot] = Press::Passed;
                    HELD.set(held);
                }
                Action::Pass => HELD.set(held),
            }
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}

pub(super) struct Hook {
    thread: Option<JoinHandle<()>>,
    id: u32,
}

impl Hook {
    pub(super) fn install(opener: u32) -> Result<Self, String> {
        let owner = unsafe { GetCurrentThreadId() };
        let desktop = unsafe { GetThreadDesktop(owner) } as usize;
        let (ready, status) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("rustile-shortcuts".into())
            .spawn(move || {
                OWNER.set(owner);
                OPENER.set(opener);
                // Match the manager's desktop, including an isolated test desktop.
                if unsafe { SetThreadDesktop(desktop as HDESK) } == 0 {
                    let _ = ready.send(Err(io::Error::last_os_error()));
                    return;
                }
                let mut message = MSG::default();
                unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE) };
                let module = unsafe { GetModuleHandleW(null()) };
                let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), module, 0) };
                if hook.is_null() {
                    let _ = ready.send(Err(io::Error::last_os_error()));
                    return;
                }
                if ready.send(Ok(unsafe { GetCurrentThreadId() })).is_ok() {
                    while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
                        unsafe { DispatchMessageW(&message) };
                    }
                }
                unsafe { UnhookWindowsHookEx(hook) };
            })
            .map_err(|e| format!("Поток сочетаний клавиш: {e}"))?;
        match status.recv() {
            Ok(Ok(id)) => Ok(Self {
                thread: Some(thread),
                id,
            }),
            result => {
                let _ = thread.join();
                match result {
                    Ok(Err(e)) => Err(format!("Обработка клавиатурных сочетаний: {e}")),
                    _ => Err("Поток сочетаний клавиш неожиданно завершился.".into()),
                }
            }
        }
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        update(None, Mode::Normal, 0);
        unsafe { PostThreadMessageW(self.id, WM_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise the hook's prediction and repeat handling before/after arbitrary
    // manager acknowledgements, without sending keyboard events to Windows.
    struct Keyboard {
        held: Held,
        state: Snapshot,
        pending: Pending,
        opener: u32,
    }

    impl Keyboard {
        fn new() -> Self {
            Self {
                held: Held::default(),
                state: Snapshot {
                    target: 100,
                    mode: Mode::Normal,
                    focus_epoch: 1,
                    handled: 0,
                },
                pending: Pending::NEW,
                opener: MOD_WIN,
            }
        }

        fn key(&mut self, slot: usize, down: bool, mods: u32) -> Action {
            let mode = (self.state.target != 0).then(|| self.pending.mode(self.state));
            let action = self.held.route(slot, down, mods, mode, self.opener);
            if let Action::Command(id) = action
                && let Some(next) = self.pending.transition(id, self.state.focus_epoch)
            {
                self.pending = next;
            }
            action
        }
    }

    #[test]
    fn move_and_focus_are_distinct_and_do_not_autorepeat() {
        let mut keyboard = Keyboard::new();
        for slot in 0..4 {
            assert_eq!(
                keyboard.key(slot, true, WIN_SHIFT),
                Action::Command(MOVE + slot as i32)
            );
            assert_eq!(keyboard.key(slot, true, MOD_WIN), Action::Consume);
            assert_eq!(keyboard.key(slot, false, 0), Action::Consume);
            assert_eq!(
                keyboard.key(slot, true, MOD_WIN),
                Action::Command(FOCUS + slot as i32)
            );
            assert_eq!(keyboard.key(slot, true, MOD_WIN), Action::Consume);
            assert_eq!(keyboard.key(slot, false, 0), Action::Consume);
        }
    }

    #[test]
    fn quick_resize_cancellation_ignores_an_older_acknowledgement() {
        for mods in [0, MOD_WIN] {
            let mut keyboard = Keyboard::new();
            assert_eq!(
                keyboard.key(ENTER, true, MOD_WIN),
                Action::Command(START_RESIZE)
            );
            let opener_ticket = keyboard.pending.ticket;
            assert_eq!(keyboard.key(ESC, true, mods), Action::Command(CANCEL));
            // Only the opener has been handled on the manager thread so far.
            keyboard.state.handled = opener_ticket;
            keyboard.state.mode = Mode::Resize;
            assert_eq!(keyboard.key(ESC, true, 0), Action::Consume);
            assert_eq!(keyboard.key(0, true, 0), Action::Pass);
            assert_eq!(keyboard.key(0, false, 0), Action::Pass);
            assert_eq!(keyboard.key(ESC, false, 0), Action::Consume);
            assert_eq!(keyboard.key(0, true, MOD_WIN), Action::Command(FOCUS));
        }
    }

    #[test]
    fn windows_x_bypasses_the_shortcut_filter() {
        // on_key forwards keys outside this list before consulting modifiers or mode.
        assert!(!KEYS.contains(&VK_X));
    }

    #[test]
    fn opening_enter_cannot_confirm_until_released_and_resize_arrows_repeat() {
        let mut keyboard = Keyboard::new();
        assert_eq!(
            keyboard.key(ENTER, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        assert_eq!(keyboard.key(ENTER, true, 0), Action::Consume);
        assert_eq!(keyboard.key(0, true, MOD_WIN), Action::Command(EXPAND));
        assert_eq!(keyboard.key(0, true, 0), Action::Command(EXPAND));
        assert_eq!(keyboard.key(0, true, WIN_SHIFT), Action::Command(SHRINK));
        assert_eq!(keyboard.key(0, true, MOD_SHIFT), Action::Command(SHRINK));
        assert_eq!(keyboard.key(0, false, 0), Action::Consume);
        assert_eq!(keyboard.key(ENTER, false, 0), Action::Consume);
        assert_eq!(keyboard.key(ENTER, true, 0), Action::Command(CONFIRM));
        assert_eq!(keyboard.key(0, true, 0), Action::Pass);
    }

    #[test]
    fn focus_change_and_rejected_opener_restore_normal_routing() {
        let mut keyboard = Keyboard::new();
        assert_eq!(
            keyboard.key(ENTER, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        keyboard.state.handled = keyboard.pending.ticket;
        assert_eq!(keyboard.key(0, true, 0), Action::Pass);
        assert_eq!(keyboard.key(ENTER, false, 0), Action::Consume);
        assert_eq!(
            keyboard.key(ENTER, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        keyboard.state.target = 0;
        keyboard.state.focus_epoch += 1;
        assert_eq!(keyboard.key(ENTER, false, 0), Action::Consume);
        assert_eq!(keyboard.key(ENTER, true, MOD_WIN), Action::Pass);
        assert_eq!(keyboard.key(ENTER, false, 0), Action::Pass);
        assert_eq!(keyboard.key(1, true, WIN_SHIFT), Action::Pass);
        keyboard.state.target = 100;
        keyboard.state.focus_epoch += 1;
        assert_eq!(keyboard.key(2, true, 0), Action::Pass);
    }

    #[test]
    fn native_keypresses_keep_their_repeats_and_up_even_if_modifiers_change() {
        let mut keyboard = Keyboard::new();
        for mods in [0, MOD_SHIFT, MOD_CONTROL, MOD_ALT, MOD_CONTROL | MOD_WIN] {
            assert_eq!(keyboard.key(1, true, mods), Action::Pass);
            assert_eq!(keyboard.key(1, true, MOD_WIN), Action::Pass);
            assert_eq!(keyboard.key(1, false, MOD_WIN), Action::Pass);
        }
        assert_eq!(keyboard.key(0, true, MOD_CONTROL | WIN_SHIFT), Action::Pass);
        assert_eq!(keyboard.key(0, false, 0), Action::Pass);
        keyboard.opener = MOD_CONTROL | MOD_ALT;
        assert_eq!(keyboard.key(ENTER, true, MOD_WIN), Action::Pass);
        assert_eq!(keyboard.key(ENTER, false, 0), Action::Pass);
        assert_eq!(
            keyboard.key(ENTER, true, MOD_CONTROL | MOD_ALT),
            Action::Command(START_RESIZE)
        );
    }
}
