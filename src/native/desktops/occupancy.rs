use std::collections::HashSet;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOTOWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW, GetWindowLongW,
    IsWindow, IsWindowVisible, WS_CHILD, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_MINIMIZE,
};

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
    let mut occupied = HashSet::new();
    for id in windows {
        let hwnd = id as HWND;
        if !task_window(hwnd)? {
            continue;
        }
        let mut desktop = service.desktop_id(hwnd).ok().filter(|d| d.0 != 0);
        if desktop.is_none() {
            // A modal window may belong to the shell view of a hidden owner.
            let owner = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
            if !owner.is_null() && owner != hwnd {
                desktop = service.desktop_id(owner).ok();
            }
        }
        if unsafe { IsWindow(hwnd) } != 0 {
            record(&mut occupied, desktops, desktop?)?;
        }
    }
    (shell_window() == service.shell).then_some(occupied)
}

fn record(occupied: &mut HashSet<DesktopId>, desktops: &[DesktopId], id: DesktopId) -> Option<()> {
    if matches!(id, PINNED_APP | PINNED_WINDOW) {
        return Some(());
    }
    // A GUID outside this snapshot can indicate a concurrent desktop change.
    if id.0 == 0 || !desktops.contains(&id) {
        return None;
    }
    occupied.insert(id);
    Some(())
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
    let mut class = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    if length <= 0 {
        return (unsafe { IsWindow(hwnd) } == 0).then_some(false);
    }
    let class = String::from_utf16_lossy(&class[..length as usize]);
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
        let mut occupied = HashSet::new();
        for id in [PINNED_WINDOW, PINNED_APP] {
            assert_eq!(record(&mut occupied, &desktops, id), Some(()));
        }
        assert!(occupied.is_empty());
        for id in [DesktopId(0), DesktopId(3)] {
            assert_eq!(record(&mut occupied, &desktops, id), None);
        }
        assert_eq!(record(&mut occupied, &desktops, desktops[1]), Some(()));
        assert_eq!(occupied, HashSet::from([desktops[1]]));
    }
}
