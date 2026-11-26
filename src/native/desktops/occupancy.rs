use std::cell::OnceCell;
use std::collections::HashSet;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWM_CLOAKED_SHELL, DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOTOWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW, GetWindowLongW,
    IsIconic, IsWindow, IsWindowVisible, WS_CHILD, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_MINIMIZE,
};

use super::shell::ApplicationViews;
use super::{DesktopId, Service, shell_window};
use crate::layout::Id;
use crate::native::{Cloak, append_window, cloak_state, pid};

// GetWindowDesktopId returns these two Shell IDs for pins. A pin is present
// everywhere and cannot occupy a particular workspace.
const PINNED_WINDOW: DesktopId = DesktopId(0xc2ddea68_66f2_4cf9_8264_1bfd00fbbbac);
const PINNED_APP: DesktopId = DesktopId(0xbb64d5b7_4de3_4ab2_a87c_db7601aea7dc);

/// Snapshot all monitors and virtual desktops independently of tiling eligibility.
/// The caller owns the COM apartment. None means that emptiness is unproven.
pub(crate) fn occupied(desktops: &[DesktopId]) -> Option<HashSet<DesktopId>> {
    scan(desktops, |windows| windows.collect::<Result<_, _>>().ok())
}

/// A known window suffices for navigation; proving emptiness requires every observation.
pub(crate) fn any_occupied(desktops: &[DesktopId], candidates: &[DesktopId]) -> Option<bool> {
    scan(desktops, |windows| any_match(windows, candidates))
}

fn any_match(
    windows: impl Iterator<Item = Result<DesktopId, ()>>,
    candidates: &[DesktopId],
) -> Option<bool> {
    let mut complete = true;
    for desktop in windows {
        match desktop {
            Ok(id) if candidates.contains(&id) => return Some(true),
            Ok(_) => {}
            Err(()) => complete = false,
        }
    }
    complete.then_some(false)
}

fn scan<T>(
    desktops: &[DesktopId],
    inspect: impl FnOnce(&mut dyn Iterator<Item = Result<DesktopId, ()>>) -> Option<T>,
) -> Option<T> {
    let service = Service::connect()?;
    if service.shell.is_null() {
        return None;
    }
    let mut windows: Vec<Id> = Vec::new();
    if unsafe { EnumWindows(Some(append_window), &raw mut windows as LPARAM) } == 0
        || windows.is_empty()
    {
        return None;
    }
    let views = OnceCell::new();
    let mut observations = windows
        .into_iter()
        .filter_map(|id| window_desktop(id as HWND, &service, &views, desktops).transpose());
    let result = inspect(&mut observations)?;
    (shell_window() == service.shell).then_some(result)
}

fn window_desktop(
    hwnd: HWND,
    service: &Service,
    views: &OnceCell<Option<ApplicationViews>>,
    desktops: &[DesktopId],
) -> Result<Option<DesktopId>, ()> {
    if !task_window(hwnd).ok_or(())? {
        return Ok(None);
    }
    let mut desktop = service.desktop_id(hwnd).ok();
    if desktop.is_none_or(|d| d.0 == 0) {
        // A modal window may belong to the shell view of a hidden owner.
        let owner = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
        if !owner.is_null() && owner != hwnd {
            desktop = match service.desktop_id(owner) {
                Ok(id) if id.0 != 0 => Some(id),
                Ok(_) => desktop,
                Err(_) => None,
            };
        }
    }
    if unsafe { IsWindow(hwnd) } == 0 {
        return Ok(None);
    }
    assigned_desktop(desktops, desktop.ok_or(())?, || {
        shell_cloaked_packaged_window(hwnd)
            && views
                .get_or_init(ApplicationViews::connect)
                .as_ref()
                .is_some_and(|views| views.is_hidden_unassigned_view(hwnd))
    })
}

fn class_name(hwnd: HWND) -> Option<String> {
    let mut name = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
    (length > 0).then(|| String::from_utf16_lossy(&name[..length as usize]))
}

fn shell_cloaked_packaged_window(hwnd: HWND) -> bool {
    // A suspended packaged app can retain its CoreWindow and ApplicationFrame
    // after the last task has closed. A failed ID lookup, a minimized window,
    // or a real task on another desktop must still protect its workspace.
    let Some(class) = class_name(hwnd) else {
        return false;
    };
    let mut cloak = 0u32;
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&raw mut cloak).cast(),
            size_of_val(&cloak) as u32,
        )
    };
    hr == 0 && parked_frame_candidate(&class, unsafe { IsIconic(hwnd) } != 0, cloak)
}

fn parked_frame_candidate(class: &str, minimized: bool, cloak: u32) -> bool {
    !minimized
        && cloak == DWM_CLOAKED_SHELL
        && matches!(
            class,
            "Windows.UI.Core.CoreWindow" | "ApplicationFrameWindow"
        )
}

fn assigned_desktop(
    desktops: &[DesktopId],
    id: DesktopId,
    parked: impl FnOnce() -> bool,
) -> Result<Option<DesktopId>, ()> {
    if matches!(id, PINNED_APP | PINNED_WINDOW) {
        return Ok(None);
    }
    if id.0 == 0 {
        return parked().then_some(None).ok_or(());
    }
    // A GUID outside this snapshot can indicate a concurrent desktop change.
    desktops.contains(&id).then_some(Some(id)).ok_or(())
}

fn task_window(hwnd: HWND) -> Option<bool> {
    // Minimized task windows retain WS_VISIBLE, including on another desktop.
    // A tray-only app explicitly hides its top-level window.
    if unsafe { IsWindowVisible(hwnd) } == 0 || pid(hwnd as Id) == std::process::id() {
        return Some(false);
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    let extended = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if style == 0 {
        return (unsafe { IsWindow(hwnd) } == 0).then_some(false);
    }
    let Some(class) = class_name(hwnd) else {
        return (unsafe { IsWindow(hwnd) } == 0).then_some(false);
    };
    if !task_style(style, extended, &class)
        || (style & WS_MINIMIZE == 0 && cloak_state(hwnd as Id) == Cloak::Application)
    {
        return Some(false);
    }
    // Packaged apps and always-present Shell input/search surfaces use
    // the same class. Leave unknown processes eligible for a desktop query.
    if class == "Windows.UI.Core.CoreWindow" && core_window_shell_host(pid(hwnd as Id)) {
        return Some(false);
    }
    Some(true)
}

fn task_style(style: u32, extended: u32, class: &str) -> bool {
    if style & WS_CHILD != 0 || class.starts_with("Rustile.") {
        return false;
    }
    if extended & WS_EX_APPWINDOW == 0 && extended & WS_EX_NOACTIVATE != 0 {
        return false;
    }
    !matches!(
        class,
        "#32768"
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
            | "Windows.Internal.Shell.TabProxyWindow"
    )
}

fn core_window_shell_host(process_id: u32) -> bool {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return false;
    }
    let mut path = [0u16; 1024];
    let mut len = path.len() as u32;
    let success = unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) };
    unsafe { CloseHandle(process) };
    if success == 0 {
        return false;
    }
    let path = String::from_utf16_lossy(&path[..len as usize]);
    let image = path.rsplit('\\').next().unwrap_or_default();
    [
        "TextInputHost.exe",
        "SearchHost.exe",
        "SearchApp.exe",
        "StartMenuExperienceHost.exe",
        "ShellExperienceHost.exe",
        "ShellHost.exe",
        "LockApp.exe",
    ]
    .iter()
    .any(|shell| image.eq_ignore_ascii_case(shell))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE};

    #[test]
    fn minimized_dialogs_and_fullscreen_or_floating_apps_count() {
        assert!(task_style(
            WS_VISIBLE | WS_MINIMIZE,
            0,
            "ApplicationFrameWindow"
        ));
        assert!(task_style(WS_VISIBLE | WS_POPUP, 0, "FullscreenGame"));
        assert!(task_style(WS_VISIBLE, WS_EX_TOOLWINDOW, "#32770"));
        assert!(task_style(WS_POPUP, WS_EX_TOOLWINDOW, "IndependentPalette"));
    }

    #[test]
    fn panels_and_shell_helpers_do_not_fill_the_spare() {
        assert!(!task_style(
            WS_VISIBLE,
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            "Yasb"
        ));
        assert!(!task_style(WS_VISIBLE, 0, "Progman"));
        assert!(!task_style(WS_VISIBLE, 0, "Rustile.ResizeHint"));
        assert!(task_style(
            WS_VISIBLE,
            WS_EX_APPWINDOW | WS_EX_NOACTIVATE,
            "App"
        ));
    }

    #[test]
    fn pinned_windows_are_global_and_unknown_ids_invalidate_the_snapshot() {
        let desktops = [DesktopId(1), DesktopId(2)];
        for id in [PINNED_WINDOW, PINNED_APP] {
            assert_eq!(assigned_desktop(&desktops, id, || unreachable!()), Ok(None));
        }
        assert_eq!(
            assigned_desktop(&desktops, DesktopId(3), || unreachable!()),
            Err(())
        );
        assert_eq!(
            assigned_desktop(&desktops, desktops[1], || unreachable!()),
            Ok(Some(desktops[1]))
        );
        assert_eq!(assigned_desktop(&desktops, DesktopId(0), || false), Err(()));
        assert_eq!(assigned_desktop(&desktops, DesktopId(0), || true), Ok(None));
    }

    #[test]
    fn positive_navigation_evidence_survives_an_unrelated_unresolved_view() {
        let target = DesktopId(1);
        let other = DesktopId(2);
        assert_eq!(
            any_match([Err(()), Ok(target)].into_iter(), &[target]),
            Some(true)
        );
        assert_eq!(any_match([Err(()), Ok(other)].into_iter(), &[target]), None);
        assert_eq!(any_match([Ok(other)].into_iter(), &[target]), Some(false));
    }

    #[test]
    fn only_shell_cloaked_unminimized_packaged_frames_need_the_extra_view_check() {
        for class in ["Windows.UI.Core.CoreWindow", "ApplicationFrameWindow"] {
            assert!(parked_frame_candidate(class, false, DWM_CLOAKED_SHELL));
            assert!(!parked_frame_candidate(class, true, DWM_CLOAKED_SHELL));
            assert!(!parked_frame_candidate(class, false, 0));
        }
        assert!(!parked_frame_candidate(
            "CabinetWClass",
            false,
            DWM_CLOAKED_SHELL
        ));
    }
}
