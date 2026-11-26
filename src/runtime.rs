mod cli_commands;
mod desktop_watch;
mod dynamic_desktops;
mod instance;
mod ipc;
mod shortcuts;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::ptr::null_mut;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::layout::Direction;
use crate::manager::Mode;
use crate::native::desktops::DesktopId;
use crate::native::{self, TransferIdentity, WindowControl};
use crate::settings::{Command, Modifier, Settings};
use crate::startup;
use crate::tray::Tray;
use crate::{Manager, cli};

pub(crate) const START_RESIZE: i32 = 1;
pub(crate) const CLOSE_WINDOW: i32 = 2;
pub(crate) const TOGGLE_MAXIMIZE: i32 = 3;
pub(crate) const TOGGLE_MOSAIC: i32 = 4;
pub(crate) const DESKTOP_PREVIOUS: i32 = 5;
pub(crate) const DESKTOP_NEXT: i32 = 6;
pub(crate) const MOVE_DESKTOP_PREVIOUS: i32 = 7;
pub(crate) const MOVE_DESKTOP_NEXT: i32 = 8;
const FOCUS: i32 = 10;
pub(crate) const EXPAND: i32 = 20;
const SHRINK: i32 = 30;
pub(crate) const CONFIRM: i32 = 40;
pub(crate) const CANCEL: i32 = 41;
const MOVE: i32 = 50;
pub(crate) const QUICK_RESIZE: i32 = 60;
pub(crate) const QUICK_REVERSE: i32 = 70;
const DESKTOP_FIRST: i32 = 80;
const DESKTOP_TENTH: i32 = DESKTOP_FIRST + 9;
const MOVE_DESKTOP_FIRST: i32 = 90;
const MOVE_DESKTOP_TENTH: i32 = MOVE_DESKTOP_FIRST + 9;
const WINDOW_EVENTS: u32 = WM_APP + 1;
const KEY_COMMAND: u32 = WM_APP + 2;
pub(crate) const OUTLINE_SETTINGS_CHANGED: u32 = WM_APP + 3;
pub(crate) const RESIZE_HINT_CHANGED: u32 = WM_APP + 4;
const DESKTOP_LIST_CHANGED: u32 = WM_APP + 5;
const DESKTOP_SETTLE_MS: u32 = 160;
const FOCUS_SETTLE_MS: u32 = 40;
const FOCUS_CONFIRM_MS: u32 = 450;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopDestination {
    Relative { next: bool },
    Number(usize),
}

impl DesktopDestination {
    fn from_command(command: i32) -> Option<Self> {
        match command {
            DESKTOP_PREVIOUS | DESKTOP_NEXT => Some(Self::Relative {
                next: command == DESKTOP_NEXT,
            }),
            DESKTOP_FIRST..=DESKTOP_TENTH => {
                Some(Self::Number((command - DESKTOP_FIRST + 1) as usize))
            }
            _ => None,
        }
    }

    fn from_move_command(command: i32) -> Option<Self> {
        match command {
            MOVE_DESKTOP_PREVIOUS | MOVE_DESKTOP_NEXT => Some(Self::Relative {
                next: command == MOVE_DESKTOP_NEXT,
            }),
            MOVE_DESKTOP_FIRST..=MOVE_DESKTOP_TENTH => {
                Some(Self::Number((command - MOVE_DESKTOP_FIRST + 1) as usize))
            }
            _ => None,
        }
    }
}

fn is_desktop_move(command: i32) -> bool {
    DesktopDestination::from_move_command(command).is_some()
}

struct DesktopStep {
    destination: DesktopDestination,
    window: Option<Rc<TransferIdentity>>,
}

struct QueuedStep {
    step: DesktopStep,
    client: Option<ipc::Request>,
    explicit_window: bool,
}

struct CliArrival {
    client: ipc::Request,
    desktop: DesktopId,
}

// A successful switch RPC can precede Explorer's active-desktop change.
// Only acknowledge when a subsequent "desktop current" would see the target.
fn confirm_cli_arrival(
    pending: &mut Option<CliArrival>,
    workspaces: &dynamic_desktops::Workspaces,
    stopped: bool,
) {
    let Some(arrival) = pending.take() else {
        return;
    };
    if let Some((current, order)) = workspaces.snapshot()
        && current == arrival.desktop
        && let Some(index) = order.iter().position(|id| *id == current)
    {
        arrival.client.respond(Ok((index + 1).to_string()));
    } else if stopped || arrival.client.expired() {
        arrival.client.respond(Err(
            "Windows did not confirm the switch to the requested desktop.".into(),
        ));
    } else {
        *pending = Some(arrival);
    }
}

impl DesktopStep {
    fn from_shortcut(
        command: i32,
        target: isize,
        following: Option<&Rc<TransferIdentity>>,
        foreground: impl FnOnce() -> isize,
    ) -> Option<Self> {
        if let Some(destination) = DesktopDestination::from_move_command(command) {
            // Explorer may temporarily focus a different tile while switching.
            // Repeated presses during that handoff still belong to the same HWND.
            let window = if let Some(window) = following.filter(|window| window.matches()) {
                Some(Rc::clone(window))
            } else {
                if target != 0 && foreground() != target {
                    return None;
                }
                // Explorer can report no HWND, the wallpaper, or an idle taskbar.
                if native::desktop_foreground(target) == native::DesktopForeground::Vacant {
                    None
                } else {
                    Some(Rc::new(TransferIdentity::capture(target)?))
                }
            };
            return Some(Self {
                destination,
                window,
            });
        }
        if target == 0 {
            Some(Self {
                destination: DesktopDestination::from_command(command)?,
                window: None,
            })
        } else {
            None
        }
    }

    fn continues_transfer(&self, following: &Rc<TransferIdentity>) -> bool {
        self.window
            .as_ref()
            .is_some_and(|window| Rc::ptr_eq(window, following))
    }
}

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
    if (hwnd.is_null() && !matches!(event, EVENT_SYSTEM_DESKTOPSWITCH | EVENT_SYSTEM_FOREGROUND))
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
                    "Could not subscribe to window events {first:#x}–{last:#x}: {}",
                    crate::cli_error::io(io::Error::last_os_error())
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
                "Could not install the Ctrl+C handler: {}",
                crate::cli_error::io(io::Error::last_os_error())
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

pub(crate) fn dispatch_with_foreground(
    manager: &mut Manager,
    target: isize,
    command: i32,
    foreground: impl Fn() -> isize,
) -> bool {
    match command {
        CLOSE_WINDOW => manager.control_window(target, WindowControl::Close, foreground),
        TOGGLE_MAXIMIZE => {
            manager.control_window(target, WindowControl::ToggleMaximize, foreground)
        }
        _ if manager.accepts_foreground_shortcut(target) => {
            dispatch_mosaic(manager, target, command)
        }
        _ => return false,
    }
    true
}

pub(crate) fn dispatch_mosaic(manager: &mut Manager, target: isize, command: i32) {
    let direction = |base| ARROWS.get((command - base) as usize).copied();
    match (manager.mode(), command) {
        (Mode::Normal, START_RESIZE) => {
            manager.begin_resize(target);
        }
        (Mode::Normal, _) => {
            if let Some(dir) = direction(MOVE) {
                manager.move_window(target, dir);
            } else if let Some(dir) = direction(FOCUS) {
                manager.focus(target, dir);
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
        let saved = self.saved.changed_for_save(command, &self.live)?;
        let write = (saved != self.saved)
            .then(|| saved.prepare(self.path))
            .transpose()?;
        manager.apply_settings_with(next.clone(), || {
            write.map_or(Ok(()), |pending| pending.commit())
        })?;
        hook.set_settings(&next);
        tray.set_settings(next.clone());
        self.live = next;
        self.saved = saved;
        Ok(())
    }
}

#[derive(Default)]
struct MessageTimer {
    id: usize,
    deadline: Option<Instant>,
}

impl MessageTimer {
    fn schedule(&mut self, delay_ms: u32) -> bool {
        let timer = unsafe { SetTimer(null_mut(), self.id, delay_ms, None) };
        if timer != 0 {
            self.id = timer;
            self.deadline = Some(Instant::now() + Duration::from_millis(delay_ms.into()));
        }
        timer != 0
    }

    fn schedule_within(&mut self, delay_ms: u32) {
        let deadline = Instant::now() + Duration::from_millis(delay_ms.into());
        if self.deadline.is_none_or(|armed| deadline < armed) {
            self.schedule(delay_ms);
        }
    }

    fn matches(&self, message: &MSG) -> bool {
        self.id != 0
            && message.message == WM_TIMER
            && message.hwnd.is_null()
            && message.wParam == self.id
    }

    fn take(&mut self, message: &MSG) -> bool {
        if !self.matches(message) {
            return false;
        }
        self.cancel();
        true
    }

    fn cancel(&mut self) {
        if self.id != 0 {
            unsafe { KillTimer(null_mut(), std::mem::take(&mut self.id)) };
            self.deadline = None;
        }
    }
}

impl Drop for MessageTimer {
    fn drop(&mut self) {
        self.cancel();
    }
}

// Keep the arrival identity after the initial retries. Explorer can revoke an
// apparently successful activation or uncloak the first app much later.
#[derive(Default)]
struct DesktopFocus {
    observed: Option<DesktopId>,
    pending: Option<(DesktopId, usize)>,
    arrival: Option<DesktopId>,
    confirm_arrival: bool,
    switch_before_first_sample: bool,
}

impl DesktopFocus {
    fn awaiting_switch(&self) -> bool {
        self.pending
            .is_some_and(|(target, _)| Some(target) != self.observed)
    }

    fn requested(&mut self, from: DesktopId, to: DesktopId) {
        self.observed = Some(from);
        self.arrived_or_requested(to);
    }

    fn arrived_or_requested(&mut self, target: DesktopId) {
        self.pending = Some((target, 0));
        self.arrival = Some(target);
        self.confirm_arrival = true;
    }

    fn stop_watching(&mut self) {
        self.arrival = None;
        self.confirm_arrival = false;
    }

    fn recheck_arrival(&mut self, actual: Option<DesktopId>) {
        if self.pending.is_none()
            && let Some(target) = self.arrival
            && Some(target) == actual
        {
            self.pending = Some((target, 0));
        }
    }

    fn observe(&mut self, actual: Option<DesktopId>) -> bool {
        let Some(actual) = actual else { return false };
        let previous = self.observed.replace(actual);
        let initial_switch = std::mem::take(&mut self.switch_before_first_sample);
        if previous == Some(actual)
            || (previous.is_none() && !initial_switch)
            || self.pending.is_some_and(|(target, _)| target == actual)
        {
            return false;
        }
        self.arrived_or_requested(actual);
        true
    }

    fn settle(
        &mut self,
        actual: Option<DesktopId>,
        restore: impl FnOnce(DesktopId) -> bool,
    ) -> Option<u32> {
        let (target, attempt) = self.pending?;
        if actual == Some(target) && restore(target) {
            self.pending = None;
            return None;
        }
        // The target's layouts, foreground and DWM cloak can settle on
        // different messages. Stop entirely on a genuinely empty desktop.
        let delay = [50, 80, 130, 200, 320].get(attempt).copied();
        self.pending = delay.map(|_| (target, attempt + 1));
        delay
    }
}

fn pump(
    manager: &mut Manager,
    hook: &shortcuts::Hook,
    tray: &mut Tray,
    preferences: &mut Preferences<'_>,
    server: &ipc::Server,
) -> Result<Option<ipc::Request>, String> {
    let mut message = MSG::default();
    let mut events = Vec::new();
    let mut desktop_timer = MessageTimer::default();
    let mut inventory_timer = MessageTimer::default();
    let mut workspaces = dynamic_desktops::Workspaces::new();
    let mut focus_timer = MessageTimer::default();
    let mut navigation: VecDeque<QueuedStep> = VecDeque::new();
    let mut cli_arrival = None;
    let mut following: Option<Rc<TransferIdentity>> = None;
    let mut desktop_focus = DesktopFocus {
        observed: workspaces
            .current_desktop()
            .or_else(|| manager.layout_desktop()),
        ..Default::default()
    };
    let mut drag_timer = MessageTimer::default();
    let (startup_tx, startup_rx) = std::sync::mpsc::channel::<Result<startup::Startup, String>>();
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut desktop_watch = None;
    if workspaces.supported {
        desktop_watch = desktop_watch::DesktopWatch::start(thread_id, DESKTOP_LIST_CHANGED);
        inventory_timer.schedule(dynamic_desktops::EVENT_BATCH_MS);
    }
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
                    "Windows message loop failed: {}",
                    crate::cli_error::io(io::Error::last_os_error())
                ));
            }
            0 => return Ok(None),
            _ => {}
        }
        PENDING.with_borrow_mut(|pending| std::mem::swap(pending, &mut events));
        let has_events = !events.is_empty();
        let timer_fired = desktop_timer.take(&message);
        let inventory_due = inventory_timer.take(&message);
        let focus_due = focus_timer.take(&message);
        let desktops_changed = message.message == DESKTOP_LIST_CHANGED;
        let mut inventory_changed = desktops_changed;
        let work_area_changed = tray.take_work_area_change();
        // Some appbars reserve their edge without a settings broadcast. Read
        // rcWork once per event batch, before placing any newly shown HWNDs.
        let refitted = has_events && manager.work_area_changed();
        if refitted {
            manager.reconcile_desktop();
        }
        let mut settling = desktops_changed;
        let mut desktop_switch_event = false;
        let mut focus_event = false;
        for (event, id) in events.drain(..) {
            // Child controls and focus changes don't change which desktops have
            // windows. A destroyed HWND can no longer be checked for a parent.
            let changed_window = match event {
                EVENT_OBJECT_DESTROY | EVENT_SYSTEM_DESKTOPSWITCH => true,
                EVENT_OBJECT_CREATE
                | EVENT_OBJECT_SHOW
                | EVENT_OBJECT_HIDE
                | EVENT_OBJECT_CLOAKED
                | EVENT_OBJECT_UNCLOAKED => {
                    (unsafe { GetAncestor(id as HWND, GA_ROOT) }) == id as HWND
                }
                _ => false,
            };
            inventory_changed |=
                changed_window || (event == EVENT_SYSTEM_FOREGROUND && desktop_watch.is_none());
            focus_event |= changed_window
                || matches!(
                    event,
                    EVENT_SYSTEM_FOREGROUND | EVENT_SYSTEM_MINIMIZESTART | EVENT_SYSTEM_MINIMIZEEND
                );
            settling |= matches!(
                event,
                EVENT_OBJECT_CLOAKED | EVENT_OBJECT_UNCLOAKED | EVENT_SYSTEM_DESKTOPSWITCH
            ) || (event == EVENT_OBJECT_HIDE && manager.tracks_window(id));
            if event == EVENT_SYSTEM_DESKTOPSWITCH {
                desktop_switch_event = true;
                desktop_focus.switch_before_first_sample |= desktop_focus.observed.is_none();
            }
            if event == EVENT_SYSTEM_DESKTOPSWITCH || id == 0 {
                manager.finish_mode();
                manager.cancel_mouse_move();
            } else {
                manager.event(event, id);
            }
        }
        let unresolved = manager.take_desktop_retry();
        let timer_due = timer_fired && !refitted;
        let reconcile = if settling
            || ((unresolved || work_area_changed) && desktop_timer.id == 0 && !timer_due)
        {
            retries = 2;
            if !timer_due {
                desktop_timer.schedule_within(DESKTOP_SETTLE_MS);
            }
            timer_due || desktop_timer.id == 0
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
        let needs_scan = inventory_changed && workspaces.activity();
        if inventory_due {
            // The source can already look empty before Explorer exposes the
            // moved view on the destination. Let the bounded handoff finish
            // before the spare-desktop policy can remove or reorder either.
            if following.is_some() && desktop_focus.pending.is_some() {
                inventory_timer.schedule(dynamic_desktops::EVENT_BATCH_MS);
            } else if let Some(delay) = workspaces.reconcile() {
                inventory_timer.schedule(delay);
            }
            if workspaces.supported && desktop_watch.is_none() {
                desktop_watch = desktop_watch::DesktopWatch::start(thread_id, DESKTOP_LIST_CHANGED);
            }
        } else if needs_scan {
            inventory_timer.schedule_within(dynamic_desktops::EVENT_BATCH_MS);
        }
        if !shortcuts::transfer_focus_uninterrupted()
            && let Some(window) = following.take()
        {
            navigation.retain(|entry| !entry.step.continues_transfer(&window));
        }
        if !preferences.live.enabled || !shortcuts::desktop_focus_uninterrupted() {
            desktop_focus.stop_watching();
        }
        if (focus_event || reconcile)
            && desktop_focus.pending.is_none()
            && let Some(home) = desktop_focus.arrival
            && manager.desktop_foreground_on(native::foreground(), home)
                != native::DesktopForeground::Preserve
        {
            focus_timer.schedule_within(FOCUS_SETTLE_MS);
        }
        let mut destination = if message.message == KEY_COMMAND {
            DesktopStep::from_shortcut(
                (message.wParam & 0xff) as i32,
                message.lParam,
                following.as_ref(),
                native::foreground,
            )
        } else {
            None
        };
        if desktops_changed
            || desktop_switch_event
            || timer_fired
            || focus_due
            || (destination.is_some() && desktop_focus.awaiting_switch())
            || (desktop_watch.is_none()
                && (inventory_due || (has_events && !workspaces.available())))
        {
            let actual = if workspaces.available() {
                workspaces.current_desktop()
            } else {
                manager.layout_desktop()
            };
            if desktop_focus.observe(actual) {
                // Task View or another program selected a different desktop.
                shortcuts::watch_desktop_focus();
                navigation.clear();
                if destination
                    .as_ref()
                    .is_some_and(|step| step.window.is_some())
                {
                    destination = None;
                }
                following = None;
                shortcuts::end_transfer_focus_watch();
                if actual != manager.layout_desktop() {
                    desktop_timer.schedule_within(FOCUS_SETTLE_MS);
                }
                focus_timer.schedule(FOCUS_SETTLE_MS);
            } else if focus_due {
                desktop_focus.recheck_arrival(actual);
                let awaiting_switch = desktop_focus.awaiting_switch();
                let delay = desktop_focus.settle(actual, |id| match &following {
                    Some(window) => manager.restore_transferred_focus(
                        window,
                        id,
                        shortcuts::transfer_focus_uninterrupted,
                    ),
                    None => {
                        manager.restore_desktop_focus(id, shortcuts::desktop_focus_uninterrupted)
                    }
                });
                if let Some(delay) = delay {
                    focus_timer.schedule(delay);
                } else {
                    // One check after the Shell animation also catches a late
                    // loss without a foreground event. Further checks need an event.
                    if std::mem::take(&mut desktop_focus.confirm_arrival)
                        && desktop_focus.arrival == actual
                        && actual.is_some()
                    {
                        focus_timer.schedule(FOCUS_CONFIRM_MS);
                    }
                    if awaiting_switch {
                        navigation.clear();
                    }
                    if let Some(window) = following.take() {
                        shortcuts::end_transfer_focus_watch();
                        if native::foreground() != window.id() || !window.matches() {
                            navigation.retain(|entry| !entry.step.continues_transfer(&window));
                        }
                    }
                }
                refresh(manager, 0);
            }
            confirm_cli_arrival(
                &mut cli_arrival,
                &workspaces,
                !desktop_focus.awaiting_switch(),
            );
        }
        if drag_timer.matches(&message) {
            manager.refresh_drop_preview(false);
        }
        let mut shortcut_command = None;
        // A native modal loop can consume the IPC wake message. Check the
        // channel on return to the normal pump as well.
        if let Some(request) = server.try_recv() {
            let outcome = cli::Command::from_wire(&request.command).and_then(|command| {
                cli_commands::execute(command, manager, &workspaces, preferences, hook, tray)
            });
            match outcome {
                Ok(cli_commands::Outcome::Done(value)) => request.respond(Ok(value)),
                Ok(cli_commands::Outcome::Quit) => return Ok(Some(request)),
                Ok(cli_commands::Outcome::Navigate {
                    step,
                    explicit_window,
                }) => {
                    navigation.push_back(QueuedStep {
                        step,
                        explicit_window,
                        client: Some(request),
                    });
                    inventory_timer.schedule_within(dynamic_desktops::EVENT_BATCH_MS);
                }
                Err(error) => request.respond(Err(error)),
            }
            refresh(manager, 0);
        }
        if message.message == OUTLINE_SETTINGS_CHANGED {
            refresh(manager, 0);
        } else if message.message == RESIZE_HINT_CHANGED {
            manager.refresh_resize_hint(message.lParam);
        } else if message.message == KEY_COMMAND {
            // A window can lose focus between its physical key-down and delivery.
            let target = message.lParam;
            let command = (message.wParam & 0xff) as i32;
            let ticket = message.wParam >> 8;
            if command == TOGGLE_MOSAIC && target == 0 {
                shortcut_command = Some((Command::Toggle, ticket));
            } else if let Some(destination) = destination {
                if matches!(destination.destination, DesktopDestination::Number(_)) {
                    // A direct jump supersedes queued navigation. Preserve any
                    // window moves and the steps on which their destinations depend.
                    while navigation
                        .back()
                        .is_some_and(|entry| entry.step.window.is_none() && entry.client.is_none())
                    {
                        navigation.pop_back();
                    }
                }
                navigation.push_back(QueuedStep {
                    step: destination,
                    explicit_window: false,
                    client: None,
                });
                inventory_timer.schedule_within(dynamic_desktops::EVENT_BATCH_MS);
                if ticket != 0 {
                    refresh(manager, ticket);
                }
            } else {
                if !dispatch_with_foreground(manager, target, command, native::foreground)
                    && preferences.live.enabled
                    && shortcuts::is_focus_command(command)
                {
                    let foreground = native::foreground();
                    if shortcuts::desktop_focus_uninterrupted()
                        && (target == 0 || target == foreground)
                        && !desktop_focus.awaiting_switch()
                        && let Some(actual) = if workspaces.available() {
                            workspaces.current_desktop()
                        } else {
                            manager.layout_desktop()
                        }
                        && manager.desktop_foreground_on(foreground, actual)
                            != native::DesktopForeground::Preserve
                    {
                        // With no starting tile, this press selects an app.
                        // Subsequent directional presses navigate from it.
                        desktop_focus.arrived_or_requested(actual);
                        focus_timer.schedule_within(1);
                    }
                }
                refresh(manager, ticket);
            }
        } else if !tray.pre_translate(&message) {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        // A wheel can send several detents before Explorer reports the first
        // switch. Resolve the next step against its confirmed destination.
        while !desktop_focus.awaiting_switch()
            && cli_arrival.is_none()
            && !following.as_ref().is_some_and(|window| {
                navigation
                    .front()
                    .is_some_and(|entry| entry.step.continues_transfer(window))
            })
            && let Some(QueuedStep {
                step:
                    DesktopStep {
                        destination,
                        window,
                    },
                client,
                explicit_window,
            }) = navigation.pop_front()
        {
            let valid_target = |window: &TransferIdentity, from| {
                window.matches()
                    && if explicit_window {
                        manager.transferable_on(window.id(), from)
                    } else {
                        manager.transfer_target_is_valid(window.id(), from)
                    }
            };
            let client = if let Some(request) = client {
                if request.expired() {
                    continue;
                }
                if let Some(window) = &window
                    && !workspaces
                        .current_desktop()
                        .is_some_and(|from| valid_target(window, from))
                {
                    request.respond(Err(
                        "The selected window is no longer available to move.".into()
                    ));
                    continue;
                }
                if let Some(index) = workspaces.stationary_index(destination) {
                    request.respond(Ok(index.to_string()));
                    continue;
                }
                shortcuts::watch_desktop_focus();
                Some(request)
            } else {
                None
            };
            let (from, to, followed) = match &window {
                None => {
                    let Some((from, to)) = workspaces.navigate(destination) else {
                        if let Some(request) = client {
                            request
                                .respond(Err("Could not switch to the requested desktop.".into()));
                        }
                        continue;
                    };
                    (from, to, true)
                }
                Some(window) => {
                    shortcuts::watch_transfer_focus();
                    let moved = workspaces.transfer(
                        window.id(),
                        destination,
                        |from| valid_target(window, from),
                        shortcuts::transfer_focus_uninterrupted,
                    );
                    let Some(moved) = moved else {
                        shortcuts::end_transfer_focus_watch();
                        if let Some(request) = client {
                            request.respond(Err(
                                "Windows could not move the window to the requested desktop."
                                    .into(),
                            ));
                        }
                        continue;
                    };
                    if window.matches() {
                        manager.transferred_window(window.id(), moved.to);
                    }
                    inventory_timer.schedule_within(dynamic_desktops::EVENT_BATCH_MS);
                    (moved.from, moved.to, moved.followed)
                }
            };
            following = window.filter(|_| followed);
            if following.is_none() {
                shortcuts::end_transfer_focus_watch();
            }
            manager.finish_mode();
            manager.cancel_mouse_move();
            desktop_timer.schedule_within(FOCUS_SETTLE_MS);
            if followed {
                desktop_focus.requested(from, to);
                focus_timer.schedule(FOCUS_SETTLE_MS);
            }
            if let Some(request) = client {
                if followed {
                    cli_arrival = Some(CliArrival {
                        client: request,
                        desktop: to,
                    });
                    confirm_cli_arrival(&mut cli_arrival, &workspaces, false);
                } else {
                    request.respond(Err(
                        "The window moved, but Windows could not switch to its desktop.".into(),
                    ));
                }
            }
            refresh(manager, 0);
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
        while let Some((command, ticket)) = shortcut_command
            .take()
            .or_else(|| tray.take_command().map(|command| (command, 0)))
        {
            match command {
                Command::Exit => return Ok(None),
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
            refresh(manager, ticket);
            if let Err(detail) = &result {
                // The inline control must display the last accepted value when
                // the layout rejects a spacing change or the save fails.
                tray.set_settings(preferences.live.clone());
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
        } else if drag_timer.id == 0 {
            drag_timer.schedule(32);
        }
    }
}

pub(crate) fn run_with_args(args: &[String], from_terminal: bool) -> Result<(), String> {
    let logon = matches!(args, [flag] if matches!(flag.as_str(), startup::STARTUP_FLAG | startup::LEGACY_ELEVATED_FLAG));
    if logon && !startup::run_at_logon()? {
        return Ok(());
    }
    if logon || matches!(args, [flag] if flag == "--background") {
        if let Some(_owner) = instance::Instance::acquire()? {
            run_primary(None)?;
        }
        return Ok(());
    }
    match cli::parse(args)? {
        cli::Invocation::Print(text) => {
            println!("{text}");
            Ok(())
        }
        cli::Invocation::Request(command) => {
            if command == cli::Command::Quit {
                if !instance::Instance::running()? {
                    println!("ok");
                    return Ok(());
                }
            } else {
                ensure_running()?;
            }
            client_command(command)
        }
        cli::Invocation::Start(modifier)
            if from_terminal || args.first().is_some_and(|a| a == "start") =>
        {
            ensure_running()?;
            if let Some(modifier) = modifier {
                ipc::send(
                    &cli::Command::ResizeShortcut(modifier).to_wire(),
                    false,
                    false,
                )?;
            }
            client_command(cli::Command::Status)
        }
        cli::Invocation::Start(modifier) => match instance::Instance::acquire()? {
            Some(_owner) => run_primary(modifier),
            None => {
                client_command(modifier.map_or(cli::Command::Status, cli::Command::ResizeShortcut))
            }
        },
    }
}

fn client_command(command: cli::Command) -> Result<(), String> {
    let grant_foreground = matches!(
        command,
        cli::Command::DesktopGo(_)
            | cli::Command::Window {
                action: cli::WindowAction::Focus(_) | cli::WindowAction::ToDesktop(_),
                ..
            }
    );
    let output = ipc::send(
        &command.to_wire(),
        grant_foreground,
        command == cli::Command::Quit,
    )?;
    println!("{output}");
    Ok(())
}

fn ensure_running() -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    if instance::Instance::running()? {
        return Ok(());
    }
    // Concurrent cold commands can each start a candidate. Only the elected
    // owner stays running. Each caller submits its command once, over IPC.
    let child = instance::launch_background()?;
    let start = Instant::now();
    while !instance::Instance::running()? {
        if start.elapsed() >= Duration::from_secs(5) {
            return Err("Rustile did not confirm startup within 5 seconds.".into());
        }
        match unsafe { WaitForSingleObject(child.as_raw_handle(), 20) } {
            WAIT_TIMEOUT => {}
            WAIT_OBJECT_0 if instance::Instance::running()? => break,
            WAIT_OBJECT_0 => return Err("Rustile exited during startup.".into()),
            _ => return Err(crate::cli_error::io(io::Error::last_os_error())),
        }
    }
    Ok(())
}

fn run_primary(resize_override: Option<Modifier>) -> Result<(), String> {
    let path = Settings::path()?;
    let (saved, load_warning) = match Settings::load(&path) {
        Ok(value) => (value, None),
        Err(detail) => (Settings::default(), Some(detail)),
    };
    let live = match resize_override {
        Some(modifier) => saved.with_resize_override(modifier)?,
        None => saved.clone(),
    };
    // Establish the thread message queue before any callback can post to it.
    unsafe { PeekMessageW(&mut MSG::default(), null_mut(), 0, 0, PM_NOREMOVE) };
    let mut preferences = Preferences {
        live: live.clone(),
        saved,
        path: &path,
    };
    let console = ConsoleHandler::install();
    match &console {
        Err(error) => eprintln!("{error}"),
        Ok(_) => println!("Press Ctrl+C to quit Rustile and restore your windows."),
    }
    let mut tray = Tray::new(live.clone())?;
    let server = ipc::Server::start()?;
    let keyboard = shortcuts::Hook::install(&live)?;
    let hooks = EventHooks::install()?;
    let mut manager = Manager::with_settings(live);
    manager.sync();
    refresh(&mut manager, 0);
    if let Some(warning) = load_warning {
        show_error(&format!("Применены настройки по умолчанию.\n\n{warning}"));
    }
    let result = pump(
        &mut manager,
        &keyboard,
        &mut tray,
        &mut preferences,
        &server,
    );
    drop(keyboard);
    drop(hooks);
    manager.shutdown();
    drop(console);
    if let Some(request) = result? {
        request.respond(Ok("ok".into()));
    }
    Ok(())
}
