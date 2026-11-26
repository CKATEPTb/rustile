pub(crate) mod access;
pub(crate) mod desktops;

use crate::layout::{Id, Rect};
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWM_CLOAKED_APP, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DEFAULT, DWMWCP_DONOTROUND, DWMWCP_ROUNDSMALL, DwmGetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, GetWindowRgnBox, HMONITOR, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, MonitorFromWindow, SIMPLEREGION,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub fn init_dpi() {
    // Must precede window creation and every geometry query.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFO> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    (!monitor.is_null() && unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then_some(info)
}

fn rect(bounds: RECT) -> Rect {
    Rect {
        x: bounds.left,
        y: bounds.top,
        w: bounds.right - bounds.left,
        h: bounds.bottom - bounds.top,
    }
}

pub fn work_area() -> Option<Rect> {
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    monitor_info(monitor).and_then(|info| usable_work_area(info.rcWork))
}

fn usable_work_area(bounds: RECT) -> Option<Rect> {
    (bounds.left < bounds.right && bounds.top < bounds.bottom).then(|| rect(bounds))
}

pub fn system_dpi() -> u32 {
    unsafe { GetDpiForSystem() }.max(96)
}

pub fn scaled_spacing(dip: u8, dpi: u32) -> i32 {
    let pixels = (u64::from(dip) * u64::from(dpi) + 48) / 96;
    pixels.min(i32::MAX as u64) as i32
}

pub fn spacing_px(dip: u8) -> i32 {
    scaled_spacing(dip, system_dpi())
}

/// Windows does not expose a composited corner radius. Follow the application's
/// DWM preference without modifying it or reshaping its window.
pub fn corner_radius_dip(id: Id) -> i32 {
    let hwnd = id as HWND;
    let mut preference = DWMWCP_DEFAULT;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&raw mut preference).cast(),
            size_of_val(&preference) as u32,
        );
    }
    match preference {
        DWMWCP_DONOTROUND => 0,
        DWMWCP_ROUNDSMALL => 4,
        // Some apps provide their own rectangular clipping region. DWM leaves
        // that shape alone even when the corner preference is at its default.
        DWMWCP_DEFAULT
            if unsafe { GetWindowRgnBox(hwnd, &mut RECT::default()) } == SIMPLEREGION =>
        {
            0
        }
        _ => 8,
    }
}

unsafe extern "system" fn append_window(hwnd: HWND, context: LPARAM) -> i32 {
    // EnumWindows invokes this synchronously with our live Vec from enumerate.
    unsafe { &mut *(context as *mut Vec<Id>) }.push(hwnd as Id);
    1
}

pub fn enumerate() -> Vec<Id> {
    let mut windows = Vec::new();
    unsafe { EnumWindows(Some(append_window), &mut windows as *mut Vec<Id> as LPARAM) };
    windows.reverse();
    windows
}

pub fn foreground() -> Id {
    unsafe { GetForegroundWindow() as Id }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopForeground {
    Vacant,
    Transient,
    Preserve,
}

// A floating app, dialog or taskbar control can own legitimate input even
// though it is not a tile. The Win32 desktop root itself never belongs to an app.
pub(crate) fn desktop_foreground(id: Id) -> DesktopForeground {
    use DesktopForeground::*;
    let hwnd = id as HWND;
    if hwnd.is_null() || hwnd == unsafe { GetDesktopWindow() } {
        return Vacant;
    }
    let Some(class) = window_class(hwnd) else {
        return Transient;
    };
    let shell = unsafe { GetShellWindow() };
    let shell_process = !shell.is_null() && pid(shell as Id) == pid(id);
    let switcher = shell_process
        && matches!(
            class.as_str(),
            "MultitaskingViewFrame" | "TaskSwitcherWnd" | "XamlExplorerHostIslandWindow"
        );
    let wallpaper = hwnd == shell || (shell_process && is_desktop_view(hwnd, shell, &class));
    let taskbar =
        shell_process && matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd");
    if !wallpaper && !taskbar && !hidden_or_minimized(id) && cloak_flags(id).is_none_or(|f| f == 0)
    {
        return if switcher { Transient } else { Preserve };
    }
    // A closed or cloaked switcher can remain the nominal foreground after
    // the desktop has changed. Only visible surfaces or live GUI interaction
    // should keep the incoming desktop from receiving keyboard focus.
    let Some(gui) = window_gui_state(hwnd) else {
        return Transient;
    };
    if gui_input_in_use(&gui)
        || (taskbar && taskbar_control_has_focus(hwnd, gui.hwndFocus))
        || (wallpaper
            && !gui.hwndFocus.is_null()
            && window_class(gui.hwndFocus).as_deref() == Some("Edit"))
    {
        return Preserve;
    }
    Vacant
}

fn window_gui_state(hwnd: HWND) -> Option<GUITHREADINFO> {
    let thread = unsafe { GetWindowThreadProcessId(hwnd, null_mut()) };
    let mut gui = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    (thread != 0 && unsafe { GetGUIThreadInfo(thread, &mut gui) } != 0).then_some(gui)
}

pub(crate) fn app_input_idle(id: Id) -> bool {
    window_gui_state(id as HWND).is_some_and(|gui| !gui_input_in_use(&gui))
}

fn gui_input_in_use(gui: &GUITHREADINFO) -> bool {
    let interacting = GUI_INMENUMODE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE | GUI_INMOVESIZE;
    gui.flags & interacting != 0 || !gui.hwndCapture.is_null() || !gui.hwndMenuOwner.is_null()
}

fn taskbar_control_has_focus(taskbar: HWND, focus: HWND) -> bool {
    // Explorer can leave focus on the taskbar frame itself while handing
    // off desktops. Win+T and tray keyboard navigation can focus a control.
    !focus.is_null() && unsafe { IsChild(taskbar, focus) } != 0
}

fn is_desktop_view(hwnd: HWND, shell: HWND, class: &str) -> bool {
    if !matches!(class, "SHELLDLL_DefView" | "WorkerW") {
        return false;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if root == shell {
        return true;
    }
    // File Explorer also hosts SHELLDLL_DefView. Only its desktop WorkerW
    // has the desktop icon view directly beneath the top-level worker.
    if window_class(root).as_deref() != Some("WorkerW") {
        return false;
    }
    let view = unsafe {
        FindWindowExW(
            root,
            null_mut(),
            windows_sys::w!("SHELLDLL_DefView"),
            null(),
        )
    };
    !view.is_null() && (hwnd == root || hwnd == view)
}

pub fn cursor_position() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    (unsafe { GetCursorPos(&mut point) } != 0).then_some((point.x, point.y))
}

// With full-window dragging disabled the HWND stays put until release. Ask
// the app whether Windows is dragging its caption before following the cursor.
pub fn outline_caption_drag(id: Id) -> bool {
    let mut full = 1i32;
    if unsafe { SystemParametersInfoW(SPI_GETDRAGFULLWINDOWS, 0, (&raw mut full).cast(), 0) } == 0
        || full != 0
    {
        return false;
    }
    let Some((x, y)) = cursor_position() else {
        return false;
    };
    let (Ok(x), Ok(y)) = (i16::try_from(x), i16::try_from(y)) else {
        return false;
    };
    let location = i32::from(x as u16) | (i32::from(y) << 16);
    let mut hit = 0;
    unsafe {
        SendMessageTimeoutW(
            id as HWND,
            WM_NCHITTEST,
            0,
            location as LPARAM,
            SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
            25,
            &mut hit,
        ) != 0
            && hit == HTCAPTION as usize
    }
}

pub fn is_snapped(id: Id) -> bool {
    unsafe { IsWindowArranged(id as HWND) != 0 }
}

pub fn pid(id: Id) -> u32 {
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(id as HWND, &mut owner) };
    owner
}

pub(crate) fn window_title(id: Id) -> String {
    let hwnd = id as HWND;
    let capacity = unsafe { GetWindowTextLengthW(hwnd) }.clamp(0, 32_767) + 1;
    let mut title = vec![0u16; capacity as usize];
    let length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), capacity) };
    String::from_utf16_lossy(&title[..length as usize])
}

fn ordinary_process(owner: u32) -> bool {
    if owner == 0 || owner == std::process::id() {
        return false;
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, owner);
        if process.is_null() {
            return false;
        }
        let mut path = [0u16; 1024];
        let mut len = path.len() as u32;
        let has_image = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) != 0;
        let allowed = has_image
            && access::ProcessAccess::current().is_some_and(|manager| {
                access::ProcessAccess::query(process)
                    .is_some_and(|target| manager.can_tile(&target))
            });
        CloseHandle(process);
        if !allowed {
            return false;
        }
        let image = String::from_utf16_lossy(&path[..len as usize]);
        let executable = image.rsplit('\\').next().unwrap_or_default();
        ![
            "ShellExperienceHost.exe",
            "ShellHost.exe",
            "SearchHost.exe",
            "SearchApp.exe",
            "StartMenuExperienceHost.exe",
            "TextInputHost.exe",
            "LockApp.exe",
            "LogonUI.exe",
            "consent.exe",
            "GameBar.exe",
            "GameBarFTServer.exe",
        ]
        .iter()
        .any(|shell| executable.eq_ignore_ascii_case(shell))
    }
}

fn window_class(hwnd: HWND) -> Option<String> {
    let mut class = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    (len > 0).then(|| String::from_utf16_lossy(&class[..len as usize]))
}

fn shell_or_dialog(hwnd: HWND) -> bool {
    let Some(class) = window_class(hwnd) else {
        return true;
    };
    matches!(
        class.as_str(),
        "#32770"
            | "#32768"
            | "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "NotifyIconOverflowWindow"
            | "TopLevelWindowForOverflowXamlIsland"
            | "tooltips_class32"
            | "SysShadow"
            | "TaskListThumbnailWnd"
            | "MultitaskingViewFrame"
            | "XamlExplorerHostIslandWindow"
    )
}

fn outer_bounds(hwnd: HWND) -> Option<Rect> {
    let mut raw = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut raw) } == 0 {
        return None;
    }
    let bounds = rect(raw);
    (bounds.w > 0 && bounds.h > 0).then_some(bounds)
}

fn measured_frame(hwnd: HWND, outer: Rect) -> Option<Rect> {
    let mut raw = RECT::default();
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut raw as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    };
    let visible = rect(raw);
    // After an async move DWM may still describe the preceding frame. Only
    // accept offsets that can plausibly describe the invisible resize border.
    let inset = [
        visible.x - outer.x,
        visible.y - outer.y,
        outer.x + outer.w - (visible.x + visible.w),
        outer.y + outer.h - (visible.y + visible.h),
    ];
    (result >= 0 && visible.w > 0 && visible.h > 0 && inset.iter().all(|n| (0..=64).contains(n)))
        .then_some(visible)
}

fn frame_bounds(hwnd: HWND, outer: Rect) -> Rect {
    measured_frame(hwnd, outer).unwrap_or(outer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cloak {
    None,
    Application,
    Other,
}

fn cloak_flags(id: Id) -> Option<u32> {
    let mut cloak = 0u32;
    let result = unsafe {
        DwmGetWindowAttribute(
            id as HWND,
            DWMWA_CLOAKED as u32,
            (&mut cloak as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    (result >= 0).then_some(cloak)
}

pub(crate) fn cloak_state(id: Id) -> Cloak {
    match cloak_flags(id) {
        Some(0) => Cloak::None,
        Some(DWM_CLOAKED_APP) => Cloak::Application,
        // Shell, inherited or mixed flags can belong to a desktop switch.
        // A failed DWM query also gives no evidence of an application hide.
        _ => Cloak::Other,
    }
}

pub fn cloaked(id: Id) -> bool {
    cloak_state(id) != Cloak::None
}

pub fn minimized_or_maximized(id: Id) -> bool {
    unsafe { IsIconic(id as HWND) != 0 || IsZoomed(id as HWND) != 0 }
}

pub fn maximized(id: Id) -> bool {
    unsafe { IsZoomed(id as HWND) != 0 }
}

pub fn window_enabled(id: Id) -> bool {
    unsafe { IsWindowEnabled(id as HWND) != 0 }
}

pub fn can_maximize(id: Id) -> bool {
    unsafe { GetWindowLongW(id as HWND, GWL_STYLE) as u32 & WS_MAXIMIZEBOX != 0 }
}

pub fn close_window(id: Id) -> bool {
    // The owning UI thread handles save prompts and may cancel or hide to tray.
    unsafe { PostMessageW(id as HWND, WM_CLOSE, 0, 0) != 0 }
}

pub fn set_maximized(id: Id, maximize: bool) -> bool {
    let command = if maximize { SC_MAXIMIZE } else { SC_RESTORE };
    unsafe { PostMessageW(id as HWND, WM_SYSCOMMAND, command as usize, 0) != 0 }
}

pub fn hidden_or_minimized(id: Id) -> bool {
    unsafe { IsWindowVisible(id as HWND) == 0 || IsIconic(id as HWND) != 0 }
}

/// Visible frame and work area for an already-managed tile on the primary display.
pub fn outline_frame(id: Id) -> Option<(Rect, Rect)> {
    let hwnd = id as HWND;
    unsafe {
        if IsWindowVisible(hwnd) == 0
            || minimized_or_maximized(id)
            || IsWindowEnabled(hwnd) == 0
            || cloaked(id)
        {
            return None;
        }
        let info = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL))?;
        if info.dwFlags & MONITORINFOF_PRIMARY == 0 {
            return None;
        }
        Some((
            frame_bounds(hwnd, outer_bounds(hwnd)?),
            usable_work_area(info.rcWork)?,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TileState {
    Eligible,
    Maximized,
    Fullscreen,
    Excluded,
}

fn untracked_in_reserved_strip(frame: Rect, screen: Rect, work: Rect, tracked: bool) -> bool {
    let right = |r: Rect| r.x.saturating_add(r.w);
    let bottom = |r: Rect| r.y.saturating_add(r.h);
    !tracked
        && frame.w > 0
        && frame.h > 0
        && frame.x >= screen.x
        && right(frame) <= right(screen)
        && frame.y >= screen.y
        && bottom(frame) <= bottom(screen)
        && (right(frame) <= work.x
            || frame.x >= right(work)
            || bottom(frame) <= work.y
            || frame.y >= bottom(work))
}

fn visible_app_style(id: Id) -> Option<(u32, u32)> {
    let hwnd = id as HWND;
    if hwnd.is_null() {
        return None;
    }
    unsafe {
        if IsWindowVisible(hwnd) == 0
            || IsIconic(hwnd) != 0
            || GetAncestor(hwnd, GA_ROOT) != hwnd
            || !GetWindow(hwnd, GW_OWNER).is_null()
            || hwnd == GetShellWindow()
            || IsHungAppWindow(hwnd) != 0
        {
            return None;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let extended = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_CHILD != 0
            || extended & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_DLGMODALFRAME) != 0
            || cloaked(id)
            || shell_or_dialog(hwnd)
        {
            return None;
        }
        Some((style, extended))
    }
}

/// Return the top-level app that can vouch for this focus target's desktop.
/// A modal dialog may be the only enabled window of that app.
pub(crate) fn focus_app_owner(id: Id) -> Option<Id> {
    let hwnd = id as HWND;
    if !window_enabled(id) {
        return None;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
    visible_app_style(root as Id)?;
    if root != hwnd {
        let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
        if owner.is_null()
            || window_enabled(owner as Id)
            || window_enabled(root as Id)
            || pid(id) != pid(root as Id)
            || hidden_or_minimized(id)
            || cloaked(id)
            || unsafe { IsHungAppWindow(hwnd) } != 0
            || unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_NOACTIVATE != 0
            || (unsafe { GetLastActivePopup(root) } != hwnd
                && unsafe { GetWindow(owner, GW_ENABLEDPOPUP) } != hwnd)
        {
            return None;
        }
    }
    let info = monitor_info(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) })?;
    let bounds = outer_bounds(hwnd)?;
    let frame = frame_bounds(hwnd, bounds);
    let work = rect(info.rcWork);
    (!untracked_in_reserved_strip(frame, rect(info.rcMonitor), work, false)
        && ordinary_process(pid(id)))
    .then_some(root as Id)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowUse {
    Tiling,
    Maximize,
    CloseOrTransfer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowControl {
    Close,
    ToggleMaximize,
}

pub(crate) fn tile_state(id: Id) -> TileState {
    classify_window(id, WindowUse::Tiling)
}

pub(crate) fn can_close_or_transfer(id: Id) -> bool {
    window_enabled(id) && classify_window(id, WindowUse::CloseOrTransfer) != TileState::Excluded
}

fn classify_window(id: Id, usage: WindowUse) -> TileState {
    let hwnd = id as HWND;
    let Some((style, extended)) = visible_app_style(id) else {
        return TileState::Excluded;
    };
    let resizable = style & WS_THICKFRAME != 0;
    let borderless = style & WS_CAPTION != WS_CAPTION;
    if usage == WindowUse::Tiling && !resizable && !borderless {
        return TileState::Excluded;
    }
    unsafe {
        let Some(info) = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL)) else {
            return TileState::Excluded;
        };
        if usage == WindowUse::Tiling && info.dwFlags & MONITORINFOF_PRIMARY == 0 {
            return TileState::Excluded;
        }
        let Some(outer) = outer_bounds(hwnd) else {
            return TileState::Excluded;
        };
        let frame = frame_bounds(hwnd, outer);
        let screen = rect(info.rcMonitor);
        let tracked = !GetPropW(hwnd, marker_name()).is_null();
        let work = rect(info.rcWork);
        if work.w > 0 && work.h > 0 && untracked_in_reserved_strip(frame, screen, work, tracked) {
            return TileState::Excluded;
        }
        let covers_monitor = frame.x <= screen.x
            && frame.y <= screen.y
            && frame.x + frame.w >= screen.x + screen.w
            && frame.y + frame.h >= screen.y + screen.h;
        let compact = frame.w <= screen.w / 2 && frame.h <= screen.h / 2;
        // Browsers can drop WS_THICKFRAME on F11. DWM may still report the
        // old resize border briefly after the outer window reaches the screen.
        let fullscreen =
            (covers_monitor && (borderless || work != screen)) || (borderless && outer == screen);
        // Our own tile may have made an ordinary topmost window compact.
        let floating_panel = extended & WS_EX_TOPMOST != 0 && (borderless || (compact && !tracked));
        // With no reserved screen edge, even a normal maximized custom frame
        // can cover the monitor. Its native maximize button still toggles it back.
        let fullscreen_only =
            borderless && fullscreen && (work != screen || style & WS_MAXIMIZEBOX == 0);
        let normal_frame = match usage {
            WindowUse::Tiling => resizable,
            WindowUse::Maximize => resizable || !borderless,
            // Closing and moving to another desktop also work for games
            // that use a fixed-size borderless window.
            WindowUse::CloseOrTransfer => true,
        };
        let state = if normal_frame && IsZoomed(hwnd) != 0 && !floating_panel && !fullscreen_only {
            TileState::Maximized
        } else if fullscreen {
            TileState::Fullscreen
        } else if normal_frame && !floating_panel {
            TileState::Eligible
        } else {
            return TileState::Excluded;
        };
        if ordinary_process(pid(id)) {
            state
        } else {
            TileState::Excluded
        }
    }
}

pub fn eligible(id: Id) -> bool {
    tile_state(id) == TileState::Eligible
}

fn marker_name() -> *const u16 {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    process_marker_name(&NAME, "WindowIdentity")
}

fn process_marker_name(storage: &'static OnceLock<Vec<u16>>, purpose: &str) -> *const u16 {
    storage
        .get_or_init(|| {
            format!("Rustile.{purpose}.{}\0", std::process::id())
                .encode_utf16()
                .collect()
        })
        .as_ptr()
}

// Properties die with the HWND, even when its process recreates a window with
// the same numeric handle. Tile, transfer and native command markers each
// have independent lifetimes.
pub(crate) struct WindowMarker {
    id: Id,
    owner: u32,
    name: *const u16,
    value: usize,
}

impl WindowMarker {
    pub(crate) fn for_window_control(id: Id, control: WindowControl) -> Option<Self> {
        static NAME: OnceLock<Vec<u16>> = OnceLock::new();
        let allowed = match control {
            WindowControl::Close => can_close_or_transfer(id),
            WindowControl::ToggleMaximize => {
                window_enabled(id)
                    && matches!(
                        classify_window(id, WindowUse::Maximize),
                        TileState::Eligible | TileState::Maximized
                    )
            }
        };
        if !allowed {
            return None;
        }
        Self::attach(id, process_marker_name(&NAME, "WindowControl"))
    }

    fn attach(id: Id, name: *const u16) -> Option<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        let owner = pid(id);
        let value = NEXT.fetch_add(1, Ordering::Relaxed);
        if owner == 0 || value == 0 || unsafe { SetPropW(id as HWND, name, value as _) } == 0 {
            return None;
        }
        let marker = Self {
            id,
            owner,
            name,
            value,
        };
        marker.matches().then_some(marker)
    }

    pub(crate) fn matches(&self) -> bool {
        pid(self.id) == self.owner
            && unsafe { GetPropW(self.id as HWND, self.name) as usize == self.value }
    }
}

impl Drop for WindowMarker {
    fn drop(&mut self) {
        if self.matches() {
            unsafe { RemovePropW(self.id as HWND, self.name) };
        }
    }
}

pub(crate) struct TransferIdentity(WindowMarker);

impl TransferIdentity {
    pub(crate) fn capture(id: Id) -> Option<Self> {
        static NAME: OnceLock<Vec<u16>> = OnceLock::new();
        if !can_close_or_transfer(id) {
            return None;
        }
        WindowMarker::attach(id, process_marker_name(&NAME, "TransferIdentity")).map(Self)
    }

    pub(crate) fn id(&self) -> Id {
        self.0.id
    }

    pub(crate) fn matches(&self) -> bool {
        self.0.matches()
    }
}

pub struct Original {
    id: Id,
    marker: WindowMarker,
    outer: Rect,
    insets: FrameInsets,
}

#[derive(Default)]
struct FrameInsets {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl FrameInsets {
    fn measure(hwnd: HWND, outer: Rect) -> Option<Self> {
        let frame = measured_frame(hwnd, outer)?;
        (outer_bounds(hwnd)? == outer).then_some(Self {
            left: frame.x - outer.x,
            top: frame.y - outer.y,
            right: outer.x + outer.w - frame.x - frame.w,
            bottom: outer.y + outer.h - frame.y - frame.h,
        })
    }

    fn outer_for(&self, frame: Rect) -> Rect {
        Rect {
            x: frame.x - self.left,
            y: frame.y - self.top,
            w: frame.w + self.left + self.right,
            h: frame.h + self.top + self.bottom,
        }
    }

    fn frame_for(&self, outer: Rect) -> Rect {
        Rect {
            x: outer.x + self.left,
            y: outer.y + self.top,
            w: outer.w - self.left - self.right,
            h: outer.h - self.top - self.bottom,
        }
    }
}

fn floating_frame(frame: Rect, work: Rect, minimum: (i32, i32)) -> Rect {
    let fit = |start: i32, length: i32, origin: i32, available: i32, min: i32| {
        let length = length.min(available.max(min));
        // If even the minimum overflows, keep the leading edge and title bar reachable.
        let last = origin.saturating_add((available - length).max(0));
        (start.clamp(origin, last), length)
    };
    let (x, w) = fit(frame.x, frame.w, work.x, work.w, minimum.0);
    let (y, h) = fit(frame.y, frame.h, work.y, work.h, minimum.1);
    Rect { x, y, w, h }
}

impl Original {
    /// Read the live drag rectangle in the same visible-frame coordinates used by `place`.
    pub fn visible_frame(&self) -> Option<Rect> {
        // DWM can still report the preceding frame while the window's edge is moving.
        let outer = outer_bounds(self.id as HWND)?;
        if !self.matches(self.id) {
            return None;
        }
        let frame = self.insets.frame_for(outer);
        (frame.w > 0 && frame.h > 0).then_some(frame)
    }

    pub fn is_at(&self, target: Rect) -> bool {
        let hwnd = self.id as HWND;
        let Some(outer) = outer_bounds(hwnd) else {
            return false;
        };
        // Leaving fullscreen can change the invisible resize border. Only
        // accept a differing outer rectangle if DWM confirms the visible tile.
        outer == self.insets.outer_for(target)
            || (measured_frame(hwnd, outer) == Some(target) && outer_bounds(hwnd) == Some(outer))
    }

    pub fn matches(&self, id: Id) -> bool {
        id == self.id && self.marker.matches()
    }

    // Sample at admission or after a user move. During a burst of async
    // placements DWM may still be reporting an earlier size for this HWND.
    pub fn refresh_frame(&mut self) {
        let hwnd = self.id as HWND;
        if let Some(outer) = outer_bounds(hwnd)
            && let Some(insets) = FrameInsets::measure(hwnd, outer)
        {
            self.insets = insets;
        }
    }

    // Queried on admission or when a changed work area requires a refit.
    pub fn minimum_frame_size(&self) -> (i32, i32) {
        self.minimum_for(&self.insets)
    }

    fn minimum_for(&self, insets: &FrameInsets) -> (i32, i32) {
        let hwnd = self.id as HWND;
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let primary_dpi = system_dpi();
        let metrics = |x, y| POINT {
            x: unsafe { GetSystemMetricsForDpi(x, dpi) },
            y: unsafe { GetSystemMetricsForDpi(y, dpi) },
        };
        let system_min = metrics(SM_CXMINTRACK, SM_CYMINTRACK);
        let mut limits = MINMAXINFO {
            ptMinTrackSize: system_min,
            ptMaxTrackSize: metrics(SM_CXMAXTRACK, SM_CYMAXTRACK),
            ptMaxSize: metrics(SM_CXMAXIMIZED, SM_CYMAXIMIZED),
            ..Default::default()
        };
        let delivered = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_GETMINMAXINFO,
                0,
                (&raw mut limits) as isize,
                SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
                50,
                null_mut(),
            )
        };
        let track = if delivered == 0 {
            system_min
        } else {
            limits.ptMinTrackSize
        };
        // Native minimums describe the outer tracking rectangle in the target
        // window's coordinates. Our grid uses visible physical pixels.
        let physical = |length: i32| {
            let scaled = i64::from(length.max(0)) * i64::from(primary_dpi);
            ((scaled + i64::from(dpi) - 1) / i64::from(dpi)).min(i64::from(i32::MAX)) as i32
        };
        (
            (physical(track.x) - insets.left - insets.right).max(0),
            (physical(track.y) - insets.top - insets.bottom).max(0),
        )
    }

    pub fn fit_floating(&self, work: Rect) {
        if work.w <= 0 || work.h <= 0 || !self.matches(self.id) || !eligible(self.id) {
            return;
        }
        let hwnd = self.id as HWND;
        let Some(outer) = outer_bounds(hwnd) else {
            return;
        };
        let measured = FrameInsets::measure(hwnd, outer);
        let insets = measured.as_ref().unwrap_or(&self.insets);
        let frame = insets.frame_for(outer);
        if frame.w <= 0 || frame.h <= 0 {
            return;
        }
        let minimum = if frame.w > work.w || frame.h > work.h {
            self.minimum_for(insets)
        } else {
            (0, 0)
        };
        let target = floating_frame(frame, work, minimum);
        if frame != target {
            self.place_frame(insets.outer_for(target));
        }
    }

    pub fn place(&self, target: Rect) -> bool {
        if target.w <= 0 || target.h <= 0 {
            return false;
        }
        self.place_frame(self.insets.outer_for(target))
    }

    fn place_frame(&self, bounds: Rect) -> bool {
        if !self.matches(self.id) || !eligible(self.id) {
            return false;
        }
        let hwnd = self.id as HWND;
        if is_snapped(self.id) {
            return place_unsnapped(hwnd, bounds);
        }
        position(hwnd, bounds, 0)
    }
}

pub fn capture(id: Id) -> Option<Original> {
    if !eligible(id) {
        return None;
    }
    let outer = outer_bounds(id as HWND)?;
    let original = Original {
        id,
        marker: WindowMarker::attach(id, marker_name())?,
        outer,
        insets: FrameInsets::measure(id as HWND, outer).unwrap_or_default(),
    };
    original.matches(id).then_some(original)
}

fn position(hwnd: HWND, bounds: Rect, flags: SET_WINDOW_POS_FLAGS) -> bool {
    unsafe {
        SetWindowPos(
            hwnd,
            null_mut(),
            bounds.x,
            bounds.y,
            bounds.w,
            bounds.h,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS | flags,
        ) != 0
    }
}

// SetWindowPos alone retains the shell's snapped state and its square DWM
// corners. Use non-activating placement to clear Snap before the final resize.
pub(crate) fn place_unsnapped(hwnd: HWND, bounds: Rect) -> bool {
    unsafe {
        let Some(monitor) = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL)) else {
            return false;
        };
        let mut placement = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        if GetWindowPlacement(hwnd, &mut placement) == 0 {
            return false;
        }
        // Top-level non-tool windows use workspace coordinates for rcNormalPosition.
        let left = bounds.x - (monitor.rcWork.left - monitor.rcMonitor.left);
        let top = bounds.y - (monitor.rcWork.top - monitor.rcMonitor.top);
        placement.rcNormalPosition = RECT {
            left,
            top,
            // Restoring at exactly the tile size can leave DWM displaying the
            // old square frame. A one-pixel size change after restore makes it
            // recompose the frame using the app's own corner preference.
            right: left + bounds.w + 1,
            bottom: top + bounds.h,
        };
        placement.flags = WPF_ASYNCWINDOWPLACEMENT;
        placement.showCmd = SW_SHOWNOACTIVATE as u32;
        if SetWindowPlacement(hwnd, &placement) == 0 {
            return false;
        }
        if position(hwnd, bounds, SWP_FRAMECHANGED) {
            return true;
        }
        placement.rcNormalPosition.right -= 1;
        SetWindowPlacement(hwnd, &placement) != 0
    }
}

pub fn restore(id: Id, original: &Original) {
    if eligible(id) && original.matches(id) {
        position(id as HWND, original.outer, 0);
    }
}

pub fn focus(id: Id) -> bool {
    eligible(id)
        && unsafe { IsWindowEnabled(id as HWND) } != 0
        && (foreground() == id || unsafe { SetForegroundWindow(id as HWND) } != 0)
}

pub(crate) fn focus_after_desktop_change(
    id: Id,
    previous: Id,
    may_activate: impl Fn() -> bool,
) -> bool {
    let hwnd = id as HWND;
    let can_activate = || may_activate() && window_enabled(id) && foreground() == previous;
    // Explorer can activate its own application view during a desktop change.
    // Recheck after RPC: input or desktop membership may already have changed.
    let shell_accepted = if let Some(view) = desktops::shell::ApplicationView::lookup(hwnd) {
        if !can_activate() {
            return false;
        }
        view.request_focus()
    } else {
        false
    };
    if foreground() == id {
        return true;
    }
    // S_OK can precede the actual focus change. While the previous window
    // can still yield, also give Windows the regular foreground request.
    let accepted = can_activate() && unsafe { SetForegroundWindow(hwnd) } != 0;
    accepted || shell_accepted
}

#[cfg(test)]
mod work_area_tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn desktop_recovery_respects_taskbar_keyboard_focus_and_desktop_context_menus() {
        let mut gui = GUITHREADINFO::default();
        assert_eq!(desktop_foreground(0), DesktopForeground::Vacant);
        assert!(!gui_input_in_use(&gui));
        gui.flags = GUI_POPUPMENUMODE;
        assert!(gui_input_in_use(&gui));
        let create = |class, style, parent| unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                class,
                null(),
                style,
                0,
                0,
                0,
                0,
                parent,
                null_mut(),
                null_mut(),
                null(),
            )
        };
        // Message-only fixtures exercise native handle relationships without
        // creating a visible window or changing the user's actual focus.
        let dialog = create(windows_sys::w!("#32770"), 0, HWND_MESSAGE);
        assert!(!dialog.is_null());
        let button = create(windows_sys::w!("BUTTON"), WS_CHILD, dialog);
        assert!(!button.is_null());
        assert!(!taskbar_control_has_focus(dialog, null_mut()));
        assert!(!taskbar_control_has_focus(dialog, dialog));
        assert!(taskbar_control_has_focus(dialog, button));
        let state = desktop_foreground(dialog as Id);
        unsafe { DestroyWindow(dialog) };
        assert_eq!(
            state,
            DesktopForeground::Vacant,
            "a hidden dialog cannot keep the incoming desktop waiting"
        );
    }

    #[test]
    fn only_untracked_frames_wholly_inside_reserved_strips_are_excluded() {
        let screen = r(-1800, -240, 1600, 1000);
        let work = r(-1760, -210, 1540, 940);
        for bar in [
            r(-1800, -240, 1600, 30), // top
            r(-1800, 730, 1600, 30),  // bottom
            r(-1800, -240, 40, 1000), // left
            r(-220, -240, 20, 1000),  // right
        ] {
            assert!(untracked_in_reserved_strip(bar, screen, work, false));
            assert!(!untracked_in_reserved_strip(bar, screen, work, true));
        }
        for window in [
            work,
            r(-1740, -235, 400, 26), // overlaps the work area by one pixel
            r(-1880, -240, 80, 30),  // lies on the neighboring monitor
            r(-1810, -240, 40, 25),  // straddles the monitor edge
        ] {
            assert!(!untracked_in_reserved_strip(window, screen, work, false));
        }
    }

    #[test]
    fn floating_frame_moves_only_as_far_as_needed_and_respects_minimums() {
        let work = r(-960, -200, 800, 600);
        let inside = r(-810, -100, 320, 220);
        assert_eq!(floating_frame(inside, work, (250, 150)), inside);
        assert_eq!(
            floating_frame(r(-980, -230, 320, 220), work, (250, 150)),
            r(-960, -200, 320, 220)
        );
        assert_eq!(
            floating_frame(r(-420, 240, 320, 220), work, (250, 150)),
            r(-480, 180, 320, 220)
        );
        assert_eq!(
            floating_frame(r(-990, -30, 960, 280), work, (480, 180)),
            r(-960, -30, 800, 280)
        );
        assert_eq!(
            floating_frame(r(-990, -350, 1100, 900), work, (880, 730)),
            r(-960, -200, 880, 730)
        );
    }

    #[test]
    fn work_area_requires_positive_extents_and_spacing_uses_the_supplied_dpi() {
        let raw = RECT {
            left: -200,
            top: -300,
            right: 0,
            bottom: 0,
        };
        assert_eq!(usable_work_area(raw), Some(r(-200, -300, 200, 300)));
        assert!(usable_work_area(RECT::default()).is_none());
        assert!(usable_work_area(RECT { right: -200, ..raw }).is_none());
        assert!(
            usable_work_area(RECT {
                bottom: -301,
                ..raw
            })
            .is_none()
        );
        assert_eq!(scaled_spacing(0, 144), 0);
        assert_eq!(scaled_spacing(8, 120), 10);
        assert_eq!(scaled_spacing(3, 144), 5);
    }
}
