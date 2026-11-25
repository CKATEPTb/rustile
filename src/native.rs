pub(crate) mod access;
pub(crate) mod desktops;

use crate::layout::{Id, Rect};
use std::mem::size_of;
use std::ptr::null_mut;
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

fn shell_or_dialog(hwnd: HWND) -> bool {
    let mut class = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    if len <= 0 {
        return true;
    }
    matches!(
        String::from_utf16_lossy(&class[..len as usize]).as_str(),
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

pub(crate) fn cloak_state(id: Id) -> Cloak {
    let mut cloak = 0u32;
    let result = unsafe {
        DwmGetWindowAttribute(
            id as HWND,
            DWMWA_CLOAKED as u32,
            (&mut cloak as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    match (result >= 0, cloak) {
        (true, 0) => Cloak::None,
        (true, DWM_CLOAKED_APP) => Cloak::Application,
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

pub fn close_window(id: Id) {
    // The owning UI thread handles save prompts and may cancel or hide to tray.
    unsafe { PostMessageW(id as HWND, WM_CLOSE, 0, 0) };
}

pub fn set_maximized(id: Id, maximize: bool) {
    let command = if maximize { SC_MAXIMIZE } else { SC_RESTORE };
    unsafe { PostMessageW(id as HWND, WM_SYSCOMMAND, command as usize, 0) };
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

pub(crate) fn tile_state(id: Id) -> TileState {
    let hwnd = id as HWND;
    if hwnd.is_null() {
        return TileState::Excluded;
    }
    unsafe {
        if IsWindowVisible(hwnd) == 0
            || IsIconic(hwnd) != 0
            || GetAncestor(hwnd, GA_ROOT) != hwnd
            || !GetWindow(hwnd, GW_OWNER).is_null()
            || hwnd == GetShellWindow()
            || IsHungAppWindow(hwnd) != 0
        {
            return TileState::Excluded;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let extended = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let resizable = style & WS_THICKFRAME != 0;
        let borderless = style & WS_CAPTION != WS_CAPTION;
        if style & WS_CHILD != 0
            || (!resizable && !borderless)
            || extended & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_DLGMODALFRAME) != 0
            || cloaked(id)
            || shell_or_dialog(hwnd)
        {
            return TileState::Excluded;
        }
        let Some(info) = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL)) else {
            return TileState::Excluded;
        };
        if info.dwFlags & MONITORINFOF_PRIMARY == 0 {
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
        let state = if resizable && IsZoomed(hwnd) != 0 && !floating_panel && !fullscreen_only {
            TileState::Maximized
        } else if fullscreen {
            TileState::Fullscreen
        } else if resizable && !floating_panel {
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
    NAME.get_or_init(|| {
        format!("Rustile.WindowIdentity.{}\0", std::process::id())
            .encode_utf16()
            .collect()
    })
    .as_ptr()
}

pub struct Original {
    id: Id,
    owner: u32,
    marker: usize,
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
        id == self.id
            && pid(id) == self.owner
            && unsafe { GetPropW(id as HWND, marker_name()) as usize == self.marker }
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

impl Drop for Original {
    fn drop(&mut self) {
        if self.matches(self.id) {
            unsafe { RemovePropW(self.id as HWND, marker_name()) };
        }
    }
}

pub fn capture(id: Id) -> Option<Original> {
    static NEXT_MARKER: AtomicUsize = AtomicUsize::new(1);
    if !eligible(id) {
        return None;
    }
    let owner = pid(id);
    let outer = outer_bounds(id as HWND)?;
    let marker = NEXT_MARKER.fetch_add(1, Ordering::Relaxed);
    // A window property dies with its HWND, even if the numerical handle is
    // subsequently reused by a second window of the same process.
    if marker == 0 || unsafe { SetPropW(id as HWND, marker_name(), marker as _) } == 0 {
        return None;
    }
    let original = Original {
        id,
        owner,
        marker,
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

#[cfg(test)]
mod work_area_tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
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
