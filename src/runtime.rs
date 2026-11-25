mod shortcuts;

use std::cell::RefCell;
use std::io;
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
use windows_sys::Win32::System::StationsAndDesktops::{
    GetProcessWindowStation, GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::Manager;
use crate::layout::Direction;
use crate::manager::Mode;

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
    if hwnd.is_null() || (event >= EVENT_OBJECT_CREATE && (object != OBJID_WINDOW || child != 0)) {
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

#[derive(Default)]
struct Hotkeys(Vec<i32>);

impl Hotkeys {
    fn bind(&mut self, id: i32, mods: u32, key: u16, label: &str) -> Result<(), String> {
        if unsafe { RegisterHotKey(null_mut(), id, mods, u32::from(key)) } == 0 {
            return Err(format!(
                "Сочетание {label} недоступно: {}",
                io::Error::last_os_error()
            ));
        }
        self.0.push(id);
        Ok(())
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            unsafe { UnregisterHotKey(null_mut(), id) };
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

fn pump(manager: &mut Manager) -> Result<(), String> {
    let mut message = MSG::default();
    let mut events = Vec::new();
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
        if !events.is_empty() {
            for (event, id) in events.drain(..) {
                manager.event(event, id);
            }
            refresh(manager, 0);
        }
        if message.message == WM_HOTKEY && message.wParam == QUIT as usize {
            return Ok(());
        }
        if message.message == OUTLINE_SETTINGS_CHANGED {
            refresh(manager, 0);
            continue;
        }
        if message.message != KEY_COMMAND {
            unsafe { DispatchMessageW(&message) };
            continue;
        }

        // A window can lose focus between its physical key-down and delivery.
        let target = message.lParam;
        let command = (message.wParam & 0xff) as i32;
        let ticket = message.wParam >> 8;
        if target != 0 && target == crate::native::foreground() {
            dispatch(manager, target, command);
        }
        refresh(manager, ticket);
    }
}
struct Instance(HANDLE);

fn object_name(handle: HANDLE) -> Result<String, String> {
    let mut bytes = 0;
    unsafe { GetUserObjectInformationW(handle, UOI_NAME, null_mut(), 0, &mut bytes) };
    let mut name = vec![0u16; (bytes as usize).div_ceil(2)];
    if bytes == 0
        || unsafe {
            GetUserObjectInformationW(
                handle,
                UOI_NAME,
                name.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        } == 0
    {
        return Err(format!(
            "Имя рабочего стола: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(String::from_utf16_lossy(
        name.strip_suffix(&[0]).unwrap_or(&name),
    ))
}

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
    let (mods, label) = match args.as_slice() {
        [] => (MOD_WIN, "Win+Enter"),
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{USAGE}");
            return Ok(());
        }
        [flag, value] if flag == "--resize-shortcut" && value.eq_ignore_ascii_case("win+enter") => {
            (MOD_WIN, "Win+Enter")
        }
        [flag, value]
            if flag == "--resize-shortcut" && value.eq_ignore_ascii_case("ctrl+alt+enter") =>
        {
            (MOD_CONTROL | MOD_ALT, "Ctrl+Alt+Enter")
        }
        _ => return Err(USAGE.to_owned()),
    };
    let _instance = Instance::acquire()?;
    // Establish the thread message queue before any callback can post to it.
    unsafe { PeekMessageW(&mut MSG::default(), null_mut(), 0, 0, PM_NOREMOVE) };
    let mut bindings = Hotkeys::default();
    let quit = bindings.bind(
        QUIT,
        MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT,
        VK_Q,
        "Ctrl+Alt+Shift+Q",
    );
    let console = ConsoleHandler::install();
    if let (Err(key), Err(handler)) = (&quit, &console) {
        return Err(format!(
            "{key}. {handler}. Для запуска необходим способ штатного выхода."
        ));
    }
    match &quit {
        Err(error) => eprintln!("{error}"),
        Ok(()) => println!("Ctrl+Alt+Shift+Q — выйти и восстановить окна."),
    }
    match &console {
        Err(error) => eprintln!("{error}"),
        Ok(_) => println!("Ctrl+C в консоли Rustile — выйти и восстановить окна."),
    }
    let keyboard = shortcuts::Hook::install(mods)?;
    let hooks = EventHooks::install()?;
    let mut manager = Manager::new();
    manager.sync();
    refresh(&mut manager, 0);
    println!("Win+стрелки — перевести фокус между плитками.");
    println!("Win+Shift+стрелка — переместить плитку.");
    println!(
        "{label} — изменить размер. Стрелки двигают разделитель в обе стороны, Shift+стрелка сужает плитку с выбранной стороны. Win можно удерживать. Enter — применить, Esc — отменить."
    );
    let result = pump(&mut manager);
    drop(keyboard);
    drop(hooks);
    drop(bindings);
    manager.shutdown();
    drop(console);
    result
}
