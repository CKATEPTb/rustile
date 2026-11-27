//! Private Explorer COM interfaces for creating, ordering and switching desktops.
//! Match the Shell DLL's layout and query its interface IDs before calling private
//! methods. A failed mutation can reach Explorer, so observe the result before retrying.

mod image_version;
use image_version::installed_layout;

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::null;

use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, CoAllowSetForegroundWindow, CoCreateInstance, IServiceProvider,
};
use windows::Win32::UI::Shell::Common::IObjectArray;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};
use windows_sys::Win32::Foundation::{
    E_NOINTERFACE, E_UNEXPECTED, HWND, RPC_E_CHANGED_MODE, TYPE_E_ELEMENTNOTFOUND,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
#[cfg(test)]
use windows_sys::core::w;

use super::{DesktopId, Service, object_name, shell_window};
use crate::layout::Id;

const IMMERSIVE_SHELL: GUID = GUID::from_u128(0xc2f03a33_21f5_47fa_b4bb_156362a2f239);
const DESKTOP_SERVICE: GUID = GUID::from_u128(0xc5e0cdca_7b6e_41b2_9fc4_d93975cc467b);

fn shell_provider() -> Option<IServiceProvider> {
    unsafe { CoCreateInstance(&IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER).ok() }
}

// Leave unused entries in order to preserve Explorer's private vtable slots.
#[repr(transparent)]
#[derive(Clone)]
struct ShellDesktop(IUnknown);

// The manager returns its native desktop interface. Both published 23H2
// desktop IIDs and the 24H2 interface have the same prefix through GetId.
const DESKTOP_IID: u128 = 0x3f07f4be_b107_441a_af0f_39d82529072c;
const DESKTOP_23H2_IID: u128 = 0xa871910e_6cc0_4e65_8b9b_458ce9115e30;

#[repr(transparent)]
#[derive(Clone)]
struct IndexedDesktop<const ID: u128>(ShellDesktop);

unsafe impl<const ID: u128> Interface for IndexedDesktop<ID> {
    type Vtable = DesktopVtable;
    const IID: GUID = GUID::from_u128(ID);
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
        let layout = interactive_layout()?;
        let provider = shell_provider()?;
        if layout == ManagerLayout::Legacy {
            // Earlier updates in the same binary family used other interfaces.
            ShellManager::connect(&provider, layout)?;
        }
        unsafe { provider.QueryService(&Self::IID).ok() }
    }

    pub(super) fn for_window(&self, hwnd: HWND) -> Result<ApplicationView, HRESULT> {
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
        // Both accepted desktop interfaces return the ID in slot 4.
        let table = unsafe { self.0.assume_vtable::<IndexedDesktop<DESKTOP_IID>>() };
        unsafe { (table.get_id)(self.0.as_raw(), &mut id) }
            .ok()
            .ok()?;
        (id.to_u128() != 0).then_some(DesktopId(id.to_u128()))
    }
}

fn desktop_id_at(order: &IObjectArray, index: u32) -> Option<DesktopId> {
    let desktop = match unsafe { order.GetAt::<IndexedDesktop<DESKTOP_IID>>(index) } {
        Ok(desktop) => desktop.0,
        Err(error) if error.code().0 == E_NOINTERFACE => {
            unsafe { order.GetAt::<IndexedDesktop<DESKTOP_23H2_IID>>(index) }
                .ok()?
                .0
        }
        Err(_) => return None,
    };
    desktop.id()
}

// 22H2/23H2 used both manager IIDs with the same slots. 24H2 kept the
// second IID but inserted SwitchDesktopAndMoveForegroundView after SwitchDesktop.
// Compare VirtualDesktopAccessor's 2023-11-10, 2024-01-25 and 2024-12-16 tags.
const EARLY_MANAGER_IID: u128 = 0xa3175f2d_239c_4bd2_8aa0_eeba8b0b138e;
const MANAGER_IID: u128 = 0x53f5ca0b_158f_4124_900c_057158060b27;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagerLayout {
    Legacy,
    Modern,
}

#[repr(transparent)]
#[derive(Clone)]
struct ManagerInterface<const ID: u128, const EXTRA: usize>(IUnknown);

unsafe impl<const ID: u128, const EXTRA: usize> Interface for ManagerInterface<ID, EXTRA> {
    type Vtable = ManagerVtable<EXTRA>;
    const IID: GUID = GUID::from_u128(ID);
}

#[repr(C)]
struct ManagerVtable<const EXTRA: usize> {
    base: IUnknown_Vtbl,
    navigation: ManagerNavigation,
    switch_and_move_foreground: [usize; EXTRA],
    desktops: ManagerDesktops,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ManagerNavigation {
    get_count: usize,
    move_view: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    can_move_view: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i32) -> HRESULT,
    current: Out<ShellDesktop>,
    list: Out<IObjectArray>,
    adjacent: usize,
    switch: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ManagerDesktops {
    create: Out<ShellDesktop>,
    move_desktop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> HRESULT,
    remove: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    find: unsafe extern "system" fn(*mut c_void, *const GUID, *mut Option<ShellDesktop>) -> HRESULT,
}

struct ShellManager {
    interface: IUnknown,
    navigation: ManagerNavigation,
    desktops: ManagerDesktops,
}

impl ShellManager {
    fn connect(provider: &IServiceProvider, layout: ManagerLayout) -> Option<Self> {
        fn query<const ID: u128, const EXTRA: usize>(
            provider: &IServiceProvider,
        ) -> windows::core::Result<ShellManager> {
            let interface: ManagerInterface<ID, EXTRA> =
                unsafe { provider.QueryService(&DESKTOP_SERVICE)? };
            Ok(ShellManager {
                navigation: interface.vtable().navigation,
                desktops: interface.vtable().desktops,
                interface: interface.0,
            })
        }

        match layout {
            ManagerLayout::Modern => query::<MANAGER_IID, 1>(provider),
            ManagerLayout::Legacy => query::<MANAGER_IID, 0>(provider).or_else(|error| {
                if error.code().0 == E_NOINTERFACE {
                    query::<EARLY_MANAGER_IID, 0>(provider)
                } else {
                    Err(error)
                }
            }),
        }
        .ok()
    }

    fn object<T>(&self, method: Out<T>) -> Option<T> {
        let mut result = None;
        unsafe { method(self.interface.as_raw(), &mut result) }
            .ok()
            .ok()?;
        result
    }

    fn find(&self, id: DesktopId) -> Option<ShellDesktop> {
        let mut desktop = None;
        unsafe {
            (self.desktops.find)(
                self.interface.as_raw(),
                &GUID::from_u128(id.0),
                &mut desktop,
            )
        }
        .ok()
        .ok()?;
        desktop
    }

    fn current(&self) -> Option<DesktopId> {
        self.object(self.navigation.current)?.id()
    }

    fn list(&self) -> Option<Vec<DesktopId>> {
        let array = self.object(self.navigation.list)?;
        let count = unsafe { array.GetCount() }.ok()?;
        let mut ids = Vec::new();
        for index in 0..count {
            let id = desktop_id_at(&array, index)?;
            if ids.contains(&id) {
                return None;
            }
            ids.push(id);
        }
        (!ids.is_empty()).then_some(ids)
    }

    fn can_move_view(&self, hwnd: HWND, view: &ApplicationView, from: DesktopId) -> Option<bool> {
        let mut movable = 0;
        unsafe {
            (self.navigation.can_move_view)(self.interface.as_raw(), view.as_raw(), &mut movable)
        }
        .ok()
        .ok()?;
        // Pinned windows and apps have special GUIDs and never match the source.
        Some(
            movable != 0
                && self.current()? == from
                && Service::connect()?.desktop_id(hwnd).ok()? == from,
        )
    }

    fn move_desktop(&self, desktop: &ShellDesktop, index: u32) -> Option<()> {
        unsafe { (self.desktops.move_desktop)(self.interface.as_raw(), desktop.0.as_raw(), index) }
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

fn interactive_layout() -> Option<ManagerLayout> {
    let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    if object_name(desktop).ok()?.eq_ignore_ascii_case("Default") {
        installed_layout()
    } else {
        None
    }
}

struct ShellConnection {
    tray: HWND,
    provider: IServiceProvider,
    manager: ShellManager,
}

impl ShellConnection {
    fn allow_foreground(&self) -> bool {
        unsafe { CoAllowSetForegroundWindow(&self.provider, None) }.is_ok()
    }
}

pub(crate) struct DesktopShell {
    service: RefCell<Option<ShellConnection>>,
    // Release the COM interfaces before leaving their apartment.
    _apartment: Apartment,
}

impl DesktopShell {
    pub(crate) fn supported() -> bool {
        interactive_layout().is_some()
    }

    pub(crate) fn connect() -> Option<Self> {
        if !Self::supported() || shell_window().is_null() {
            return None;
        }
        let shell = Self {
            service: RefCell::default(),
            _apartment: Apartment::enter()?,
        };
        shell.with_connection(|_| Some(()))?;
        Some(shell)
    }

    fn with_manager<T>(&self, call: impl FnOnce(&ShellManager) -> Option<T>) -> Option<T> {
        self.with_connection(|shell| call(&shell.manager))
    }

    fn with_connection<T>(&self, call: impl FnOnce(&ShellConnection) -> Option<T>) -> Option<T> {
        let tray = shell_window();
        let mut service = self.service.borrow_mut();
        if tray.is_null() {
            service.take();
            return None;
        }
        if service.as_ref().is_none_or(|shell| shell.tray != tray) {
            service.take();
            let provider = shell_provider()?;
            let manager = ShellManager::connect(&provider, installed_layout()?)?;
            // Confirm that Explorer exposes typed desktop objects in Task View
            // order before enabling operations on this connection.
            let current = manager.current()?;
            if !manager.list()?.contains(&current) {
                return None;
            }
            *service = Some(ShellConnection {
                tray,
                provider,
                manager,
            });
        }
        let result = call(service.as_ref()?);
        if result.is_none() {
            // Reconnect on a subsequent observation. Never replay a Shell
            // mutation: an RPC failure can arrive after Explorer committed it.
            service.take();
        }
        result
    }

    pub(crate) fn list(&self) -> Option<Vec<DesktopId>> {
        self.with_manager(ShellManager::list)
    }

    pub(crate) fn current(&self) -> Option<DesktopId> {
        self.with_manager(ShellManager::current)
    }

    pub(crate) fn allow_shell_foreground(&self) -> bool {
        self.with_connection(|shell| Some(shell.allow_foreground()))
            .unwrap_or(false)
    }

    pub(crate) fn create(&self) -> Option<DesktopId> {
        self.with_manager(|manager| manager.object(manager.desktops.create)?.id())
    }

    pub(crate) fn move_before_first(&self, id: DesktopId, first: DesktopId) -> bool {
        if id == first {
            return false;
        }
        self.with_manager(|manager| {
            let desktop = manager.find(id)?;
            let order = manager.object(manager.navigation.list)?;
            let head = desktop_id_at(&order, 0)?;
            if head == id {
                return Some(desktop_id_at(&order, 1)? == first && manager.current()? == first);
            }
            if head != first || manager.current()? != first {
                return Some(false);
            }
            manager.move_desktop(&desktop, 0)?;
            Some(true)
        })
        .unwrap_or(false)
    }

    pub(crate) fn move_to_end(&self, id: DesktopId) -> bool {
        self.with_manager(|manager| {
            let desktop = manager.find(id)?;
            let order = manager.object(manager.navigation.list)?;
            let last = unsafe { order.GetCount() }.ok()?.checked_sub(1)?;
            if desktop_id_at(&order, last)? == id {
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

    pub(crate) fn switch(&self, from: DesktopId, to: DesktopId) -> bool {
        self.with_connection(|shell| {
            let manager = &shell.manager;
            if from == to {
                return Some(manager.current()? == from);
            }
            let desktop = manager.find(to)?;
            // Pass the caller's foreground permission to the local server that
            // will select the arriving window. A denied grant still permits
            // the desktop switch itself.
            shell.allow_foreground();
            if manager.current()? != from {
                return Some(false);
            }
            // Both relative and numbered shortcuts use the direct Shell switch.
            unsafe { (manager.navigation.switch)(manager.interface.as_raw(), desktop.0.as_raw()) }
                .ok()
                .ok()?;
            Some(true)
        })
        .unwrap_or(false)
    }

    pub(crate) fn can_move_window(&self, id: Id, from: DesktopId) -> bool {
        let hwnd = id as HWND;
        if hwnd.is_null() {
            return false;
        }
        self.with_manager(|manager| {
            manager.can_move_view(hwnd, &ApplicationView::lookup(hwnd)?, from)
        })
        .unwrap_or(false)
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
            // The caller validates the exact identity here, including foreground
            // ownership for hotkeys and implicit CLI targets.
            if !manager.can_move_view(hwnd, &view, from)? || !validate() {
                return Some(false);
            }
            let moved = unsafe {
                (manager.navigation.move_view)(
                    manager.interface.as_raw(),
                    view.as_raw(),
                    destination.0.as_raw(),
                )
            };
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
                (manager.desktops.remove)(
                    manager.interface.as_raw(),
                    desktop.0.as_raw(),
                    fallback.0.as_raw(),
                )
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
    fn move_and_switch_slots_match_the_supported_shell_manager() {
        use std::mem::offset_of;

        let slot = size_of::<usize>();
        for prefix in [
            offset_of!(ManagerVtable<0>, navigation),
            offset_of!(ManagerVtable<1>, navigation),
        ] {
            assert_eq!(prefix + offset_of!(ManagerNavigation, move_view), 4 * slot);
            assert_eq!(
                prefix + offset_of!(ManagerNavigation, can_move_view),
                5 * slot
            );
            assert_eq!(prefix + offset_of!(ManagerNavigation, current), 6 * slot);
            assert_eq!(prefix + offset_of!(ManagerNavigation, list), 7 * slot);
            assert_eq!(prefix + offset_of!(ManagerNavigation, switch), 9 * slot);
        }

        let legacy = offset_of!(ManagerVtable<0>, desktops);
        let modern = offset_of!(ManagerVtable<1>, desktops);
        assert_eq!(legacy, 10 * slot);
        assert_eq!(modern, 11 * slot);
        assert_eq!(offset_of!(ManagerDesktops, create), 0);
        assert_eq!(offset_of!(ManagerDesktops, move_desktop), slot);
        assert_eq!(offset_of!(ManagerDesktops, remove), 2 * slot);
        assert_eq!(offset_of!(ManagerDesktops, find), 3 * slot);
        assert_eq!(offset_of!(DesktopVtable, get_id), 4 * slot);
    }

    fn move_for_test(shell: &DesktopShell, id: DesktopId, index: u32) -> bool {
        shell
            .with_manager(|manager| manager.move_desktop(&manager.find(id)?, index))
            .is_some()
    }

    #[test]
    #[ignore = "read-only COM probe for a supported live Explorer session"]
    fn shell_exposes_foreground_transfer() {
        use windows::Win32::System::Com::IForegroundTransfer;
        let shell = DesktopShell::connect().expect("supported Explorer");
        assert_eq!(
            shell.with_connection(|shell| Some(
                shell.provider.cast::<IForegroundTransfer>().is_ok()
            )),
            Some(true)
        );
        let current = shell.current().expect("current desktop");
        assert!(shell.switch(current, current));
        let stale = DesktopId(!current.0);
        assert!(!shell.switch(stale, stale));
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
        assert!(shell.can_move_window(foreground as Id, source));
        assert!(!shell.can_move_window(foreground as Id, destination));
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
    #[ignore = "creates and removes its own background desktop; start on the first desktop"]
    fn desktop_shell_background_round_trip() {
        let shell = DesktopShell::connect().expect("supported Explorer on the interactive desktop");
        let original = shell.current().expect("initial desktop");
        let before = shell.list().expect("initial desktop list");
        assert_eq!(
            before.first(),
            Some(&original),
            "start on the first desktop"
        );
        let created = shell.create().expect("new desktop ID");
        assert!(!before.contains(&created));
        let after = shell.list().expect("desktop list after creation");
        assert!(after.contains(&created));
        assert_eq!(
            shell.current(),
            Some(original),
            "creation unexpectedly switched desktops"
        );
        let used = occupied(&after).expect("complete occupancy inventory");
        assert!(!used.contains(&created), "new desktop acquired a window");
        assert!(shell.move_before_first(created, original));
        let mut prepended = before.clone();
        prepended.insert(0, created);
        assert_eq!(shell.list().as_ref(), Some(&prepended));
        assert_eq!(shell.current(), Some(original));
        assert!(shell.move_before_first(created, original));
        assert_eq!(shell.list().as_ref(), Some(&prepended));
        assert!(shell.move_to_end(created));
        assert_eq!(shell.list().expect("original order restored"), after);
        assert_eq!(shell.current(), Some(original));
        assert!(occupied(&after).is_some_and(|used| !used.contains(&created)));
        assert!(shell.remove(created, original));
        assert_eq!(shell.list().as_ref(), Some(&before));
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
