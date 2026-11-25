use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance, IServiceProvider};
use windows::Win32::UI::Shell::Common::IObjectArray;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND, RPC_E_CHANGED_MODE};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
};
use windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::core::w;

use super::{DesktopId, object_name, shell_window};

const IMMERSIVE_SHELL: GUID = GUID::from_u128(0xc2f03a33_21f5_47fa_b4bb_156362a2f239);
const DESKTOP_SERVICE: GUID = GUID::from_u128(0xc5e0cdca_7b6e_41b2_9fc4_d93975cc467b);

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
    move_view: usize,
    can_move_view: usize,
    current: Out<ShellDesktop>,
    list: Out<IObjectArray>,
    adjacent: usize,
    switch: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    switch_and_move_foreground: usize,
    create: Out<ShellDesktop>,
    move_desktop: usize,
    remove: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    find: unsafe extern "system" fn(*mut c_void, *const GUID, *mut Option<ShellDesktop>) -> HRESULT,
}

impl ShellManager {
    fn connect() -> Option<Self> {
        unsafe {
            let provider: IServiceProvider =
                CoCreateInstance(&IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER).ok()?;
            provider.QueryService(&DESKTOP_SERVICE).ok()
        }
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

    pub(crate) fn switch(&self, id: DesktopId) -> bool {
        self.with_manager(|manager| {
            let desktop = manager.find(id)?;
            unsafe { (manager.vtable().switch)(manager.as_raw(), desktop.as_raw()) }
                .ok()
                .ok()
        })
        .is_some()
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
    use super::*;

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
    #[ignore = "creates and removes its own background desktop on a supported Explorer session"]
    fn desktop_shell_background_round_trip() {
        use super::super::occupancy::occupied;

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
        assert!(shell.remove(created, original));
        assert!(
            !shell
                .list()
                .expect("desktop list after removal")
                .contains(&created)
        );
        assert_eq!(shell.current(), Some(original));
    }
}
