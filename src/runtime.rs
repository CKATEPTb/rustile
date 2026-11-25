mod shortcuts;

use std::cell::RefCell;
use std::io;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND,
};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::StationsAndDesktops::{GetProcessWindowStation, GetThreadDesktop};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::Manager;
use crate::layout::Direction;
use crate::manager::Mode;
use crate::native::desktops::object_name;
use crate::settings::{Chord, Command, Modifier, Settings, Shortcut};
use crate::tray::Tray;

const START_RESIZE: i32 = 1;
const QUIT: i32 = 2;
const FOCUS: i32 = 10;
const EXPAND: i32 = 20;
const SHRINK: i32 = 30;
const CONFIRM: i32 = 40;
const CANCEL: i32 = 41;
const MOVE: i32 = 50;
const WINDOW_EVENTS: u32 = WM_APP + 1;
const KEY_COMMAND: u32 = WM_APP + 2;
pub(crate) const OUTLINE_SETTINGS_CHANGED: u32 = WM_APP + 3;
const DESKTOP_SETTLE_MS: u32 = 160;

// GetMessage dispatches out-of-context WinEvents on the hook's installing thread.
// Defer all application work until the callback returns, including Win32 calls
// that may themselves deliver more events.
thread_local! {
    static PENDING: RefCell<Vec<(u32, isize)>> = const { RefCell::new(Vec::new()) };
}

const ARROWS: [Direction; 4] = [
    Direction::Left,
    Direction::Right,
    Direction::Up,
    Direction::Down,
];

static LOOP_THREAD: AtomicU32 = AtomicU32::new(0);
static RESTORED: Mutex<bool> = Mutex::new(false);
static STOPPED: Condvar = Condvar::new();

unsafe extern "system" fn on_window_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if (hwnd.is_null() && event != EVENT_SYSTEM_DESKTOPSWITCH)
        || (event >= EVENT_OBJECT_CREATE && (object != OBJID_WINDOW || child != 0))
    {
        return;
    }
    // Controls can emit LOCATIONCHANGE at scroll/animation rates. Never query
    // the parent of a destroyed HWND; the manager still needs its removal.
    if event == EVENT_OBJECT_LOCATIONCHANGE && unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
        return;
    }
    let item = (event, hwnd as isize);
    let wake = PENDING.with_borrow_mut(|pending| {
        let first = pending.is_empty();
        if pending.last() != Some(&item) || event != EVENT_OBJECT_LOCATIONCHANGE {
            pending.push(item);
        }
        first
    });
    if wake {
        // Only one wake-up per batch. If the OS message queue is already full,
        // the loop drains PENDING on its next ordinary message as well.
        unsafe { PostThreadMessageW(GetCurrentThreadId(), WINDOW_EVENTS, 0, 0) };
    }
}

#[derive(Default)]
struct EventHooks(Vec<HWINEVENTHOOK>);

impl EventHooks {
    fn install() -> Result<Self, String> {
        let mut hooks = Self::default();
        for (first, last) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_DESKTOPSWITCH),
            (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ] {
            let handle = unsafe {
                SetWinEventHook(
                    first,
                    last,
                    null_mut(),
                    Some(on_window_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if handle.is_null() {
                return Err(format!(
                    "Подписка на события окон {first:#x}–{last:#x}: {}",
                    io::Error::last_os_error()
                ));
            }
            hooks.0.push(handle);
        }
        Ok(hooks)
    }
}

impl Drop for EventHooks {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            unsafe { UnhookWinEvent(handle) };
        }
    }
}

struct QuitHotkey {
    id: i32,
    chord: Chord,
}

impl QuitHotkey {
    fn register(id: i32, chord: Chord) -> Result<Self, String> {
        if unsafe {
            RegisterHotKey(
                null_mut(),
                id,
                chord.mods | MOD_NOREPEAT,
                u32::from(chord.key),
            )
        } == 0
        {
            return Err(format!(
                "Не удалось назначить {}: {}",
                chord.label(),
                io::Error::last_os_error()
            ));
        }
        Ok(Self { id, chord })
    }

    fn receives(&self, message: &MSG) -> bool {
        let key_and_modifiers = message.lParam as u32 & !MOD_NOREPEAT;
        message.message == WM_HOTKEY
            && message.wParam == self.id as usize
            && key_and_modifiers == ((u32::from(self.chord.key) << 16) | self.chord.mods)
    }
}

impl Drop for QuitHotkey {
    fn drop(&mut self) {
        unsafe { UnregisterHotKey(null_mut(), self.id) };
    }
}

unsafe extern "system" fn console_signal(signal: u32) -> i32 {
    if !matches!(signal, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT) {
        return 0;
    }
    let thread = LOOP_THREAD.load(Ordering::Acquire);
    if thread == 0 || unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0) } == 0 {
        return 0;
    }
    // Windows may terminate the process as soon as a close handler returns.
    if signal == CTRL_CLOSE_EVENT {
        let restored = RESTORED.lock().unwrap_or_else(|e| e.into_inner());
        let _result = STOPPED.wait_timeout_while(restored, Duration::from_secs(4), |done| !*done);
    }
    1
}

struct ConsoleHandler;

impl ConsoleHandler {
    fn install() -> Result<Self, String> {
        *RESTORED.lock().unwrap_or_else(|e| e.into_inner()) = false;
        LOOP_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
        if unsafe { SetConsoleCtrlHandler(Some(console_signal), 1) } == 0 {
            LOOP_THREAD.store(0, Ordering::Release);
            return Err(format!(
                "Обработчик Ctrl+C недоступен: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Self)
    }
}

impl Drop for ConsoleHandler {
    fn drop(&mut self) {
        *RESTORED.lock().unwrap_or_else(|e| e.into_inner()) = true;
        STOPPED.notify_all();
        LOOP_THREAD.store(0, Ordering::Release);
        unsafe { SetConsoleCtrlHandler(Some(console_signal), 0) };
    }
}

fn refresh(manager: &mut Manager, handled: usize) {
    let target = manager.refresh_focus();
    shortcuts::update(target, manager.mode(), handled);
}

fn dispatch(manager: &mut Manager, target: isize, command: i32) {
    let direction = |base| ARROWS.get((command - base) as usize).copied();
    match (manager.mode(), command) {
        (Mode::Normal, START_RESIZE) => {
            manager.begin_resize(target);
        }
        (Mode::Normal, _) => {
            if let Some(dir) = direction(MOVE) {
                manager.move_window(target, dir);
            } else if let Some(dir) = direction(FOCUS) {
                manager.focus(dir);
            }
        }
        (Mode::Resize, CANCEL) => manager.finish_mode(false),
        (Mode::Resize, CONFIRM) => manager.finish_mode(true),
        (Mode::Resize, _) => {
            if let Some(dir) = direction(EXPAND) {
                manager.resize(target, dir, false);
            } else if let Some(dir) = direction(SHRINK) {
                manager.resize(target, dir, true);
            }
        }
    }
}

fn show_error(detail: &str) {
    let message: Vec<u16> = detail.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            null_mut(),
            message.as_ptr(),
            windows_sys::w!("Rustile"),
            MB_ICONWARNING | MB_OK,
        )
    };
}

struct Preferences<'a> {
    live: Settings,
    saved: Settings,
    path: &'a Path,
    quit: Option<QuitHotkey>,
}

impl Preferences<'_> {
    fn apply(
        &mut self,
        manager: &mut Manager,
        hook: &shortcuts::Hook,
        tray: &mut Tray,
        command: Command,
    ) -> Result<(), String> {
        let next = self.live.changed(command)?;
        // The command-line resize shortcut applies only to this launch unless
        // the user explicitly chooses a new resize binding in the tray.
        let saved = self.saved.changed(command)?;
        let write = (saved != self.saved)
            .then(|| saved.prepare(self.path))
            .transpose()?;
        // Keep the old registration active until both registration and
        // persistence succeed. A failed save drops only the tentative handle.
        let new_quit = if matches!(command, Command::Shortcut(Shortcut::Quit, _))
            && self.quit.as_ref().map(|hotkey| hotkey.chord) != Some(next.chord(Shortcut::Quit))
        {
            let id = if self.quit.as_ref().is_some_and(|hotkey| hotkey.id == QUIT) {
                QUIT + 1
            } else {
                QUIT
            };
            Some(QuitHotkey::register(id, next.chord(Shortcut::Quit))?)
        } else {
            None
        };
        manager.apply_settings_with(next, || write.map_or(Ok(()), |pending| pending.commit()))?;
        if let Some(binding) = new_quit {
            self.quit = Some(binding);
        }
        self.live = next;
        self.saved = saved;
        hook.set_settings(next);
        tray.set_settings(next);
        refresh(manager, 0);
        Ok(())
    }
}

// Explorer may report visibility and desktop membership at different points in
// a switch. Recheck once after the burst, including switches to an empty desktop.
#[derive(Default)]
struct DesktopSettleTimer(usize);

impl DesktopSettleTimer {
    fn schedule(&mut self, delay_ms: u32) -> bool {
        let timer = unsafe { SetTimer(null_mut(), self.0, delay_ms, None) };
        if timer != 0 {
            self.0 = timer;
        }
        timer != 0
    }

    fn take(&mut self, message: &MSG) -> bool {
        if self.0 == 0
            || message.message != WM_TIMER
            || !message.hwnd.is_null()
            || message.wParam != self.0
        {
            return false;
        }
        self.cancel();
        true
    }

    fn cancel(&mut self) {
        if self.0 != 0 {
            unsafe { KillTimer(null_mut(), std::mem::take(&mut self.0)) };
        }
    }
}

impl Drop for DesktopSettleTimer {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn pump(
    manager: &mut Manager,
    hook: &shortcuts::Hook,
    tray: &mut Tray,
    preferences: &mut Preferences<'_>,
) -> Result<(), String> {
    let mut message = MSG::default();
    let mut events = Vec::new();
    let mut desktop_timer = DesktopSettleTimer::default();
    let mut retries = 0;
    if manager.take_desktop_retry() {
        retries = 2;
        if !desktop_timer.schedule(DESKTOP_SETTLE_MS) {
            manager.reconcile_desktop();
        }
    }
    loop {
        match unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } {
            -1 => {
                return Err(format!(
                    "Ошибка цикла сообщений: {}",
                    io::Error::last_os_error()
                ));
            }
            0 => return Ok(()),
            _ => {}
        }
        PENDING.with_borrow_mut(|pending| std::mem::swap(pending, &mut events));
        let has_events = !events.is_empty();
        let timer_fired = desktop_timer.take(&message);
        let mut settling = false;
        for (event, id) in events.drain(..) {
            settling |= matches!(
                event,
                EVENT_OBJECT_CLOAKED | EVENT_OBJECT_UNCLOAKED | EVENT_SYSTEM_DESKTOPSWITCH
            ) || (event == EVENT_OBJECT_HIDE && manager.tracks_window(id));
            if event != EVENT_SYSTEM_DESKTOPSWITCH {
                manager.event(event, id);
            }
        }
        let unresolved = manager.take_desktop_retry();
        let reconcile = if settling || (unresolved && desktop_timer.0 == 0 && !timer_fired) {
            retries = 2;
            !desktop_timer.schedule(DESKTOP_SETTLE_MS)
        } else {
            timer_fired
        };
        if reconcile {
            manager.reconcile_desktop();
            // A new top-level HWND can briefly have no desktop GUID. Give the
            // shell time to register it, without polling an idle desktop forever.
            if manager.take_desktop_retry() && retries > 0 {
                let delay = if retries == 2 { 2 } else { 4 };
                retries -= 1;
                desktop_timer.schedule(DESKTOP_SETTLE_MS * delay);
            }
        }
        if has_events || reconcile {
            refresh(manager, 0);
        }
        if preferences
            .quit
            .as_ref()
            .is_some_and(|hotkey| hotkey.receives(&message))
        {
            return Ok(());
        }
        if message.message == OUTLINE_SETTINGS_CHANGED {
            refresh(manager, 0);
        } else if message.message == KEY_COMMAND {
            // A window can lose focus between its physical key-down and delivery.
            let target = message.lParam;
            let command = (message.wParam & 0xff) as i32;
            let ticket = message.wParam >> 8;
            if target != 0 && target == crate::native::foreground() {
                dispatch(manager, target, command);
            }
            refresh(manager, ticket);
        } else if !tray.pre_translate(&message) {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        while let Some(command) = tray.take_command() {
            if command == Command::Exit {
                return Ok(());
            }
            let result = preferences.apply(manager, hook, tray, command);
            if let Err(detail) = &result {
                // The inline control must display the last accepted value when
                // the layout rejects a spacing change or the save fails.
                tray.set_settings(preferences.live);
                if !matches!(command, Command::Shortcut(..)) {
                    show_error(detail);
                }
            }
            if matches!(command, Command::Shortcut(..)) {
                tray.shortcut_result(result.as_ref().err().map(String::as_str));
            }
        }
    }
}

struct Instance(HANDLE);

impl Instance {
    fn acquire() -> Result<Self, String> {
        let desktop = object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })?;
        let name = if desktop.eq_ignore_ascii_case("Default") {
            "Local\\RustileWM".to_owned()
        } else {
            let station = object_name(unsafe { GetProcessWindowStation() })?;
            format!("Local\\RustileWM.{}.{station}.{desktop}", station.len())
        };
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(format!(
                "Проверка экземпляра Rustile: {}",
                io::Error::last_os_error()
            ));
        }
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let instance = Self(handle);
        if exists {
            return Err("Rustile уже работает в этом сеансе Windows.".to_owned());
        }
        Ok(instance)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

const USAGE: &str = "rustile [--resize-shortcut win+enter|ctrl+alt+enter]\nПо умолчанию размер изменяется через Win+Enter.";

pub fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let resize_override = match args.as_slice() {
        [] => None,
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{USAGE}");
            return Ok(());
        }
        [flag, value] if flag == "--resize-shortcut" && value.eq_ignore_ascii_case("win+enter") => {
            Some(Modifier::Win)
        }
        [flag, value]
            if flag == "--resize-shortcut" && value.eq_ignore_ascii_case("ctrl+alt+enter") =>
        {
            Some(Modifier::CtrlAlt)
        }
        _ => return Err(USAGE.to_owned()),
    };
    let _instance = Instance::acquire()?;
    let path = Settings::path()?;
    let (saved, load_warning) = match Settings::load(&path) {
        Ok(value) => (value, None),
        Err(detail) => (Settings::default(), Some(detail)),
    };
    let live = match resize_override {
        Some(modifier) => saved.with_resize_override(modifier)?,
        None => saved,
    };
    // Establish the thread message queue before any callback can post to it.
    unsafe { PeekMessageW(&mut MSG::default(), null_mut(), 0, 0, PM_NOREMOVE) };
    let exit_chord = live.chord(Shortcut::Quit);
    let quit = match QuitHotkey::register(QUIT, exit_chord) {
        Ok(binding) => {
            println!("{} — выйти и восстановить окна.", exit_chord.label());
            Some(binding)
        }
        Err(error) => {
            eprintln!("{error}");
            None
        }
    };
    let mut preferences = Preferences {
        live,
        saved,
        path: &path,
        quit,
    };
    let console = ConsoleHandler::install();
    match &console {
        Err(error) => eprintln!("{error}"),
        Ok(_) => println!("Ctrl+C в консоли Rustile — выйти и восстановить окна."),
    }
    let mut tray = Tray::new(live)?;
    let keyboard = shortcuts::Hook::install(live)?;
    let hooks = EventHooks::install()?;
    let mut manager = Manager::with_settings(live);
    manager.sync();
    refresh(&mut manager, 0);
    if let Some(warning) = load_warning {
        show_error(&format!("Применены настройки по умолчанию.\n\n{warning}"));
    }
    let result = pump(&mut manager, &keyboard, &mut tray, &mut preferences);
    drop(keyboard);
    drop(hooks);
    drop(preferences);
    manager.shutdown();
    drop(console);
    result
}
