use std::cell::Cell;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::StationsAndDesktops::{GetThreadDesktop, HDESK, SetThreadDesktop};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

mod wheel;

use super::{
    CANCEL, CLOSE_WINDOW, CONFIRM, DESKTOP_FIRST, DESKTOP_NEXT, DESKTOP_PREVIOUS, DESKTOP_TENTH,
    DesktopDestination, EXPAND, FOCUS, KEY_COMMAND, MOVE, MOVE_DESKTOP_NEXT, MOVE_DESKTOP_PREVIOUS,
    QUICK_RESIZE, QUICK_REVERSE, SHRINK, START_RESIZE, TOGGLE_MAXIMIZE, TOGGLE_MOSAIC,
};
use crate::manager::{Mode, ShortcutTarget};
use crate::settings::{Chord, Settings, Shortcut};

static TRANSFER_FOCUS_WATCH: AtomicBool = AtomicBool::new(false);
static DESKTOP_FOCUS_WATCH: AtomicBool = AtomicBool::new(false);

pub(super) fn watch_desktop_focus() {
    DESKTOP_FOCUS_WATCH.store(true, Ordering::Relaxed);
}

pub(super) fn desktop_focus_uninterrupted() -> bool {
    DESKTOP_FOCUS_WATCH.load(Ordering::Relaxed)
}

pub(super) fn end_desktop_focus_watch() {
    DESKTOP_FOCUS_WATCH.store(false, Ordering::Relaxed);
}

pub(super) fn watch_transfer_focus() {
    TRANSFER_FOCUS_WATCH.store(true, Ordering::Relaxed);
}

pub(crate) fn transfer_focus_uninterrupted() -> bool {
    TRANSFER_FOCUS_WATCH.load(Ordering::Relaxed)
}

pub(super) fn end_transfer_focus_watch() {
    TRANSFER_FOCUS_WATCH.store(false, Ordering::Relaxed);
}

fn is_desktop_move(id: i32) -> bool {
    matches!(id, MOVE_DESKTOP_PREVIOUS | MOVE_DESKTOP_NEXT)
}

pub(super) fn is_focus_command(id: i32) -> bool {
    (FOCUS..FOCUS + 4).contains(&id)
}

fn uses_desktop_focus(id: i32) -> bool {
    is_focus_command(id) || is_desktop_move(id) || DesktopDestination::from_command(id).is_some()
}

// The table is rebuilt only on a settings change. The hook inspects virtual
// key codes and current modifiers; it never translates or records typed text.
#[derive(Clone, Copy, Default)]
struct KeyRoutes {
    normal: [u8; 16],
    resize: [u8; 16],
}

impl KeyRoutes {
    fn bind(&mut self, action: Shortcut, mods: u32) {
        let id = command_id(action);
        if !action.is_resize() {
            self.normal[mods as usize] = id;
        }
        if action.is_resize() || action.works_in_both_modes() {
            self.resize[mods as usize] = id;
        }
    }

    fn binding(
        self,
        state: Snapshot,
        pending: Pending,
        mods: u32,
        foreground: impl FnOnce() -> isize,
    ) -> Option<BoundCommand> {
        let normal = *self.normal.get(mods as usize)?;
        let resize = self.resize[mods as usize];
        let mode = pending.mode(state);
        if normal == 0 && (mode == Mode::Normal || resize == 0) {
            return None;
        }
        let active = foreground();
        let known_tile =
            matches!(state.target, Some(ShortcutTarget::Tile { id, .. }) if id == active);
        // Only the window being resized owns modal-only keys like bare arrows.
        let resizing = mode == Mode::Resize && known_tile;
        let id = i32::from(if resizing && resize != 0 {
            resize
        } else {
            normal
        });
        if id == 0 {
            return None;
        }
        let target = if id == TOGGLE_MOSAIC || DesktopDestination::from_command(id).is_some() {
            Some(0) // Global commands need no foreground window.
        } else {
            // When Explorer clears the foreground, a focus command can select
            // a tile on the current desktop, and a transfer can continue its handoff.
            let has_target = active != 0
                || is_focus_command(id)
                || (is_desktop_move(id) && transfer_focus_uninterrupted());
            // Own a configured normal chord even when its action is unavailable
            // in resize mode. The manager validates the sampled HWND on delivery.
            (has_target && (!resizing || resize != 0)).then_some(active)
        };
        Some(BoundCommand {
            id,
            target,
            focus_epoch: state.focus_epoch,
            mods,
            predict_resize: known_tile,
        })
    }
}

const INPUT_SLOTS: usize = crate::settings::WHEEL_DOWN as usize + 1;
struct Routes([Option<KeyRoutes>; INPUT_SLOTS]);

impl Routes {
    const EMPTY: Self = Self([None; INPUT_SLOTS]);

    fn new(settings: Settings) -> Self {
        let mut keys = [None; INPUT_SLOTS];
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
            row.bind(action, chord.mods);
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
        CloseWindow => CLOSE_WINDOW,
        ToggleMaximize => TOGGLE_MAXIMIZE,
        ToggleMosaic => TOGGLE_MOSAIC,
        PreviousDesktop | PreviousDesktopAlt | PreviousDesktopWheel => DESKTOP_PREVIOUS,
        NextDesktop | NextDesktopAlt | NextDesktopWheel => DESKTOP_NEXT,
        Desktop1 => DESKTOP_FIRST,
        Desktop2 => DESKTOP_FIRST + 1,
        Desktop3 => DESKTOP_FIRST + 2,
        Desktop4 => DESKTOP_FIRST + 3,
        Desktop5 => DESKTOP_FIRST + 4,
        Desktop6 => DESKTOP_FIRST + 5,
        Desktop7 => DESKTOP_FIRST + 6,
        Desktop8 => DESKTOP_FIRST + 7,
        Desktop9 => DESKTOP_FIRST + 8,
        Desktop10 => DESKTOP_TENTH,
        MovePreviousDesktop | MovePreviousDesktopAlt => MOVE_DESKTOP_PREVIOUS,
        MoveNextDesktop | MoveNextDesktopAlt => MOVE_DESKTOP_NEXT,
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
    ResizeStep(isize),
}

impl Press {
    fn route(&mut self, down: bool, binding: Option<BoundCommand>) -> Action {
        let consumed = matches!(*self, Self::Consumed | Self::ResizeStep(_));
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
        if let Some(binding) = binding {
            let Some(target) = binding.target else {
                *self = Self::Consumed;
                return Action::Consume;
            };
            let id = binding.id;
            let repeatable = [EXPAND, SHRINK, QUICK_RESIZE, QUICK_REVERSE]
                .into_iter()
                .any(|first| (first..first + 4).contains(&id));
            if !consumed || (*self == Self::ResizeStep(target) && repeatable) {
                *self = if repeatable {
                    Self::ResizeStep(target)
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
    target: Option<ShortcutTarget>,
    mode: Mode,
    focus_epoch: usize,
    handled: usize,
}

struct Routing {
    focus: Snapshot,
    keys: Routes,
    revision: usize,
}

static ROUTING: LazyLock<Mutex<Routing>> = LazyLock::new(|| {
    Mutex::new(Routing {
        focus: Snapshot {
            target: None,
            mode: Mode::Normal,
            focus_epoch: 0,
            handled: 0,
        },
        // Global shortcuts can fire before the manager has sampled a window.
        // Arm them only when the user's actual settings have been loaded.
        keys: Routes::EMPTY,
        revision: 0,
    })
});

pub(super) fn update(target: Option<ShortcutTarget>, mode: Mode, handled: usize) {
    let mut routing = ROUTING.lock().unwrap_or_else(|e| e.into_inner());
    let state = &mut routing.focus;
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
            CONFIRM | CANCEL | TOGGLE_MOSAIC | MOVE_DESKTOP_PREVIOUS | MOVE_DESKTOP_NEXT => {
                Mode::Normal
            }
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct BoundCommand {
    id: i32,
    target: Option<isize>,
    focus_epoch: usize,
    mods: u32,
    predict_resize: bool,
}

impl BoundCommand {
    fn transition(self, pending: Pending) -> Option<Pending> {
        if self.id == START_RESIZE && !self.predict_resize {
            return None;
        }
        pending.transition(self.id, self.focus_epoch)
    }

    fn post(self, pending: Pending) {
        let Some(target) = self.target else { return };
        // Arm at the gesture so input arriving before the pump handles the
        // command can cancel automatic activation.
        if uses_desktop_focus(self.id) {
            watch_desktop_focus();
        }
        let transition = self.transition(pending);
        let ticket = transition.map_or(0, |next| next.ticket);
        let command = (ticket << 8) | self.id as usize;
        if unsafe { PostThreadMessageW(OWNER.get(), KEY_COMMAND, command, target) } != 0
            && let Some(next) = transition
        {
            PENDING.set(next);
        }
    }
}

fn route_snapshot(slot: usize) -> (Snapshot, Option<KeyRoutes>, usize) {
    let routing = ROUTING.lock().unwrap_or_else(|e| e.into_inner());
    (routing.focus, routing.keys.0[slot], routing.revision)
}

fn bound_command(slot: usize, pending: Pending) -> Option<BoundCommand> {
    let (state, key, _) = route_snapshot(slot);
    key?.binding(
        state,
        pending,
        modifiers(),
        || unsafe { GetForegroundWindow() } as isize,
    )
}

unsafe extern "system" fn on_key(code: i32, message: usize, data: isize) -> isize {
    if code == HC_ACTION as i32 {
        // Read only the virtual key. No translations, strings or input logs.
        let vk = unsafe { (*(data as *const KBDLLHOOKSTRUCT)).vkCode };
        let modifier = matches!(
            vk as u16,
            VK_LWIN
                | VK_RWIN
                | VK_SHIFT
                | VK_LSHIFT
                | VK_RSHIFT
                | VK_CONTROL
                | VK_LCONTROL
                | VK_RCONTROL
                | VK_MENU
                | VK_LMENU
                | VK_RMENU
        );
        if matches!(message as u32, WM_KEYUP | WM_SYSKEYUP) && modifier {
            wheel::reset();
        }
        if let Ok(vk) = u8::try_from(vk)
            && !modifier
            && vk != 0xff
        {
            let down = matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let slot = usize::from(vk);
            let previous = HELD.with(|held| held[slot].get());
            let pending = PENDING.get();
            let binding = if down && previous != Press::Passed && Chord::supports_key(vk.into()) {
                bound_command(slot, pending)
            } else {
                None
            };
            if down
                && transfer_focus_uninterrupted()
                && !binding.is_some_and(|b| is_desktop_move(b.id))
            {
                end_transfer_focus_watch();
            }
            if down
                && desktop_focus_uninterrupted()
                && !binding.is_some_and(|b| uses_desktop_focus(b.id))
            {
                end_desktop_focus_watch();
            }
            let mut press = previous;
            let action = press.route(down, binding);
            HELD.with(|held| held[slot].set(press));
            if action != Action::Pass {
                if let Action::Command(_) = action
                    && let Some(binding) = binding
                {
                    binding.post(pending);
                }
                if down
                    && previous == Press::Up
                    && binding.is_some_and(|b| b.mods & (MOD_WIN | MOD_ALT) != 0)
                {
                    mark_modifier_used();
                }
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}

#[cfg(test)]
pub(super) fn test_keyboard_event(key: u16, down: bool, owner: u32) -> isize {
    let saved = OWNER.replace(owner);
    let event = KBDLLHOOKSTRUCT {
        vkCode: u32::from(key),
        ..Default::default()
    };
    let message = if down { WM_KEYDOWN } else { WM_KEYUP };
    let result = unsafe {
        on_key(
            HC_ACTION as i32,
            message as usize,
            &raw const event as isize,
        )
    };
    OWNER.set(saved);
    result
}

#[cfg(test)]
pub(super) fn test_wheel_event(message: u32, delta: i16, mods: u32, owner: u32) -> bool {
    let saved = OWNER.replace(owner);
    let consumed = message == WM_MOUSEWHEEL && wheel::handle(delta, || mods, || {});
    OWNER.set(saved);
    consumed
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
                // The first callback must already see the user's assignments.
                let keys = Routes::new(settings);
                ROUTING.lock().unwrap_or_else(|e| e.into_inner()).keys = keys;
                let module = unsafe { GetModuleHandleW(null()) };
                let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), module, 0) };
                if hook.is_null() {
                    let _ = ready.send(Err(io::Error::last_os_error()));
                    return;
                }
                let mouse =
                    unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(wheel::on_mouse), module, 0) };
                if mouse.is_null() {
                    let error = io::Error::last_os_error();
                    unsafe { UnhookWindowsHookEx(hook) };
                    let _ = ready.send(Err(error));
                    return;
                }
                if ready.send(Ok(unsafe { GetCurrentThreadId() })).is_ok() {
                    while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
                        unsafe { DispatchMessageW(&message) };
                    }
                }
                unsafe {
                    UnhookWindowsHookEx(mouse);
                    UnhookWindowsHookEx(hook);
                }
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
                    Ok(Err(e)) => Err(format!("Обработка сочетаний клавиш и колёсика: {e}")),
                    _ => Err("Поток сочетаний клавиш неожиданно завершился.".into()),
                }
            }
        }
    }

    pub(super) fn set_settings(&self, settings: Settings) {
        let keys = Routes::new(settings);
        let mut routing = ROUTING.lock().unwrap_or_else(|e| e.into_inner());
        routing.keys = keys;
        routing.revision = routing.revision.wrapping_add(1);
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe { PostThreadMessageW(self.id, WM_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        end_transfer_focus_watch();
        end_desktop_focus_watch();
        update(None, Mode::Normal, 0);
        ROUTING.lock().unwrap_or_else(|e| e.into_inner()).keys = Routes::EMPTY;
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
                    target: Some(ShortcutTarget::Tile {
                        id: 100,
                        can_maximize: true,
                    }),
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
            self.key_with_foreground(
                key,
                down,
                mods,
                self.state.target.map_or(0, ShortcutTarget::id),
            )
        }

        fn key_with_foreground(
            &mut self,
            key: u16,
            down: bool,
            mods: u32,
            active: isize,
        ) -> Action {
            let slot = usize::from(key);
            let routes = Routes::new(self.settings);
            let binding = routes.0[slot]
                .and_then(|route| route.binding(self.state, self.pending, mods, || active));
            let action = self.held[slot].route(down, binding);
            if let Action::Command(_) = action
                && let Some(next) = binding.and_then(|b| b.transition(self.pending))
            {
                self.pending = next;
            }
            action
        }
    }

    #[test]
    fn desktop_shortcuts_are_global_and_switch_once_per_physical_press() {
        let mods = MOD_CONTROL | MOD_WIN;
        let mut keyboard = Keyboard::new();
        keyboard.state.target = None;
        for mode in [Mode::Normal, Mode::Resize] {
            keyboard.state.mode = mode;
            for (key, id) in [
                (VK_LEFT, DESKTOP_PREVIOUS),
                (VK_UP, DESKTOP_PREVIOUS),
                (VK_RIGHT, DESKTOP_NEXT),
                (VK_DOWN, DESKTOP_NEXT),
            ] {
                assert_eq!(keyboard.key(key, true, mods), Action::Command(id));
                assert_eq!(keyboard.key(key, true, mods), Action::Consume);
                assert_eq!(keyboard.key(key, false, 0), Action::Consume);
            }
        }
    }

    #[test]
    fn desktop_move_targets_the_foreground_in_both_modes_and_leaves_neighboring_chords_alone() {
        let mods = MOD_WIN | MOD_CONTROL | MOD_SHIFT;
        let mut keyboard = Keyboard::new();
        let routes = Routes::new(keyboard.settings);
        for mode in [Mode::Normal, Mode::Resize] {
            keyboard.state.mode = mode;
            for (slot, (key, transfer, navigate)) in [
                (VK_LEFT, MOVE_DESKTOP_PREVIOUS, DESKTOP_PREVIOUS),
                (VK_RIGHT, MOVE_DESKTOP_NEXT, DESKTOP_NEXT),
                (VK_UP, MOVE_DESKTOP_PREVIOUS, DESKTOP_PREVIOUS),
                (VK_DOWN, MOVE_DESKTOP_NEXT, DESKTOP_NEXT),
            ]
            .into_iter()
            .enumerate()
            {
                let binding = |mods, active| {
                    routes.0[usize::from(key)]?
                        .binding(keyboard.state, Pending::NEW, mods, || active)
                        .map(|b| (b.id, b.target))
                };
                for active in [100, 999, 0] {
                    assert_eq!(
                        binding(mods, active),
                        Some((transfer, (active != 0).then_some(active)))
                    );
                    assert_eq!(
                        binding(MOD_WIN | MOD_CONTROL, active),
                        Some((navigate, Some(0)))
                    );
                }
                if mode == Mode::Normal {
                    assert_eq!(
                        binding(WIN_SHIFT, 100),
                        Some((MOVE + slot as i32, Some(100)))
                    );
                }
            }
        }
    }

    #[test]
    fn desktop_move_ends_queued_resize_and_requires_a_new_press_for_each_step() {
        let mut keyboard = Keyboard::new();
        let mods = MOD_WIN | MOD_CONTROL | MOD_SHIFT;
        assert_eq!(
            keyboard.key(VK_RETURN, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        let opener = keyboard.pending.ticket;
        assert_eq!(
            keyboard.key(VK_RIGHT, true, mods),
            Action::Command(MOVE_DESKTOP_NEXT)
        );
        keyboard.state.handled = opener;
        keyboard.state.mode = Mode::Resize;
        assert_eq!(keyboard.key(VK_UP, true, 0), Action::Pass);
        assert_eq!(keyboard.key(VK_UP, false, 0), Action::Pass);
        assert_eq!(keyboard.key(VK_RIGHT, true, mods), Action::Consume);
        assert_eq!(keyboard.key(VK_RIGHT, false, mods), Action::Consume);
        assert_eq!(
            keyboard.key(VK_RIGHT, true, mods),
            Action::Command(MOVE_DESKTOP_NEXT)
        );
    }

    #[test]
    fn top_row_digits_select_desktop_numbers_even_without_a_tile() {
        let mut keyboard = Keyboard::new();
        keyboard.state.target = None;
        for mode in [Mode::Normal, Mode::Resize] {
            keyboard.state.mode = mode;
            for number in 1..=10 {
                let key = VK_0 + number % 10;
                let command = DESKTOP_FIRST + i32::from(number) - 1;
                assert!(matches!(DesktopDestination::from_command(command),
                    Some(DesktopDestination::Number(slot)) if slot == usize::from(number)));
                assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Command(command));
                assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Consume);
                assert_eq!(keyboard.key(key, false, 0), Action::Consume);
                assert_eq!(keyboard.key(key, true, 0), Action::Pass);
                assert_eq!(keyboard.key(key, false, 0), Action::Pass);
            }
        }
    }

    #[test]
    fn remapping_one_desktop_shortcut_preserves_its_alternative_in_all_modes() {
        for (action, id, target) in [
            (Shortcut::NextDesktop, DESKTOP_NEXT, 0),
            (Shortcut::MoveNextDesktop, MOVE_DESKTOP_NEXT, 100),
        ] {
            let mods = action.default_chord().mods;
            let mut keyboard = Keyboard::new();
            keyboard.assign(action, MOD_ALT, VK_F8);
            let routes = Routes::new(keyboard.settings);
            for mode in [Mode::Normal, Mode::Resize] {
                keyboard.state.mode = mode;
                let command = |key: u16, mods| {
                    routes.0[key as usize]?
                        .binding(keyboard.state, Pending::NEW, mods, || 100)
                        .map(|b| (b.id, b.target))
                };
                assert_eq!(command(VK_RIGHT, mods), None);
                assert_eq!(command(VK_F8, MOD_ALT), Some((id, Some(target))));
                assert_eq!(command(VK_DOWN, mods), Some((id, Some(target))));
            }
        }
    }

    #[test]
    fn mosaic_toggles_once_per_press_across_pause_and_modal_focus_changes() {
        let mut keyboard = Keyboard::new();
        assert_eq!(
            keyboard.key(VK_RETURN, true, MOD_WIN),
            Action::Command(START_RESIZE)
        );
        assert_eq!(
            keyboard.key(VK_Y, true, MOD_WIN),
            Action::Command(TOGGLE_MOSAIC)
        );
        assert_eq!(keyboard.key(VK_LEFT, true, 0), Action::Pass);
        assert_eq!(keyboard.key(VK_LEFT, false, 0), Action::Pass);

        keyboard.state.target = None;
        keyboard.state.focus_epoch += 1;
        assert_eq!(keyboard.key(VK_Y, true, MOD_WIN), Action::Consume);
        assert_eq!(keyboard.key(VK_Y, false, 0), Action::Consume);
        assert_eq!(
            keyboard.key(VK_Y, true, MOD_WIN),
            Action::Command(TOGGLE_MOSAIC)
        );
        keyboard.state.target = Some(ShortcutTarget::Expanded(222));
        assert_eq!(keyboard.key(VK_Y, true, MOD_WIN), Action::Consume);
        assert_eq!(keyboard.key(VK_Y, false, 0), Action::Consume);
    }

    #[test]
    fn window_chords_capture_the_live_foreground_and_global_chords_need_no_target() {
        let mut keyboard = Keyboard::new();
        keyboard.assign(Shortcut::ToggleMosaic, MOD_ALT, VK_F8);
        let routes = Routes::new(keyboard.settings);
        for mode in [Mode::Normal, Mode::Resize] {
            keyboard.state.mode = mode;
            let command = |key: u16, mods| {
                routes.0[key as usize]?
                    .binding(keyboard.state, Pending::NEW, mods, || 999)
                    .map(|b| (b.id, b.target))
            };
            assert_eq!(command(VK_F8, MOD_ALT), Some((TOGGLE_MOSAIC, Some(0))));
            assert_eq!(command(VK_F8, MOD_ALT | MOD_WIN), None);
            assert_eq!(command(VK_Y, MOD_WIN), None);
            assert_eq!(command(VK_LEFT, MOD_WIN), Some((FOCUS, Some(999))));
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
    fn navigation_with_no_foreground_posts_the_observed_null_target() {
        let keyboard = Keyboard::new(); // The snapshot still names the outgoing tile.
        let routes = Routes::new(keyboard.settings);
        for (slot, key) in [VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN].into_iter().enumerate() {
            let binding = |mods| {
                routes.0[key as usize]?
                    .binding(keyboard.state, Pending::NEW, mods, || 0)
                    .map(|b| (b.id, b.target))
            };
            assert_eq!(binding(MOD_WIN), Some((FOCUS + slot as i32, Some(0))));
            assert_eq!(binding(WIN_SHIFT), Some((MOVE + slot as i32, None)));
            assert_eq!(
                binding(MOD_WIN | MOD_ALT),
                Some((QUICK_RESIZE + slot as i32, None))
            );
        }
    }

    #[test]
    fn a_stale_resize_snapshot_does_not_capture_plain_keys_on_a_different_window() {
        let mut keyboard = Keyboard::new();
        keyboard.state.mode = Mode::Resize;
        let other = 204;
        assert_eq!(
            keyboard.key_with_foreground(VK_UP, true, 0, other),
            Action::Pass
        );
        assert_eq!(
            keyboard.key_with_foreground(VK_UP, false, 0, other),
            Action::Pass
        );
        assert_eq!(
            keyboard.key_with_foreground(VK_UP, true, WIN_SHIFT, other),
            Action::Command(MOVE + 2)
        );
        keyboard.state.mode = Mode::Normal;
        assert_eq!(
            keyboard.key_with_foreground(VK_RETURN, true, MOD_WIN, other),
            Action::Command(START_RESIZE)
        );
        assert_eq!(
            keyboard.pending.ticket, 0,
            "the manager must validate a new resize target"
        );
        assert_eq!(
            keyboard.key_with_foreground(VK_ESCAPE, true, 0, other),
            Action::Pass
        );
    }

    #[test]
    fn a_quick_resize_repeat_cannot_modify_a_window_that_gained_focus_mid_press() {
        let mut keyboard = Keyboard::new();
        let mods = MOD_WIN | MOD_ALT;
        assert_eq!(
            keyboard.key_with_foreground(VK_LEFT, true, mods, 100),
            Action::Command(QUICK_RESIZE)
        );
        assert_eq!(
            keyboard.key_with_foreground(VK_LEFT, true, mods, 200),
            Action::Consume
        );
        assert_eq!(
            keyboard.key_with_foreground(VK_LEFT, false, 0, 200),
            Action::Consume
        );
        assert_eq!(
            keyboard.key_with_foreground(VK_LEFT, true, mods, 200),
            Action::Command(QUICK_RESIZE)
        );
    }

    #[test]
    fn window_commands_require_fresh_presses_even_when_the_foreground_changes() {
        for (key, command) in [(VK_Q, CLOSE_WINDOW), (VK_F, TOGGLE_MAXIMIZE)] {
            let mut keyboard = Keyboard::new();
            assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Command(command));
            keyboard.state.target = Some(ShortcutTarget::Expanded(100));
            assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Consume);
            keyboard.state.target = Some(ShortcutTarget::Tile {
                id: 101,
                can_maximize: true,
            });
            assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Consume);
            assert_eq!(keyboard.key(key, false, 0), Action::Consume);
            assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Command(command));
            assert_eq!(keyboard.key(key, false, 0), Action::Consume);
            keyboard.state.target = None;
            assert_eq!(keyboard.key(key, true, MOD_WIN), Action::Consume);
            assert_eq!(keyboard.key(key, false, 0), Action::Consume);
        }
    }

    #[test]
    fn window_capabilities_are_checked_by_the_manager_after_the_hook_claims_the_chord() {
        let mut keyboard = Keyboard::new();
        keyboard.state.target = Some(ShortcutTarget::Tile {
            id: 100,
            can_maximize: false,
        });
        assert_eq!(
            keyboard.key(VK_F, true, MOD_WIN),
            Action::Command(TOGGLE_MAXIMIZE)
        );
        assert_eq!(keyboard.key(VK_F, false, 0), Action::Consume);
        assert_eq!(keyboard.key(VK_LEFT, true, MOD_WIN), Action::Command(FOCUS));
        assert_eq!(
            keyboard.key(VK_Q, true, MOD_WIN),
            Action::Command(CLOSE_WINDOW)
        );
    }

    #[test]
    fn expanded_windows_keep_their_configured_chords_and_wait_for_a_valid_resize_target() {
        let mut keyboard = Keyboard::new();
        keyboard.state.target = Some(ShortcutTarget::Expanded(100));
        keyboard.assign(Shortcut::ToggleMaximize, MOD_ALT, VK_F8);
        assert_eq!(keyboard.key(VK_F, true, MOD_WIN), Action::Pass);
        assert_eq!(keyboard.key(VK_F, false, 0), Action::Pass);
        for (mods, key, id) in [
            (MOD_WIN, VK_LEFT, FOCUS),
            (WIN_SHIFT, VK_RIGHT, MOVE + 1),
            (MOD_WIN, VK_RETURN, START_RESIZE),
            (MOD_WIN | MOD_ALT, VK_UP, QUICK_RESIZE + 2),
        ] {
            assert_eq!(keyboard.key(key, true, mods), Action::Command(id));
            assert_eq!(keyboard.key(key, false, 0), Action::Consume);
        }
        assert_eq!(keyboard.key(VK_LEFT, true, 0), Action::Pass);
        assert_eq!(
            keyboard.key(VK_F8, true, MOD_ALT),
            Action::Command(TOGGLE_MAXIMIZE)
        );
        assert_eq!(keyboard.key(VK_F8, false, 0), Action::Consume);
        assert_eq!(
            keyboard.key(VK_Q, true, MOD_WIN),
            Action::Command(CLOSE_WINDOW)
        );
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
        k.state.target = None;
        assert_eq!(k.key(VK_LEFT, true, mods), Action::Consume);
        assert_eq!(k.key(VK_LEFT, false, 0), Action::Consume);
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
        assert_eq!(k.key(VK_F8, true, MOD_CONTROL), Action::Consume);
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
        keyboard.state.target = None;
        keyboard.state.focus_epoch += 1;
        assert_eq!(keyboard.key(VK_RETURN, false, 0), Action::Consume);
        assert_eq!(keyboard.key(VK_RETURN, true, MOD_WIN), Action::Consume);
        assert_eq!(keyboard.key(VK_RETURN, false, 0), Action::Consume);
        assert_eq!(keyboard.key(VK_RIGHT, true, WIN_SHIFT), Action::Consume);
        keyboard.state.target = Some(ShortcutTarget::Tile {
            id: 100,
            can_maximize: true,
        });
        keyboard.state.focus_epoch += 1;
        assert_eq!(keyboard.key(VK_UP, true, 0), Action::Pass);
    }

    #[test]
    fn native_keypresses_keep_their_repeats_and_up_even_if_modifiers_change() {
        let mut keyboard = Keyboard::new();
        for mods in [0, MOD_SHIFT, MOD_CONTROL, MOD_ALT, MOD_ALT | MOD_SHIFT] {
            assert_eq!(keyboard.key(VK_RIGHT, true, mods), Action::Pass);
            assert_eq!(keyboard.key(VK_RIGHT, true, MOD_WIN), Action::Pass);
            assert_eq!(keyboard.key(VK_RIGHT, false, MOD_WIN), Action::Pass);
        }
        assert_eq!(
            keyboard.key(VK_LEFT, true, MOD_ALT | MOD_CONTROL | WIN_SHIFT),
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
        k.state.target = None;
        assert_eq!(k.key(VK_K, false, 0), Action::Consume);
        assert_eq!(k.key(VK_H, true, MOD_ALT), Action::Command(FOCUS));
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
        assert_eq!(k.key(VK_UP, true, MOD_WIN), Action::Consume); // still bound to normal FocusUp
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
                Shortcut::GrowLeft.into(),
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
