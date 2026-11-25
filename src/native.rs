use crate::layout::{Id, Rect};
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DEFAULT,
    DWMWCP_DONOTROUND, DWMWCP_ROUNDSMALL, DwmGetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, GetWindowRgnBox, HMONITOR, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, MonitorFromWindow, SIMPLEREGION,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
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
    monitor_info(monitor).map(|info| rect(info.rcWork))
}

// The prototype tiles the primary monitor, whose DPI is the system DPI.
pub fn spacing_px(dip: u8) -> i32 {
    let dpi = unsafe { GetDpiForSystem() }.max(96) as i32;
    (i32::from(dip) * dpi + 48) / 96
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
        let mut token = null_mut();
        let mut path = [0u16; 1024];
        let mut len = path.len() as u32;
        let has_image = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) != 0;
        let has_token = OpenProcessToken(process, TOKEN_QUERY, &mut token) != 0;
        CloseHandle(process);
        if !has_token {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut bytes = 0;
        let queried = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut bytes,
        );
        CloseHandle(token);
        if queried == 0 || elevation.TokenIsElevated != 0 || !has_image {
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

fn uncloaked(hwnd: HWND) -> bool {
    let mut cloak = 0u32;
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloak as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    result >= 0 && cloak == 0
}

/// Visible frame and work area for an already-managed tile on the primary display.
pub fn outline_frame(id: Id) -> Option<(Rect, Rect)> {
    let hwnd = id as HWND;
    unsafe {
        if IsWindowVisible(hwnd) == 0
            || IsIconic(hwnd) != 0
            || IsZoomed(hwnd) != 0
            || IsWindowEnabled(hwnd) == 0
            || !uncloaked(hwnd)
        {
            return None;
        }
        let info = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL))?;
        if info.dwFlags & MONITORINFOF_PRIMARY == 0 {
            return None;
        }
        Some((frame_bounds(hwnd, outer_bounds(hwnd)?), rect(info.rcWork)))
    }
}

pub fn eligible(id: Id) -> bool {
    let hwnd = id as HWND;
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        if IsWindowVisible(hwnd) == 0
            || IsIconic(hwnd) != 0
            || IsZoomed(hwnd) != 0
            || GetAncestor(hwnd, GA_ROOT) != hwnd
            || !GetWindow(hwnd, GW_OWNER).is_null()
            || hwnd == GetShellWindow()
            || IsHungAppWindow(hwnd) != 0
        {
            return false;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let extended = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_CHILD != 0
            || style & WS_THICKFRAME == 0
            || extended & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_DLGMODALFRAME) != 0
            || !uncloaked(hwnd)
            || shell_or_dialog(hwnd)
        {
            return false;
        }
        let Some(info) = monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL)) else {
            return false;
        };
        if info.dwFlags & MONITORINFOF_PRIMARY == 0 {
            return false;
        }
        let Some(outer) = outer_bounds(hwnd) else {
            return false;
        };
        let frame = frame_bounds(hwnd, outer);
        let screen = rect(info.rcMonitor);
        let covers_monitor = frame.x <= screen.x
            && frame.y <= screen.y
            && frame.x + frame.w >= screen.x + screen.w
            && frame.y + frame.h >= screen.y + screen.h;
        let compact = frame.w <= screen.w / 2 && frame.h <= screen.h / 2;
        let borderless = style & WS_CAPTION != WS_CAPTION;
        let fullscreen = covers_monitor && (borderless || rect(info.rcWork) != screen);
        // Our own tile may have made an ordinary topmost window compact.
        let floating_panel = extended & WS_EX_TOPMOST != 0
            && (borderless || (compact && GetPropW(hwnd, marker_name()).is_null()));
        if fullscreen || floating_panel {
            return false;
        }
        ordinary_process(pid(id))
    }
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
}

impl Original {
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

    // Called only when an HWND enters the grid, never from the keyboard hook.
    pub fn minimum_frame_size(&self) -> (i32, i32) {
        let hwnd = self.id as HWND;
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let primary_dpi = unsafe { GetDpiForSystem() }.max(96);
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
            (physical(track.x) - self.insets.left - self.insets.right).max(0),
            (physical(track.y) - self.insets.top - self.insets.bottom).max(0),
        )
    }

    pub fn place(&self, target: Rect) -> bool {
        if target.w <= 0 || target.h <= 0 || !self.matches(self.id) || !eligible(self.id) {
            return false;
        }
        let hwnd = self.id as HWND;
        let bounds = self.insets.outer_for(target);
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
