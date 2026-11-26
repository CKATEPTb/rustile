use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance, IServiceProvider};
use windows::Win32::UI::Shell::Common::IObjectArray;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};
use windows_sys::Win32::Foundation::{
    E_UNEXPECTED, ERROR_SUCCESS, HWND, RPC_E_CHANGED_MODE, TYPE_E_ELEMENTNOTFOUND,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
};
use windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::core::w;

use super::{DesktopId, Service, object_name, shell_window};
use crate::layout::Id;

const IMMERSIVE_SHELL: GUID = GUID::from_u128(0xc2f03a33_21f5_47fa_b4bb_156362a2f239);
const DESKTOP_SERVICE: GUID = GUID::from_u128(0xc5e0cdca_7b6e_41b2_9fc4_d93975cc467b);

fn shell_provider() -> Option<IServiceProvider> {
    unsafe { CoCreateInstance(&IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER).ok() }
}

// Explorer's private ABI for 26100.2605+ and 26200, as defined by
// VirtualDesktopAccessor (winvd 0.0.49, src/interfaces.rs). Leave unused
// vtable slots in order. Never dispatch this layout on an unverified build.
#[repr(transparent)]
#[derive(Clone)]
struct ShellDesktop(IUnknown);

unsafe impl Interface for ShellDesktop {
    type Vtable = DesktopVtable;
    const IID: GUID = GUID::from_u128(0x3f07f4be_b107_441a_af0f_39d82529072c);
}

type Out<T> = unsafe extern "system" fn(*mut c_void, *mut Option<T>) -> HRESULT;

// IApplicationViewCollection and IApplicationView from the same gated Shell ABI.
#[repr(transparent)]
#[derive(Clone)]
pub(super) struct ApplicationViews(IUnknown);

unsafe impl Interface for ApplicationViews {
    type Vtable = ViewsVtable;
    const IID: GUID = GUID::from_u128(0x1841c6d7_4f9d_42c0_af41_8747538f10e5);
}

#[repr(C)]
pub(super) struct ViewsVtable {
    base: IUnknown_Vtbl,
    enumerate_views: [usize; 3],
    for_window:
        unsafe extern "system" fn(*mut c_void, HWND, *mut Option<ApplicationView>) -> HRESULT,
}

#[repr(transparent)]
#[derive(Clone)]
pub(in crate::native) struct ApplicationView(IUnknown);

unsafe impl Interface for ApplicationView {
    type Vtable = ViewVtable;
    const IID: GUID = GUID::from_u128(0x372e1d3b_38d3_42e4_a15b_8ab2b178f513);
}

#[repr(C)]
pub(in crate::native) struct ViewVtable {
    base: IUnknown_Vtbl,
    inspectable: [usize; 3],
    set_focus: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    switch_to_through_get_monitor: [usize; 4],
    visibility: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    set_cloak_through_set_activation_timestamp: [usize; 13],
    desktop_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
    set_desktop_id: usize,
    show_in_switchers: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
}

impl ApplicationView {
    pub(in crate::native) fn lookup(hwnd: HWND) -> Option<Self> {
        ApplicationViews::connect()?.for_window(hwnd).ok()
    }

    pub(in crate::native) fn request_focus(&self) -> bool {
        unsafe { (self.vtable().set_focus)(self.as_raw()) }.0 == 0
    }
}

impl ApplicationViews {
    pub(super) fn connect() -> Option<Self> {
        if !DesktopShell::supported() {
            return None;
        }
        unsafe { shell_provider()?.QueryService(&Self::IID).ok() }
    }

    fn for_window(&self, hwnd: HWND) -> Result<ApplicationView, HRESULT> {
        let mut view = None;
        let hr = unsafe { (self.vtable().for_window)(self.as_raw(), hwnd, &mut view) };
        if hr.0 != 0 {
            return Err(hr);
        }
        view.ok_or(HRESULT(E_UNEXPECTED))
    }

    pub(super) fn confirms_no_view(&self, hwnd: HWND) -> bool {
        matches!(self.for_window(hwnd), Err(hr) if hr.0 == TYPE_E_ELEMENTNOTFOUND)
    }

    pub(super) fn is_hidden_unassigned_view(&self, hwnd: HWND) -> bool {
        let Ok(view) = self.for_window(hwnd) else {
            return false;
        };
        let mut visible = 1;
        let mut switchable = 1;
        let mut id = GUID::zeroed();
        let table = view.vtable();
        let this = view.as_raw();
        unsafe {
            (table.visibility)(this, &mut visible).0 == 0
                && visible == 0
                && (table.show_in_switchers)(this, &mut switchable).0 == 0
                && switchable == 0
                && (table.desktop_id)(this, &mut id).0 == 0
                && id.to_u128() == 0
        }
    }
}

#[repr(C)]
struct DesktopVtable {
    base: IUnknown_Vtbl,
    is_view_visible: usize,
    get_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
}

impl ShellDesktop {
    fn id(&self) -> Option<DesktopId> {
        let mut id = GUID::zeroed();
        unsafe { (self.vtable().get_id)(self.as_raw(), &mut id) }
            .ok()
            .ok()?;
        (id.to_u128() != 0).then_some(DesktopId(id.to_u128()))
    }
}

#[repr(transparent)]
#[derive(Clone)]
struct ShellManager(IUnknown);

unsafe impl Interface for ShellManager {
    type Vtable = ManagerVtable;
    const IID: GUID = GUID::from_u128(0x53f5ca0b_158f_4124_900c_057158060b27);
}

#[repr(C)]
struct ManagerVtable {
    base: IUnknown_Vtbl,
    get_count: usize,
    move_view: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    can_move_view: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i32) -> HRESULT,
    current: Out<ShellDesktop>,
    list: Out<IObjectArray>,
    adjacent: usize,
    switch: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    switch_and_move_foreground: usize,
    create: Out<ShellDesktop>,
    move_desktop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> HRESULT,
    remove: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    find: unsafe extern "system" fn(*mut c_void, *const GUID, *mut Option<ShellDesktop>) -> HRESULT,
}

impl ShellManager {
    fn connect() -> Option<Self> {
        unsafe { shell_provider()?.QueryService(&DESKTOP_SERVICE).ok() }
    }

    fn object<T>(&self, method: Out<T>) -> Option<T> {
        let mut result = None;
        unsafe { method(self.as_raw(), &mut result) }.ok().ok()?;
        result
    }

    fn find(&self, id: DesktopId) -> Option<ShellDesktop> {
        let mut desktop = None;
        unsafe { (self.vtable().find)(self.as_raw(), &GUID::from_u128(id.0), &mut desktop) }
            .ok()
            .ok()?;
        desktop
    }

    fn current(&self) -> Option<DesktopId> {
        self.object(self.vtable().current)?.id()
    }

    fn move_desktop(&self, desktop: &ShellDesktop, index: u32) -> Option<()> {
        unsafe { (self.vtable().move_desktop)(self.as_raw(), desktop.as_raw(), index) }
            .ok()
            .ok()
    }
}

struct Apartment(bool);

impl Apartment {
    fn enter() -> Option<Self> {
        let hr = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        (hr >= 0 || hr == RPC_E_CHANGED_MODE).then_some(Self(hr >= 0))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn supported_abi(build: u32, revision: u32) -> bool {
    (build == 26100 && revision >= 2605) || build == 26200
}

fn supports_installed_shell() -> bool {
    let version = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let mut build = [0u16; 16];
    let mut bytes = size_of_val(&build) as u32;
    let mut revision = 0u32;
    let mut revision_bytes = size_of_val(&revision) as u32;
    unsafe {
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            version,
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            null_mut(),
            build.as_mut_ptr().cast(),
            &mut bytes,
        ) != ERROR_SUCCESS
            || RegGetValueW(
                HKEY_LOCAL_MACHINE,
                version,
                w!("UBR"),
                RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
                null_mut(),
                (&raw mut revision).cast(),
                &mut revision_bytes,
            ) != ERROR_SUCCESS
        {
            return false;
        }
    }
    let build = &build[..bytes as usize / 2];
    build
        .strip_suffix(&[0])
        .and_then(|text| String::from_utf16(text).ok())
        .and_then(|build| build.parse().ok())
        .is_some_and(|build| supported_abi(build, revision))
}

pub(crate) struct DesktopShell {
    service: RefCell<Option<(HWND, ShellManager)>>,
    // Release the COM interfaces before leaving their apartment.
    _apartment: Apartment,
}

impl DesktopShell {
    pub(crate) fn supported() -> bool {
        let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        object_name(desktop).is_ok_and(|name| name.eq_ignore_ascii_case("Default"))
            && supports_installed_shell()
    }

    pub(crate) fn connect() -> Option<Self> {
        if !Self::supported() || shell_window().is_null() {
            return None;
        }
        let shell = Self {
            service: RefCell::default(),
            _apartment: Apartment::enter()?,
        };
        shell.current()?;
        Some(shell)
    }

    fn with_manager<T>(&self, call: impl FnOnce(&ShellManager) -> Option<T>) -> Option<T> {
        let tray = shell_window();
        let mut service = self.service.borrow_mut();
        if tray.is_null() {
            service.take();
            return None;
        }
        if service
            .as_ref()
            .is_none_or(|(old_tray, _)| *old_tray != tray)
        {
            service.take();
            *service = Some((tray, ShellManager::connect()?));
        }
        let result = call(&service.as_ref()?.1);
        if result.is_none() {
            // Reconnect on a subsequent observation. Never replay a Shell
            // mutation: an RPC failure can arrive after Explorer committed it.
            service.take();
        }
        result
    }

    pub(crate) fn list(&self) -> Option<Vec<DesktopId>> {
        self.with_manager(|manager| {
            let array = manager.object(manager.vtable().list)?;
            let count = unsafe { array.GetCount() }.ok()?;
            let mut ids = Vec::new();
            for index in 0..count {
                let desktop = unsafe { array.GetAt::<ShellDesktop>(index) }.ok()?;
                let id = desktop.id()?;
                if ids.contains(&id) {
                    return None;
                }
                ids.push(id);
            }
            (!ids.is_empty()).then_some(ids)
        })
    }

    pub(crate) fn current(&self) -> Option<DesktopId> {
        self.with_manager(ShellManager::current)
    }

    pub(crate) fn create(&self) -> Option<DesktopId> {
        self.with_manager(|manager| manager.object(manager.vtable().create)?.id())
    }

    pub(crate) fn move_to_end(&self, id: DesktopId) -> bool {
        self.with_manager(|manager| {
            let desktop = manager.find(id)?;
            let order = manager.object(manager.vtable().list)?;
            let last = unsafe { order.GetCount() }.ok()?.checked_sub(1)?;
            if unsafe { order.GetAt::<ShellDesktop>(last) }.ok()?.id()? == id {
                return Some(true);
            }
            // The user can select the spare after the occupancy scan.
            if manager.current()? == id {
                return Some(false);
            }
            manager.move_desktop(&desktop, last)?;
            Some(true)
        })
        .unwrap_or(false)
    }

    pub(crate) fn switch(&self, id: DesktopId) -> bool {
        self.with_manager(|manager| {
            let desktop = manager.find(id)?;
            unsafe { (manager.vtable().switch)(manager.as_raw(), desktop.as_raw()) }
                .ok()
                .ok()
        })
        .is_some()
    }

    pub(crate) fn move_window(
        &self,
        id: Id,
        from: DesktopId,
        to: DesktopId,
        validate: impl FnOnce() -> bool,
    ) -> bool {
        let hwnd = id as HWND;
        if from == to || hwnd.is_null() {
            return false;
        }
        self.with_manager(|manager| {
            // Keep the exact view and destination alive throughout the RPCs.
            let view = ApplicationView::lookup(hwnd)?;
            let destination = manager.find(to)?;
            let table = manager.vtable();
            let mut movable = 0;
            let check =
                unsafe { (table.can_move_view)(manager.as_raw(), view.as_raw(), &mut movable) };
            check.ok().ok()?;
            if movable == 0 || manager.current()? != from {
                return Some(false);
            }
            // The public API returns a special GUID for app and window pins.
            // A pinned HWND or a window already moved by Explorer has no match.
            // The caller validates the exact identity here, including foreground
            // ownership for hotkeys and implicit CLI targets.
            if Service::connect()?.desktop_id(hwnd).ok()? != from || !validate() {
                return Some(false);
            }
            let moved =
                unsafe { (table.move_view)(manager.as_raw(), view.as_raw(), destination.as_raw()) };
            Some(moved.0 == 0)
        })
        .unwrap_or(false)
    }

    pub(crate) fn remove(&self, id: DesktopId, fallback: DesktopId) -> bool {
        if id == fallback {
            return false;
        }
        self.with_manager(|manager| {
            let desktop = manager.find(id)?;
            let fallback = manager.find(fallback)?;
            if manager.current()? == id {
                return None;
            }
            unsafe {
                (manager.vtable().remove)(manager.as_raw(), desktop.as_raw(), fallback.as_raw())
            }
            .ok()
            .ok()
        })
        .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::super::occupancy::occupied;
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    #[test]
    fn application_view_entries_match_the_supported_abi() {
        let slot = size_of::<usize>();
        assert_eq!(std::mem::offset_of!(ViewVtable, set_focus), 6 * slot);
        assert_eq!(std::mem::offset_of!(ViewVtable, visibility), 11 * slot);
        assert_eq!(std::mem::offset_of!(ViewVtable, desktop_id), 25 * slot);
        assert_eq!(
            std::mem::offset_of!(ViewVtable, show_in_switchers),
            27 * slot
        );
    }

    #[test]
    fn move_view_slots_match_the_supported_shell_manager() {
        let slot = size_of::<usize>();
        assert_eq!(std::mem::offset_of!(ManagerVtable, move_view), 4 * slot);
        assert_eq!(std::mem::offset_of!(ManagerVtable, can_move_view), 5 * slot);
        assert_eq!(std::mem::offset_of!(ManagerVtable, current), 6 * slot);
    }

    fn move_for_test(shell: &DesktopShell, id: DesktopId, index: u32) -> bool {
        shell
            .with_manager(|manager| manager.move_desktop(&manager.find(id)?, index))
            .is_some()
    }

    #[test]
    fn private_abi_is_limited_to_known_builds() {
        assert!(!supported_abi(26099, 9000));
        assert!(!supported_abi(26100, 2604));
        assert!(supported_abi(26100, 2605));
        assert!(supported_abi(26200, 9974));
        assert!(!supported_abi(26201, 9000));
    }

    #[test]
    #[ignore = "read-only integration probe for a supported live Explorer session"]
    fn desktop_shell_inventory() {
        let shell = DesktopShell::connect().expect("supported Explorer on the interactive desktop");
        let ids = shell.list().expect("ordered desktop IDs");
        assert!(ids.contains(&shell.current().expect("current desktop ID")));
        super::super::occupancy::occupied(&ids).expect("complete occupancy inventory");
    }

    #[test]
    #[ignore = "read-only probe; needs a movable foreground window and a second desktop"]
    fn desktop_shell_foreground_transfer_preflight() {
        let shell = DesktopShell::connect().expect("supported interactive Explorer");
        let source = shell.current().expect("current desktop");
        let destination = shell
            .list()
            .expect("ordered desktop IDs")
            .into_iter()
            .find(|id| *id != source)
            .expect("another existing desktop");
        let foreground = unsafe { GetForegroundWindow() };
        let mut validated = false;
        assert!(
            !shell.move_window(foreground as Id, source, destination, || {
                validated = true;
                false
            })
        );
        assert!(
            validated,
            "foreground transfer preflight did not reach validation"
        );
        assert_eq!(
            Service::connect().unwrap().desktop_id(foreground).unwrap(),
            source
        );
    }

    #[test]
    #[ignore = "creates and removes its own background desktop on a supported Explorer session"]
    fn desktop_shell_background_round_trip() {
        let shell = DesktopShell::connect().expect("supported Explorer on the interactive desktop");
        let original = shell.current().expect("initial desktop");
        let before = shell.list().expect("initial desktop list");
        let created = shell.create().expect("new desktop ID");
        assert!(!before.contains(&created));
        let after = shell.list().expect("desktop list after creation");
        assert!(after.contains(&created));
        assert_eq!(
            shell.current(),
            Some(original),
            "creation unexpectedly switched desktops"
        );
        let occupied = occupied(&after).expect("complete occupancy inventory");
        assert!(
            !occupied.contains(&created),
            "new desktop acquired a window"
        );
        assert!(move_for_test(&shell, created, 0));
        assert_eq!(shell.list().expect("moved desktop").first(), Some(&created));
        assert!(shell.move_to_end(created));
        assert_eq!(shell.list().expect("original order restored"), after);
        assert_eq!(shell.current(), Some(original));
        assert!(shell.remove(created, original));
        assert!(
            !shell
                .list()
                .expect("desktop list after removal")
                .contains(&created)
        );
        assert_eq!(shell.current(), Some(original));
    }

    #[test]
    #[ignore = "requires running Rustile; temporarily reorders the verified empty background spare"]
    fn running_rustile_returns_displaced_spare_to_end() {
        use std::time::{Duration, Instant};
        use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;

        let tray = unsafe { FindWindowW(w!("Rustile.Tray"), w!("Rustile")) };
        assert!(!tray.is_null(), "start the updated Rustile first");
        let shell = DesktopShell::connect().expect("supported interactive Explorer");
        let before = shell.list().expect("initial order");
        let current = shell.current().expect("initial current desktop");
        let spare = *before.last().expect("trailing spare");
        let used = occupied(&before).expect("complete occupancy inventory");
        assert!(before.len() >= 2 && used.contains(&current));
        assert!(!used.contains(&spare));
        assert!(
            before[..before.len() - 1]
                .iter()
                .all(|id| used.contains(id))
        );
        assert_eq!(shell.list().as_ref(), Some(&before));
        assert_eq!(shell.current(), Some(current));
        let index = before.len() - 2;
        let mut displaced = before.clone();
        displaced.swap(index, index + 1);
        assert!(move_for_test(&shell, spare, u32::try_from(index).unwrap()));

        let deadline = Instant::now() + Duration::from_secs(8);
        let mut saw_displacement = false;
        loop {
            let order = shell.list();
            if shell.current() != Some(current) {
                break;
            }
            if order.as_ref() == Some(&before) && saw_displacement {
                return;
            }
            if order.as_ref() != Some(&displaced) || Instant::now() >= deadline {
                break;
            }
            saw_displacement = true;
            std::thread::sleep(Duration::from_millis(50));
        }
        // A stopped build may leave the test's reorder in place. Undo only
        // that exact reorder if nobody switched, opened a window or edited it.
        if shell.list().as_ref() == Some(&displaced)
            && shell.current() == Some(current)
            && occupied(&displaced).is_some_and(|ids| !ids.contains(&spare))
        {
            assert!(
                shell.move_to_end(spare),
                "failed to restore the verified spare"
            );
        }
        panic!("running Rustile did not restore the observed displaced spare");
    }
}
