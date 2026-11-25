use std::cell::Cell;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::{LazyLock, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::StationsAndDesktops::{GetThreadDesktop, HDESK, SetThreadDesktop};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{
    CANCEL, CONFIRM, EXPAND, FOCUS, KEY_COMMAND, MOVE, QUICK_RESIZE, QUICK_REVERSE, SHRINK,
    START_RESIZE,
};
use crate::manager::Mode;
use crate::settings::{Chord, Settings, Shortcut};

// The table is rebuilt only on a settings change. The hook inspects virtual
// key codes and current modifiers; it never translates or records typed text.
#[derive(Clone, Copy, Default)]
struct KeyRoutes {
    normal: [u8; 16],
    resize: [u8; 16],
}

impl KeyRoutes {
    fn for_mode(&mut self, action: Shortcut) -> &mut [u8; 16] {
        if action.is_resize() {
            &mut self.resize
        } else {
            &mut self.normal
        }
    }

    fn command(self, mode: Mode, mods: u32) -> Option<i32> {
        let commands = match mode {
            Mode::Normal => self.normal,
            Mode::Resize => self.resize,
        };
        commands
            .get(mods as usize)
            .copied()
            .filter(|&id| id != 0)
            .map(i32::from)
    }
}

struct Routes([Option<KeyRoutes>; 256]);

impl Routes {
    fn new(settings: Settings) -> Self {
        let mut keys = [None; 256];
        for action in Shortcut::ALL {
            let id = command_id(action);
            let Some(chord) = settings.binding(action) else {
                continue;
            };
            let row = keys[usize::from(chord.key)].get_or_insert_with(KeyRoutes::default);
            if action.is_resize() {
                for mods in 0..16 {
                    if settings.resize_alias(
                        action,
                        Chord {
                            mods,
                            key: chord.key,
                        },
                    ) {
                        row.resize[mods as usize] = id;
                    }
                }
            }
        }
        // An explicitly assigned modal chord wins over the implicit variants
        // that let a user keep the opener's modifiers held in resize mode.
        for action in Shortcut::ALL {
            let Some(chord) = settings.binding(action) else {
                continue;
            };
            let row = keys[usize::from(chord.key)].get_or_insert_with(KeyRoutes::default);
            row.for_mode(action)[chord.mods as usize] = command_id(action);
        }
        Self(keys)
    }
}

fn command_id(action: Shortcut) -> u8 {
    use Shortcut::*;
    let id = match action {
        FocusLeft => FOCUS,
        FocusRight => FOCUS + 1,
        FocusUp => FOCUS + 2,
        FocusDown => FOCUS + 3,
        MoveLeft => MOVE,
        MoveRight => MOVE + 1,
        MoveUp => MOVE + 2,
        MoveDown => MOVE + 3,
        StartResize => START_RESIZE,
        GrowLeft => EXPAND,
        GrowRight => EXPAND + 1,
        GrowUp => EXPAND + 2,
        GrowDown => EXPAND + 3,
        ShrinkLeft => SHRINK,
        ShrinkRight => SHRINK + 1,
        ShrinkUp => SHRINK + 2,
        ShrinkDown => SHRINK + 3,
        Confirm => CONFIRM,
        Cancel => CANCEL,
        QuickLeft => QUICK_RESIZE,
        QuickRight => QUICK_RESIZE + 1,
        QuickUp => QUICK_RESIZE + 2,
        QuickDown => QUICK_RESIZE + 3,
        QuickReverseLeft => QUICK_REVERSE,
        QuickReverseRight => QUICK_REVERSE + 1,
        QuickReverseUp => QUICK_REVERSE + 2,
        QuickReverseDown => QUICK_REVERSE + 3,
    };
    id as u8
}

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
    ResizeStep,
}

impl Press {
    fn route(&mut self, down: bool, command: Option<i32>) -> Action {
        let consumed = matches!(*self, Self::Consumed | Self::ResizeStep);
        if !down {
            *self = Self::Up;
            return if consumed {
                Action::Consume
            } else {
                Action::Pass
            };
        }
        if *self == Self::Passed {
            return Action::Pass;
        }
        if let Some(id) = command {
            let repeatable = [EXPAND, SHRINK, QUICK_RESIZE, QUICK_REVERSE]
                .into_iter()
                .any(|first| (first..first + 4).contains(&id));
            if !consumed || (*self == Self::ResizeStep && repeatable) {
                *self = if repeatable {
                    Self::ResizeStep
                } else {
                    Self::Consumed
                };
                return Action::Command(id);
            }
        }
        if consumed {
            Action::Consume
        } else {
            *self = Self::Passed;
            Action::Pass
        }
    }
}

#[derive(Clone, Copy)]
struct Snapshot {
    target: isize,
    mode: Mode,
    focus_epoch: usize,
    handled: usize,
}

struct Routing {
    focus: Snapshot,
    keys: Routes,
}

static ROUTING: LazyLock<Mutex<Routing>> = LazyLock::new(|| {
    Mutex::new(Routing {
        focus: Snapshot {
            target: 0,
            mode: Mode::Normal,
            focus_epoch: 0,
            handled: 0,
        },
        keys: Routes::new(Settings::default()),
    })
});

pub(super) fn update(target: Option<isize>, mode: Mode, handled: usize) {
    let mut routing = ROUTING.lock().unwrap_or_else(|e| e.into_inner());
    let state = &mut routing.focus;
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
    static HELD: [Cell<Press>; 256] = const { [const { Cell::new(Press::Up) }; 256] };
    static PENDING: Cell<Pending> = const { Cell::new(Pending::NEW) };
}

fn modifiers() -> u32 {
    // Read only modifier states preceding the chord's main key transition.
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

fn mark_modifier_used() {
    // The shell or foreground app otherwise sees a bare Win or Alt release
    // after a consumed chord and can activate Start or its menu bar.
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

fn bound_command(slot: usize, pending: Pending) -> Option<(i32, Snapshot, u32)> {
    let (state, key) = {
        let routing = ROUTING.lock().unwrap_or_else(|e| e.into_inner());
        (routing.focus, routing.keys.0[slot]?)
    };
    if state.target == 0 || state.target != unsafe { GetForegroundWindow() } as isize {
        return None;
    }
    let mods = modifiers();
    key.command(pending.mode(state), mods)
        .map(|id| (id, state, mods))
}

unsafe extern "system" fn on_key(code: i32, message: usize, data: isize) -> isize {
    if code == HC_ACTION as i32 {
        // Read only the virtual key. No translations, strings or input logs.
        let vk = unsafe { (*(data as *const KBDLLHOOKSTRUCT)).vkCode };
        if let Ok(vk) = u8::try_from(vk)
            && Chord::supports_key(u16::from(vk))
        {
            let down = matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let slot = usize::from(vk);
            let previous = HELD.with(|held| held[slot].get());
            let mut press = previous;
            let pending = PENDING.get();
            let binding = if down && previous != Press::Passed {
                bound_command(slot, pending)
            } else {
                None
            };
            match (press.route(down, binding.map(|(id, _, _)| id)), binding) {
                (Action::Consume, _) => {
                    HELD.with(|held| held[slot].set(press));
                    return 1;
                }
                (Action::Command(id), Some((_, state, mods))) => {
                    let target = state.target;
                    let transition = pending.transition(id, state.focus_epoch);
                    let ticket = transition.map_or(0, |next| next.ticket);
                    let command = (ticket << 8) | id as usize;
                    if unsafe { PostThreadMessageW(OWNER.get(), KEY_COMMAND, command, target) } != 0
                    {
                        HELD.with(|held| held[slot].set(press));
                        if let Some(next) = transition {
                            PENDING.set(next);
                        }
                        if previous == Press::Up && mods & (MOD_WIN | MOD_ALT) != 0 {
                            mark_modifier_used();
                        }
                        return 1;
                    }
                    if previous != Press::Up {
                        return 1;
                    }
                    HELD.with(|held| held[slot].set(Press::Passed));
                }
                _ => HELD.with(|held| held[slot].set(press)),
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
    pub(super) fn install(settings: Settings) -> Result<Self, String> {
        let owner = unsafe { GetCurrentThreadId() };
        let desktop = unsafe { GetThreadDesktop(owner) } as usize;
        let (ready, status) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("rustile-shortcuts".into())
            .spawn(move || {
                OWNER.set(owner);
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
            Ok(Ok(id)) => {
                let hook = Self {
                    thread: Some(thread),
                    id,
                };
                hook.set_settings(settings);
                Ok(hook)
            }
            result => {
                let _ = thread.join();
                match result {
                    Ok(Err(e)) => Err(format!("Обработка клавиатурных сочетаний: {e}")),
                    _ => Err("Поток сочетаний клавиш неожиданно завершился.".into()),
                }
            }
        }
    }

    pub(super) fn set_settings(&self, settings: Settings) {
        let keys = Routes::new(settings);
        ROUTING.lock().unwrap_or_else(|e| e.into_inner()).keys = keys;
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
    use crate::settings::{Command, Modifier};

    const WIN_SHIFT: u32 = MOD_WIN | MOD_SHIFT;

    // Exercise the hook's prediction and repeat handling before/after arbitrary
    // manager acknowledgements, without sending keyboard events to Windows.
    struct Keyboard {
        held: [Press; 256],
        settings: Settings,
        state: Snapshot,
        pending: Pending,
    }

    impl Keyboard {
        fn new() -> Self {
            Self {
                held: [Press::Up; 256],
                settings: Settings::default(),
                state: Snapshot {
                    target: 100,
                    mode: Mode::Normal,
                    focus_epoch: 1,
                    handled: 0,
                },
                pending: Pending::NEW,
            }
        }

        fn assign(&mut self, shortcut: Shortcut, mods: u32, key: u16) {
            self.settings = self
                .settings
                .changed(Command::Shortcut(shortcut, Chord { mods, key }))
                .unwrap();
        }

        fn key(&mut self, key: u16, down: bool, mods: u32) -> Action {
            let slot = usize::from(key);
            let routes = Routes::new(self.settings);
            let command = routes.0[slot].and_then(|route| {
                (self.state.target != 0)
                    .then(|| self.pending.mode(self.state))
                    .and_then(|mode| route.command(mode, mods))
            });
            let action = self.held[slot].route(down, command);
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
        for (slot, key) in [VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN].into_iter().enumerate() {
            assert_eq!(
                keyboard.key(key, true, WIN_SHIFT),
                Action::Command(MOVE + slot as i32)
            );
            assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Consume);
            assert_eq!(keyboard.key(key, false, 0), Action::Consume);
            assert_eq!(
                keyboard.key(key, true, MOD_WIN),
                Action::Command(FOCUS + slot as i32)
            );
            assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Consume);
            assert_eq!(keyboard.key(key, false, 0), Action::Consume);
        }
    }

    #[test]
    fn quick_resize_repeats_and_reverses_without_entering_modal_routing() {
        let mut k = Keyboard::new();
        let mods = MOD_WIN | MOD_ALT;
        for modifier in [VK_LWIN, VK_RWIN, VK_MENU] {
            assert_eq!(k.key(modifier, true, mods), Action::Pass);
            assert_eq!(k.key(modifier, false, 0), Action::Pass);
        }
        for (slot, key) in [VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN].into_iter().enumerate() {
            for flags in [mods, mods, mods | MOD_SHIFT, mods] {
                let id = if flags == mods {
                    QUICK_RESIZE
                } else {
                    QUICK_REVERSE
                };
                assert_eq!(k.key(key, true, flags), Action::Command(id + slot as i32));
            }
            assert_eq!(k.key(key, true, MOD_WIN), Action::Consume);
            assert_eq!(k.key(key, false, 0), Action::Consume);
            assert_eq!(k.key(key, true, 0), Action::Pass);
            assert_eq!(k.key(key, false, 0), Action::Pass);
        }
        assert_eq!(k.pending.ticket, 0);
        k.state.target = 0;
        assert_eq!(k.key(VK_LEFT, true, mods), Action::Pass);
        assert_eq!(k.key(VK_LEFT, false, 0), Action::Pass);
    }

    #[test]
    fn rebinding_a_quick_resize_chord_releases_its_old_keys() {
        let mut k = Keyboard::new();
        k.assign(Shortcut::QuickUp, MOD_CONTROL, VK_F8);
        assert_eq!(k.key(VK_UP, true, MOD_WIN | MOD_ALT), Action::Pass);
        assert_eq!(k.key(VK_UP, false, 0), Action::Pass);
        assert_eq!(
            k.key(VK_F8, true, MOD_CONTROL),
            Action::Command(QUICK_RESIZE + 2)
        );
        assert_eq!(k.key(VK_F8, false, 0), Action::Consume);
        k.state.mode = Mode::Resize;
        assert_eq!(k.key(VK_F8, true, MOD_CONTROL), Action::Pass);
        assert_eq!(k.key(VK_UP, true, 0), Action::Command(EXPAND + 2));
    }

    #[test]
    fn immediate_modal_exit_ignores_an_older_acknowledgement() {
        for mods in [0, MOD_WIN] {
            let mut keyboard = Keyboard::new();
            assert_eq!(
                keyboard.key(VK_RETURN, true, MOD_WIN),
                Action::Command(START_RESIZE)
            );
            let opener_ticket = keyboard.pending.ticket;
            assert_eq!(keyboard.key(VK_ESCAPE, true, mods), Action::Command(CANCEL));
            // Only the opener has been handled on the manager thread so far.
            keyboard.state.handled = opener_ticket;
            keyboard.state.mode = Mode::Resize;
            assert_eq!(keyboard.key(VK_ESCAPE, true, 0), Action::Consume);
            assert_eq!(keyboard.key(VK_LEFT, true, 0), Action::Pass);
            assert_eq!(keyboard.key(VK_LEFT, false, 0), Action::Pass);
            assert_eq!(keyboard.key(VK_ESCAPE, false, 0), Action::Consume);
            assert_eq!(keyboard.key(VK_LEFT, true, MOD_WIN), Action::Command(FOCUS));
        }
    }

    #[test]
    fn windows_x_bypasses_the_shortcut_filter() {
        let mut k = Keyboard::new();
        assert_eq!(k.key(VK_X, true, MOD_WIN), Action::Pass);
        assert_eq!(k.key(VK_X, false, 0), Action::Pass);
    }

    #[test]
    fn opening_enter_cannot_confirm_until_released_and_resize_arrows_repeat() {
        let mut keyboard = Keyboard::new();
        assert_eq!(
            keyboard.key(VK_RETURN, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        assert_eq!(keyboard.key(VK_RETURN, true, 0), Action::Consume);
        assert_eq!(
            keyboard.key(VK_LEFT, true, MOD_WIN),
            Action::Command(EXPAND)
        );
        assert_eq!(keyboard.key(VK_LEFT, true, 0), Action::Command(EXPAND));
        assert_eq!(
            keyboard.key(VK_LEFT, true, WIN_SHIFT),
            Action::Command(SHRINK)
        );
        assert_eq!(
            keyboard.key(VK_LEFT, true, MOD_SHIFT),
            Action::Command(SHRINK)
        );
        assert_eq!(keyboard.key(VK_LEFT, false, 0), Action::Consume);
        assert_eq!(keyboard.key(VK_RETURN, false, 0), Action::Consume);
        assert_eq!(keyboard.key(VK_RETURN, true, 0), Action::Command(CONFIRM));
        assert_eq!(keyboard.key(VK_LEFT, true, 0), Action::Pass);
    }

    #[test]
    fn focus_change_and_rejected_opener_restore_normal_routing() {
        let mut keyboard = Keyboard::new();
        assert_eq!(
            keyboard.key(VK_RETURN, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        keyboard.state.handled = keyboard.pending.ticket;
        assert_eq!(keyboard.key(VK_LEFT, true, 0), Action::Pass);
        assert_eq!(keyboard.key(VK_RETURN, false, 0), Action::Consume);
        assert_eq!(
            keyboard.key(VK_RETURN, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        keyboard.state.target = 0;
        keyboard.state.focus_epoch += 1;
        assert_eq!(keyboard.key(VK_RETURN, false, 0), Action::Consume);
        assert_eq!(keyboard.key(VK_RETURN, true, MOD_WIN), Action::Pass);
        assert_eq!(keyboard.key(VK_RETURN, false, 0), Action::Pass);
        assert_eq!(keyboard.key(VK_RIGHT, true, WIN_SHIFT), Action::Pass);
        keyboard.state.target = 100;
        keyboard.state.focus_epoch += 1;
        assert_eq!(keyboard.key(VK_UP, true, 0), Action::Pass);
    }

    #[test]
    fn native_keypresses_keep_their_repeats_and_up_even_if_modifiers_change() {
        let mut keyboard = Keyboard::new();
        for mods in [0, MOD_SHIFT, MOD_CONTROL, MOD_ALT, MOD_CONTROL | MOD_WIN] {
            assert_eq!(keyboard.key(VK_RIGHT, true, mods), Action::Pass);
            assert_eq!(keyboard.key(VK_RIGHT, true, MOD_WIN), Action::Pass);
            assert_eq!(keyboard.key(VK_RIGHT, false, MOD_WIN), Action::Pass);
        }
        assert_eq!(
            keyboard.key(VK_LEFT, true, MOD_CONTROL | WIN_SHIFT),
            Action::Pass
        );
        assert_eq!(keyboard.key(VK_LEFT, false, 0), Action::Pass);
        keyboard.settings = keyboard
            .settings
            .with_resize_override(Modifier::CtrlAlt)
            .unwrap();
        assert_eq!(keyboard.key(VK_RETURN, true, MOD_WIN), Action::Pass);
        assert_eq!(keyboard.key(VK_RETURN, false, 0), Action::Pass);
        assert_eq!(
            keyboard.key(VK_RETURN, true, MOD_CONTROL | MOD_ALT),
            Action::Command(START_RESIZE)
        );
    }

    #[test]
    fn editing_shortcuts_takes_effect_on_the_next_physical_press() {
        let mut k = Keyboard::new();
        assert_eq!(k.key(VK_LEFT, true, MOD_WIN), Action::Command(FOCUS));
        assert_eq!(k.key(VK_RIGHT, true, MOD_ALT), Action::Pass);
        k.assign(Shortcut::FocusLeft, MOD_ALT, VK_H);
        k.assign(Shortcut::FocusRight, MOD_ALT, VK_RIGHT);
        k.assign(Shortcut::MoveUp, MOD_CONTROL | MOD_ALT, VK_K);

        // Neither a consumed nor a passed key switches owners mid-press.
        assert_eq!(k.key(VK_LEFT, true, MOD_WIN), Action::Consume);
        assert_eq!(k.key(VK_LEFT, false, 0), Action::Consume);
        assert_eq!(k.key(VK_RIGHT, true, MOD_ALT), Action::Pass);
        assert_eq!(k.key(VK_RIGHT, false, 0), Action::Pass);
        assert_eq!(k.key(VK_RIGHT, true, MOD_ALT), Action::Command(FOCUS + 1));
        assert_eq!(k.key(VK_RIGHT, false, 0), Action::Consume);

        assert_eq!(k.key(VK_UP, true, WIN_SHIFT), Action::Pass);
        assert_eq!(k.key(VK_UP, false, 0), Action::Pass);
        assert_eq!(k.key(VK_UP, true, MOD_WIN), Action::Command(FOCUS + 2));
        assert_eq!(
            k.key(VK_K, true, MOD_CONTROL | MOD_ALT),
            Action::Command(MOVE + 2)
        );
        k.state.target = 0;
        assert_eq!(k.key(VK_K, false, 0), Action::Consume);
        assert_eq!(k.key(VK_H, true, MOD_ALT), Action::Pass);
    }

    #[test]
    fn modal_rebinds_change_only_the_requested_step_and_leave_no_stale_aliases() {
        let mut k = Keyboard::new();
        k.assign(Shortcut::StartResize, MOD_ALT, VK_F8);
        k.assign(Shortcut::GrowUp, 0, VK_K);
        k.assign(Shortcut::Confirm, 0, VK_SPACE);
        k.assign(Shortcut::Cancel, 0, VK_BACK);
        assert_eq!(k.key(VK_F8, true, MOD_ALT), Action::Command(START_RESIZE));
        assert_eq!(k.key(VK_K, true, 0), Action::Command(EXPAND + 2));
        assert_eq!(k.key(VK_K, true, 0), Action::Command(EXPAND + 2));
        assert_eq!(k.key(VK_UP, true, MOD_WIN), Action::Pass);
        assert_eq!(k.key(VK_ESCAPE, true, 0), Action::Pass);
        assert_eq!(k.key(VK_RETURN, true, 0), Action::Pass);
        assert_eq!(k.key(VK_SPACE, true, 0), Action::Command(CONFIRM));
        assert_eq!(k.key(VK_F8, false, 0), Action::Consume);
        assert_eq!(k.key(VK_F8, true, MOD_ALT), Action::Command(START_RESIZE));
        assert_eq!(k.key(VK_BACK, true, 0), Action::Command(CANCEL));
    }

    #[test]
    fn former_exit_chord_passes_to_apps_and_is_available_for_remapping() {
        let mut k = Keyboard::new();
        let mods = MOD_CONTROL | MOD_ALT | MOD_SHIFT;
        for mode in [Mode::Resize, Mode::Normal] {
            k.state.mode = mode;
            assert_eq!(k.key(VK_Q, true, mods), Action::Pass);
            assert_eq!(k.key(VK_Q, false, 0), Action::Pass);
        }
        k.assign(Shortcut::MoveLeft, mods, VK_Q);
        assert_eq!(k.key(VK_Q, true, mods), Action::Command(MOVE));
        assert_eq!(k.key(VK_Q, false, 0), Action::Consume);
        k.settings = k
            .settings
            .changed(Command::ResetShortcut(Shortcut::MoveLeft))
            .unwrap();
        assert_eq!(k.key(VK_Q, true, mods), Action::Pass);
        assert_eq!(k.key(VK_LEFT, true, WIN_SHIFT), Action::Command(MOVE));
    }

    #[test]
    fn transferred_resize_action_leaves_no_implicit_shortcuts_in_the_hook() {
        let mut keyboard = Keyboard::new();
        let chord = Chord {
            mods: MOD_WIN,
            key: VK_LEFT,
        };
        keyboard.settings = keyboard
            .settings
            .changed(Command::ReassignShortcut(
                Shortcut::ShrinkUp,
                chord,
                Shortcut::GrowLeft,
            ))
            .unwrap();
        keyboard.state.mode = Mode::Resize;
        assert_eq!(keyboard.key(VK_LEFT, true, 0), Action::Pass);
        assert_eq!(keyboard.key(VK_LEFT, false, 0), Action::Pass);
        assert_eq!(
            keyboard.key(VK_LEFT, true, MOD_WIN),
            Action::Command(SHRINK + 2)
        );
        assert_eq!(keyboard.key(VK_LEFT, false, 0), Action::Consume);
        keyboard.state.mode = Mode::Normal;
        assert_eq!(keyboard.key(VK_LEFT, true, MOD_WIN), Action::Command(FOCUS));
    }

    #[test]
    fn layout_and_media_keys_dispatch_on_their_own_virtual_key() {
        let mut keyboard = Keyboard::new();
        for key in [VK_OEM_3, VK_OEM_PLUS, VK_OEM_102, VK_MEDIA_PLAY_PAUSE] {
            keyboard.assign(Shortcut::MoveDown, MOD_CONTROL, key);
            assert_eq!(
                keyboard.key(key, true, MOD_CONTROL),
                Action::Command(MOVE + 3)
            );
            assert_eq!(keyboard.key(key, false, 0), Action::Consume);
            assert_eq!(keyboard.key(key, true, MOD_ALT), Action::Pass);
            assert_eq!(keyboard.key(key, false, 0), Action::Pass);
        }
    }

    #[test]
    fn custom_resize_opener_supports_held_modifiers_and_shift_for_shrink() {
        for opener in [Modifier::CtrlAlt, Modifier::CtrlAltShift] {
            let mut k = Keyboard::new();
            k.settings = k.settings.with_resize_override(opener).unwrap();
            assert_eq!(
                k.key(VK_RETURN, true, opener.bits()),
                Action::Command(START_RESIZE)
            );
            assert_eq!(k.key(VK_RETURN, false, 0), Action::Consume);
            for mods in [0, MOD_WIN, MOD_CONTROL | MOD_ALT] {
                assert_eq!(k.key(VK_UP, true, mods), Action::Command(EXPAND + 2));
                assert_eq!(
                    k.key(VK_UP, true, mods | MOD_SHIFT),
                    Action::Command(SHRINK + 2)
                );
                assert_eq!(k.key(VK_UP, false, 0), Action::Consume);
            }
            assert_eq!(
                k.key(VK_ESCAPE, true, opener.bits()),
                Action::Command(CANCEL)
            );
        }
    }
}
