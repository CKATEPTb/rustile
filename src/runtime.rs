mod instance;
mod shortcuts;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::io;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::Manager;
use crate::layout::Direction;
use crate::manager::Mode;
use crate::settings::{Command, Modifier, Settings};
use crate::startup;
use crate::tray::Tray;

pub(crate) const START_RESIZE: i32 = 1;
const FOCUS: i32 = 10;
pub(crate) const EXPAND: i32 = 20;
const SHRINK: i32 = 30;
pub(crate) const CONFIRM: i32 = 40;
pub(crate) const CANCEL: i32 = 41;
const MOVE: i32 = 50;
pub(crate) const QUICK_RESIZE: i32 = 60;
pub(crate) const QUICK_REVERSE: i32 = 70;
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
    if matches!(
        event,
        EVENT_OBJECT_LOCATIONCHANGE | EVENT_OBJECT_STATECHANGE
    ) && unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd
    {
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
            (EVENT_OBJECT_STATECHANGE, EVENT_OBJECT_STATECHANGE),
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

pub(crate) fn dispatch(manager: &mut Manager, target: isize, command: i32) {
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
            } else if let Some(dir) = direction(QUICK_RESIZE) {
                manager.quick_resize(target, dir, false);
            } else if let Some(dir) = direction(QUICK_REVERSE) {
                manager.quick_resize(target, dir, true);
            }
        }
        (Mode::Resize, CANCEL | CONFIRM) => manager.finish_mode(),
        (Mode::Resize, _) => {
            if let Some(dir) = direction(EXPAND) {
                manager.resize(target, dir, false);
            } else if let Some(dir) = direction(SHRINK) {
                manager.resize(target, dir, true);
            }
        }
    }
}

pub(crate) fn show_error(detail: &str) {
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
        let saved = self.saved.changed_for_save(command, self.live)?;
        let write = (saved != self.saved)
            .then(|| saved.prepare(self.path))
            .transpose()?;
        manager.apply_settings_with(next, || write.map_or(Ok(()), |pending| pending.commit()))?;
        self.live = next;
        self.saved = saved;
        hook.set_settings(next);
        tray.set_settings(next);
        refresh(manager, 0);
        Ok(())
    }
}

#[derive(Default)]
struct MessageTimer(usize);

impl MessageTimer {
    fn schedule(&mut self, delay_ms: u32) -> bool {
        let timer = unsafe { SetTimer(null_mut(), self.0, delay_ms, None) };
        if timer != 0 {
            self.0 = timer;
        }
        timer != 0
    }

    fn matches(&self, message: &MSG) -> bool {
        self.0 != 0
            && message.message == WM_TIMER
            && message.hwnd.is_null()
            && message.wParam == self.0
    }

    fn take(&mut self, message: &MSG) -> bool {
        if !self.matches(message) {
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

impl Drop for MessageTimer {
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
    let mut desktop_timer = MessageTimer::default();
    let mut drag_timer = MessageTimer::default();
    let (startup_tx, startup_rx) = std::sync::mpsc::channel::<Result<startup::Startup, String>>();
    let thread_id = unsafe { GetCurrentThreadId() };
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
        let work_area_changed = tray.take_work_area_change();
        // Some appbars reserve their edge without a settings broadcast. Read
        // rcWork once per event batch, before placing any newly shown HWNDs.
        let refitted = has_events && manager.work_area_changed();
        if refitted {
            manager.reconcile_desktop();
        }
        let mut settling = false;
        for (event, id) in events.drain(..) {
            settling |= matches!(
                event,
                EVENT_OBJECT_CLOAKED | EVENT_OBJECT_UNCLOAKED | EVENT_SYSTEM_DESKTOPSWITCH
            ) || (event == EVENT_OBJECT_HIDE && manager.tracks_window(id));
            if event == EVENT_SYSTEM_DESKTOPSWITCH {
                manager.finish_mode();
                manager.cancel_mouse_move();
            } else {
                manager.event(event, id);
            }
        }
        let unresolved = manager.take_desktop_retry();
        let timer_due = timer_fired && !refitted;
        let reconcile = if settling
            || ((unresolved || work_area_changed) && desktop_timer.0 == 0 && !timer_due)
        {
            retries = 2;
            !desktop_timer.schedule(DESKTOP_SETTLE_MS)
        } else {
            timer_due
        };
        if reconcile {
            manager.reconcile_desktop();
            // Desktop membership, window styles and app-restored bounds may
            // settle at different times. Keep retries finite on an idle desktop.
            if manager.take_desktop_retry() && retries > 0 {
                let delay = if retries == 2 { 2 } else { 4 };
                retries -= 1;
                desktop_timer.schedule(DESKTOP_SETTLE_MS * delay);
            }
        }
        if has_events || reconcile {
            refresh(manager, 0);
        }
        if drag_timer.matches(&message) {
            manager.refresh_drop_preview(false);
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
        if let Ok(result) = startup_rx.try_recv() {
            match result {
                Ok(actual) => tray.set_startup(Some(actual)),
                Err(detail) => {
                    tray.set_startup(startup::Startup::read().ok());
                    show_error(&detail);
                }
            }
        }
        while let Some(command) = tray.take_command() {
            match command {
                Command::Exit => return Ok(()),
                Command::SetStartup(mode) => {
                    if !tray.begin_startup_change() {
                        continue;
                    }
                    // A UAC prompt can stay open indefinitely. Keep processing
                    // WinEvents and the keyboard hook on their installing thread.
                    let sender = startup_tx.clone();
                    if let Err(error) =
                        std::thread::Builder::new()
                            .name("startup".into())
                            .spawn(move || {
                                if sender.send(mode.apply()).is_ok() {
                                    unsafe { PostThreadMessageW(thread_id, WM_NULL, 0, 0) };
                                }
                            })
                    {
                        tray.set_startup(startup::Startup::read().ok());
                        show_error(&format!("Не удалось начать настройку автозапуска: {error}"));
                    }
                    continue;
                }
                _ => {}
            }
            let result = preferences.apply(manager, hook, tray, command);
            if let Err(detail) = &result {
                // The inline control must display the last accepted value when
                // the layout rejects a spacing change or the save fails.
                tray.set_settings(preferences.live);
                if !command.changes_shortcuts() {
                    show_error(detail);
                }
            }
            if command.changes_shortcuts() {
                tray.shortcut_result(result.as_ref().err().map(String::as_str));
            }
        }
        if !manager.tracking_drag_cursor() {
            drag_timer.cancel();
        } else if drag_timer.0 == 0 {
            drag_timer.schedule(32);
        }
    }
}

const USAGE: &str = "rustile [--resize-shortcut win+enter|ctrl+alt+enter]\nПо умолчанию размер изменяется через Win+Enter.";

pub(crate) fn run_with_args(args: &[String]) -> Result<(), String> {
    let resize_override = match args {
        [] => None,
        [flag]
            if matches!(
                flag.as_str(),
                startup::STARTUP_FLAG | startup::LEGACY_ELEVATED_FLAG
            ) =>
        {
            if !startup::run_at_logon()? {
                return Ok(());
            }
            None
        }
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
    let _instance = instance::Instance::acquire()?;
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
    let mut preferences = Preferences {
        live,
        saved,
        path: &path,
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
    manager.shutdown();
    drop(console);
    result
}
