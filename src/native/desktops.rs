//! Resolve HWNDs to virtual desktop IDs for the tiler. An unresolved membership
//! is distinct from a window on another desktop during an Explorer transition.
//! The private Shell interfaces for navigation and enumeration live in [`shell`].

pub(crate) mod occupancy;
pub(crate) mod shell;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::io;
use std::ptr::{NonNull, null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CO_E_OBJNOTCONNECTED, CO_E_SERVER_STOPPING, HANDLE, HWND, RPC_E_CHANGED_MODE,
    RPC_E_DISCONNECTED, RPC_E_SERVER_DIED, RPC_E_SERVER_DIED_DNE, TYPE_E_ELEMENTNOTFOUND,
};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Shell::VirtualDesktopManager;
use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows_sys::core::{BOOL, GUID, HRESULT, IUnknown_Vtbl, w};

use crate::layout::Id;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DesktopId(pub(crate) u128);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopMembership {
    Current(DesktopId),
    Other(DesktopId),
    Unknown,
}

pub(crate) fn object_name(handle: HANDLE) -> io::Result<String> {
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
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf16_lossy(
        name.strip_suffix(&[0]).unwrap_or(&name),
    ))
}

fn shell_window() -> HWND {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), null()) }
}

// The public, stable IVirtualDesktopManager ABI in ShObjIdl_core.h. windows-sys
// exposes its CLSID and the IUnknown header, but omits the interface methods.
const IID_DESKTOP_MANAGER: GUID = GUID::from_u128(0xa5cd92ff_29be_454c_8d04_d82879fb3f1b);

#[repr(C)]
struct DesktopVtable {
    base: IUnknown_Vtbl,
    is_current: unsafe extern "system" fn(*mut c_void, HWND, *mut BOOL) -> HRESULT,
    desktop_id: unsafe extern "system" fn(*mut c_void, HWND, *mut GUID) -> HRESULT,
    _move_window_to_desktop: unsafe extern "system" fn(*mut c_void, HWND, *const GUID) -> HRESULT,
}

struct Service {
    interface: NonNull<*const DesktopVtable>,
    shell: HWND,
}

impl Service {
    fn connect() -> Option<Self> {
        let mut ptr = null_mut();
        let result = unsafe {
            CoCreateInstance(
                &VirtualDesktopManager,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_DESKTOP_MANAGER,
                &mut ptr,
            )
        };
        if result < 0 {
            return None;
        }
        Some(Self {
            interface: NonNull::new(ptr.cast())?,
            shell: shell_window(),
        })
    }

    fn vtable(&self) -> &DesktopVtable {
        // CoCreateInstance returned this exact IID and we own its reference.
        unsafe { &**self.interface.as_ptr() }
    }

    fn desktop_id(&self, hwnd: HWND) -> Result<DesktopId, HRESULT> {
        let this = self.interface.as_ptr().cast();
        let mut guid = GUID::default();
        let result = unsafe { (self.vtable().desktop_id)(this, hwnd, &mut guid) };
        if result != 0 {
            return Err(result);
        }
        Ok(DesktopId(
            (u128::from(guid.data1) << 96)
                | (u128::from(guid.data2) << 80)
                | (u128::from(guid.data3) << 64)
                | u128::from(u64::from_be_bytes(guid.data4)),
        ))
    }

    fn is_current(&self, hwnd: HWND) -> Result<bool, HRESULT> {
        let mut current = 0;
        let result = unsafe {
            (self.vtable().is_current)(self.interface.as_ptr().cast(), hwnd, &mut current)
        };
        (result == 0).then_some(current != 0).ok_or(result)
    }

    fn query(&self, hwnd: HWND) -> Result<DesktopMembership, HRESULT> {
        let id = self.desktop_id(hwnd)?;
        // The shell also returns GUID_NULL for windows without a workspace.
        if id.0 == 0 {
            return Ok(DesktopMembership::Unknown);
        }
        Ok(if self.is_current(hwnd)? {
            DesktopMembership::Current(id)
        } else {
            DesktopMembership::Other(id)
        })
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        unsafe { (self.vtable().base.Release)(self.interface.as_ptr().cast()) };
    }
}

fn opening_membership(
    assigned: Result<DesktopId, HRESULT>,
    is_current: impl FnOnce() -> Result<bool, HRESULT>,
    current_desktop: impl Fn() -> Option<DesktopId>,
) -> Result<DesktopMembership, HRESULT> {
    use DesktopMembership::*;
    match assigned {
        Ok(id) if occupancy::is_pinned(id) => Ok(Unknown),
        Ok(id) if id.0 != 0 => Ok(if is_current()? {
            Current(id)
        } else {
            Other(id)
        }),
        Ok(_) | Err(TYPE_E_ELEMENTNOTFOUND) => {
            // An ordinary new HWND can have no registered Shell view until
            // ShowWindow. Only speculate while Explorer's selection is stable.
            let Some(before) = current_desktop() else {
                return Ok(Unknown);
            };
            Ok(if is_current()? && current_desktop() == Some(before) {
                Current(before)
            } else {
                Unknown
            })
        }
        Err(hr) => Err(hr),
    }
}

pub(crate) struct Desktops {
    isolated: bool,
    com_result: HRESULT,
    service: RefCell<Option<Service>>,
    opening_shell: RefCell<Option<shell::DesktopShell>>,
    retry_after: Cell<Option<Instant>>,
    #[cfg(test)]
    fixture: Option<std::collections::HashMap<Id, DesktopMembership>>,
    #[cfg(test)]
    pub(crate) opening_hint_fixture: Option<(Id, DesktopId)>,
}

impl Desktops {
    pub fn new() -> Self {
        let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        // A private Win32 desktop has no Explorer virtual desktops. A temporary
        // shell outage on Default must never merge its saved workspaces.
        let isolated = object_name(desktop).is_ok_and(|name| !name.eq_ignore_ascii_case("Default"))
            && shell_window().is_null();
        let com_result = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        Self {
            isolated,
            com_result,
            service: RefCell::default(),
            opening_shell: RefCell::default(),
            retry_after: Cell::default(),
            #[cfg(test)]
            fixture: None,
            #[cfg(test)]
            opening_hint_fixture: None,
        }
    }

    pub fn membership(&self, id: Id) -> DesktopMembership {
        self.lookup(id, Service::query)
    }

    /// A placement hint for a newly created HWND. Admission and
    /// changes to a workspace always require the regular membership query.
    pub(crate) fn opening_desktop(&self, id: Id) -> Option<DesktopId> {
        #[cfg(test)]
        if let Some((window, desktop)) = self.opening_hint_fixture
            && window == id
        {
            return Some(desktop);
        }
        let membership = self.lookup(id, |service, hwnd| {
            if service.shell.is_null() || shell_window() != service.shell {
                return Err(CO_E_OBJNOTCONNECTED);
            }
            opening_membership(
                service.desktop_id(hwnd),
                || service.is_current(hwnd),
                || {
                    let mut cached = self.opening_shell.borrow_mut();
                    if cached.is_none() {
                        *cached = shell::DesktopShell::connect();
                    }
                    cached.as_ref()?.current()
                },
            )
        });
        match membership {
            DesktopMembership::Current(id) if !occupancy::is_pinned(id) => Some(id),
            _ => None,
        }
    }

    fn lookup(
        &self,
        id: Id,
        query: impl Fn(&Service, HWND) -> Result<DesktopMembership, HRESULT>,
    ) -> DesktopMembership {
        #[cfg(test)]
        if let Some(fixture) = &self.fixture {
            return fixture
                .get(&id)
                .copied()
                .unwrap_or(DesktopMembership::Unknown);
        }
        if self.isolated {
            return DesktopMembership::Current(DesktopId(0));
        }
        if (self.com_result < 0 && self.com_result != RPC_E_CHANGED_MODE)
            || self.retry_after.get().is_some_and(|at| Instant::now() < at)
        {
            return DesktopMembership::Unknown;
        }
        let mut cached = self.service.borrow_mut();
        for _ in 0..2 {
            if cached.is_none() {
                *cached = Service::connect();
            }
            let Some(service) = cached.as_ref() else {
                break;
            };
            match query(service, id as HWND) {
                Ok(membership) => return membership,
                Err(code)
                    if matches!(
                        code,
                        RPC_E_DISCONNECTED
                            | RPC_E_SERVER_DIED
                            | RPC_E_SERVER_DIED_DNE
                            | CO_E_OBJNOTCONNECTED
                            | CO_E_SERVER_STOPPING
                    ) || shell_window() != service.shell =>
                {
                    *cached = None;
                }
                Err(_) => return DesktopMembership::Unknown,
            }
        }
        // Retry on the next window event after this deadline; no idle timer.
        self.retry_after
            .set(Some(Instant::now() + Duration::from_millis(150)));
        DesktopMembership::Unknown
    }

    #[cfg(test)]
    pub fn set_memberships(
        &mut self,
        memberships: impl IntoIterator<Item = (Id, DesktopMembership)>,
    ) {
        self.fixture = Some(memberships.into_iter().collect());
    }
}

impl Drop for Desktops {
    fn drop(&mut self) {
        self.service.get_mut().take();
        self.opening_shell.get_mut().take();
        // S_OK and S_FALSE both need balancing on this thread. An apartment
        // already initialized in a different model remains owned by its caller.
        if self.com_result >= 0 {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DesktopMembership::*;

    #[test]
    #[ignore = "requires interactive Explorer; creates and destroys a never-shown test HWND"]
    fn hidden_opening_hint_identifies_the_selected_desktop_before_admission() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, IsWindowVisible, WS_OVERLAPPEDWINDOW,
        };
        let desktops = Desktops::new();
        let shell = shell::DesktopShell::connect().expect("supported interactive Explorer");
        let current = shell.current().expect("selected desktop");
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                w!("Rustile hidden opening test"),
                WS_OVERLAPPEDWINDOW,
                150,
                150,
                640,
                480,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            )
        };
        assert!(!hwnd.is_null());
        let opening = desktops.opening_desktop(hwnd as Id);
        let membership = desktops.membership(hwnd as Id);
        let still_hidden = unsafe { IsWindowVisible(hwnd) } == 0;
        unsafe { DestroyWindow(hwnd) };
        assert_eq!(
            shell.current(),
            Some(current),
            "selection changed during the probe"
        );
        assert!(still_hidden);
        assert_eq!(opening, Some(current));
        // Whichever registration timing Explorer used, the hint cannot assign
        // this HWND to a different real workspace.
        assert!(membership == Unknown || membership == Current(current));
    }

    #[test]
    fn unregistered_openings_require_a_positive_current_check_between_matching_selections() {
        let home = DesktopId(10);
        for missing in [Ok(DesktopId(0)), Err(TYPE_E_ELEMENTNOTFOUND)] {
            assert_eq!(
                opening_membership(missing, || Ok(true), || Some(home)),
                Ok(Current(home))
            );
            assert_eq!(
                opening_membership(missing, || Ok(false), || Some(home)),
                Ok(Unknown)
            );
            assert_eq!(
                opening_membership(missing, || Err(-1), || Some(home)),
                Err(-1)
            );
            assert_eq!(
                opening_membership(missing, || panic!(), || None),
                Ok(Unknown)
            );
            let first = Cell::new(true);
            assert_eq!(
                opening_membership(
                    missing,
                    || Ok(true),
                    || {
                        Some(if first.replace(false) {
                            home
                        } else {
                            DesktopId(20)
                        })
                    }
                ),
                Ok(Unknown)
            );
        }
    }

    #[test]
    fn assigned_other_and_pinned_windows_and_query_errors_do_not_borrow_the_current_desktop() {
        let home = DesktopId(10);
        let pinned = DesktopId(0xc2ddea68_66f2_4cf9_8264_1bfd00fbbbac);
        let no_shell = || panic!("explicit association must not sample the selected desktop");
        assert_eq!(
            opening_membership(Ok(home), || Ok(true), no_shell),
            Ok(Current(home))
        );
        assert_eq!(
            opening_membership(Ok(home), || Ok(false), no_shell),
            Ok(Other(home))
        );
        assert_eq!(
            opening_membership(Ok(pinned), || panic!(), no_shell),
            Ok(Unknown)
        );
        assert_eq!(opening_membership(Err(-1), || panic!(), no_shell), Err(-1));
    }
}
