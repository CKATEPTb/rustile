use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows::{
    Win32::{
        Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, RPC_E_CHANGED_MODE, VARIANT_BOOL,
        },
        System::{Com::*, TaskScheduler::*, Variant::VARIANT},
    },
    core::{BSTR, Error, HRESULT, Interface, Result as ComResult},
};
use windows_sys::Win32::{
    Foundation::{GENERIC_READ, LocalFree},
    Security::{
        self as security, ACCESS_ALLOWED_ACE, Authorization::*, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PSID,
    },
    Storage::FileSystem::FILE_GENERIC_READ,
};

use super::protected;

const SOURCE: &str = "Rustile.Windows.Logon";
const OCCUPIED: &str = "Имя задания Rustile в Планировщике Windows занято другим заданием.";

fn failure(err: Error) -> String {
    format!("Планировщик заданий Windows: {err}")
}

struct Apartment(bool);

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

struct Scheduler {
    service: ITaskService,
    root: ITaskFolder,
    // Interfaces above must be released before the thread leaves COM.
    _apartment: Apartment,
}

impl Scheduler {
    fn connect() -> ComResult<Self> {
        unsafe {
            let status = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if status != RPC_E_CHANGED_MODE {
                status.ok()?;
            }
            let apartment = Apartment(status.is_ok());
            let service: ITaskService =
                CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
            let empty = VARIANT::default();
            service.Connect(&empty, &empty, &empty, &empty)?;
            let root = service.GetFolder(&BSTR::from(r"\"))?;
            Ok(Self {
                service,
                root,
                _apartment: apartment,
            })
        }
    }

    fn lookup(&self, name: &BSTR) -> ComResult<Option<IRegisteredTask>> {
        match unsafe { self.root.GetTask(name) } {
            Ok(task) => Ok(Some(task)),
            Err(err)
                if err.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
                    || err.code() == HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

fn wide_path(path: &Path) -> BSTR {
    BSTR::from_wide(&path.as_os_str().encode_wide().collect::<Vec<_>>())
}

fn definition(service: &ITaskService, sid: &str, exe: &Path) -> ComResult<ITaskDefinition> {
    let user = BSTR::from(sid);
    unsafe {
        let task = service.NewTask(0)?;
        let info = task.RegistrationInfo()?;
        info.SetAuthor(&BSTR::from("Rustile"))?;
        info.SetSource(&BSTR::from(SOURCE))?;
        info.SetDescription(&BSTR::from("Запуск Rustile при входе в Windows."))?;
        let principal = task.Principal()?;
        principal.SetUserId(&user)?;
        principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)?;
        principal.SetRunLevel(TASK_RUNLEVEL_HIGHEST)?;
        let trigger: ILogonTrigger = task.Triggers()?.Create(TASK_TRIGGER_LOGON)?.cast()?;
        trigger.SetUserId(&user)?;
        let action: IExecAction = task.Actions()?.Create(TASK_ACTION_EXEC)?.cast()?;
        action.SetPath(&wide_path(exe))?;
        action.SetWorkingDirectory(&wide_path(exe.parent().unwrap()))?;
        let settings = task.Settings()?;
        settings.SetDisallowStartIfOnBatteries(false.into())?;
        settings.SetStopIfGoingOnBatteries(false.into())?;
        settings.SetExecutionTimeLimit(&BSTR::from("PT0S"))?;
        settings.SetAllowDemandStart(false.into())?;
        settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
        Ok(task)
    }
}

fn property<T: Default>(read: impl FnOnce(*mut T) -> ComResult<()>) -> ComResult<T> {
    let mut value = T::default();
    read(&mut value)?;
    Ok(value)
}

struct LocalMem(*mut std::ffi::c_void);
impl Drop for LocalMem {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

// The service may return DOMAIN\name for an identity originally specified by SID.
fn same_user(account: &BSTR, sid: &str) -> bool {
    if account == sid {
        return true;
    }
    if account.is_empty() || account.contains(&0) {
        return false;
    }
    unsafe {
        let mut expected = null_mut();
        if ConvertStringSidToSidW(BSTR::from(sid).as_ptr(), &mut expected) == 0 {
            return false;
        }
        let expected = LocalMem(expected);
        let (mut bytes, mut domain_len, mut kind) = (0, 0, 0);
        security::LookupAccountNameW(
            null(),
            account.as_ptr(),
            null_mut(),
            &mut bytes,
            null_mut(),
            &mut domain_len,
            &mut kind,
        );
        if bytes == 0 {
            return false;
        }
        let mut actual = vec![0u32; (bytes as usize).div_ceil(4)];
        let mut domain = vec![0u16; domain_len as usize];
        security::LookupAccountNameW(
            null(),
            account.as_ptr(),
            actual.as_mut_ptr().cast(),
            &mut bytes,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        ) != 0
            && security::EqualSid(expected.0, actual.as_mut_ptr().cast()) != 0
    }
}

// A user-created lookalike at our public task name must never be upgraded.
// The owner and every writer must be SYSTEM or the local Administrators group.
fn protected_acl(descriptor: &BSTR) -> bool {
    unsafe fn privileged(sid: PSID) -> bool {
        !sid.is_null()
            && unsafe {
                security::IsWellKnownSid(sid, security::WinLocalSystemSid) != 0
                    || security::IsWellKnownSid(sid, security::WinBuiltinAdministratorsSid) != 0
            }
    }
    unsafe {
        let mut sd = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        ) == 0
        {
            return false;
        }
        let sd = LocalMem(sd);
        let (mut owner, mut acl) = (null_mut(), null_mut());
        let (mut defaulted, mut present) = (0, 0);
        if security::GetSecurityDescriptorOwner(sd.0, &mut owner, &mut defaulted) == 0
            || !privileged(owner)
            || security::GetSecurityDescriptorDacl(sd.0, &mut present, &mut acl, &mut defaulted)
                == 0
            || present == 0
            || acl.is_null()
        {
            return false;
        }
        (0..u32::from((*acl).AceCount)).all(|index| {
            let mut ptr = null_mut();
            if security::GetAce(acl, index, &mut ptr) == 0 {
                return false;
            }
            // The descriptor we install contains simple ACCESS_ALLOWED_ACEs (type 0).
            let ace = ptr.cast::<ACCESS_ALLOWED_ACE>();
            (*ace).Header.AceType == 0
                && (privileged((&raw mut (*ace).SidStart).cast())
                    || (*ace).Mask & !(FILE_GENERIC_READ | GENERIC_READ) == 0)
        })
    }
}

fn matches_definition(task: &ITaskDefinition, sid: &str, exe: &Path) -> ComResult<bool> {
    unsafe {
        let marker = property(|out| task.RegistrationInfo()?.Source(out))?;
        let principal = task.Principal()?;
        let triggers = task.Triggers()?;
        let actions = task.Actions()?;
        if marker != SOURCE
            || property(|out| principal.RunLevel(out))? != TASK_RUNLEVEL_HIGHEST
            || property(|out| principal.LogonType(out))? != TASK_LOGON_INTERACTIVE_TOKEN
            || !same_user(&property(|out| principal.UserId(out))?, sid)
            || property(|out| triggers.Count(out))? != 1
            || property(|out| actions.Count(out))? != 1
        {
            return Ok(false);
        }
        let trigger = triggers.get_Item(1)?;
        let action = actions.get_Item(1)?;
        if property(|out| trigger.Type(out))? != TASK_TRIGGER_LOGON
            || property(|out| action.Type(out))? != TASK_ACTION_EXEC
        {
            return Ok(false);
        }
        let trigger: ILogonTrigger = trigger.cast()?;
        let action: IExecAction = action.cast()?;
        let path = OsString::from_wide(&property(|out| action.Path(out))?);
        let dir = OsString::from_wide(&property(|out| action.WorkingDirectory(out))?);
        Ok(same_user(&property(|out| trigger.UserId(out))?, sid)
            && path.eq_ignore_ascii_case(exe.as_os_str())
            && dir.eq_ignore_ascii_case(exe.parent().unwrap().as_os_str())
            && property(|out| action.Arguments(out))?.is_empty())
    }
}

struct Registration {
    // Release the COM task before Scheduler's apartment guard.
    task: Option<IRegisteredTask>,
    name: BSTR,
    exe: PathBuf,
    scheduler: Scheduler,
}

impl Registration {
    fn open(sid: &str) -> Result<Self, String> {
        let scheduler = Scheduler::connect().map_err(failure)?;
        let name = BSTR::from(format!("Rustile.Autostart.{sid}"));
        let exe = protected::exe_path(sid)?;
        // The scheduler expands %variables% in Exec paths at logon.
        if exe.as_os_str().encode_wide().any(|c| c == b'%' as u16) {
            return Err("Планировщик не поддерживает знак % в пути установки Rustile.".into());
        }
        let task = scheduler.lookup(&name).map_err(failure)?;
        if let Some(current) = &task {
            let flags = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            let sddl = unsafe { current.GetSecurityDescriptor(flags as i32) }.map_err(failure)?;
            let def = unsafe { current.Definition() }.map_err(failure)?;
            if !protected_acl(&sddl) || !matches_definition(&def, sid, &exe).map_err(failure)? {
                return Err(OCCUPIED.into());
            }
        }
        Ok(Self {
            scheduler,
            name,
            exe,
            task,
        })
    }
}

pub(super) fn installed(sid: &str) -> Result<bool, String> {
    let record = Registration::open(sid)?;
    let Some(task) = &record.task else {
        return Ok(false);
    };
    let trigger = (|| unsafe { task.Definition()?.Triggers()?.get_Item(1) })().map_err(failure)?;
    let enabled: VARIANT_BOOL = property(|out| unsafe { trigger.Enabled(out) }).map_err(failure)?;
    Ok(record.exe.is_file()
        && enabled.as_bool()
        && unsafe { task.Enabled() }.map_err(failure)?.as_bool())
}

fn task_acl(sid: &str) -> VARIANT {
    VARIANT::from(format!("O:BAG:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GR;;;{sid})").as_str())
}

pub(super) fn install(sid: &str) -> Result<(), String> {
    let record = Registration::open(sid)?;
    let task = definition(&record.scheduler.service, sid, &record.exe).map_err(failure)?;
    let mode = if record.task.is_some() {
        TASK_UPDATE
    } else {
        TASK_CREATE
    };
    let flags = mode.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0 | TASK_IGNORE_REGISTRATION_TRIGGERS.0;
    let empty = VARIANT::default();
    unsafe {
        record.scheduler.root.RegisterTaskDefinition(
            &record.name,
            &task,
            flags,
            &VARIANT::from(sid),
            &empty,
            TASK_LOGON_INTERACTIVE_TOKEN,
            &task_acl(sid),
        )
    }
    .map_err(failure)?;
    Ok(())
}

pub(super) fn remove(sid: &str) -> Result<(), String> {
    let record = Registration::open(sid)?;
    if record.task.is_some() {
        unsafe { record.scheduler.root.DeleteTask(&record.name, 0) }.map_err(failure)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logon_definition_round_trips_in_windows_without_registering_it() {
        let scheduler = Scheduler::connect().unwrap();
        let missing = BSTR::from(format!("Rustile.UnitTest.Missing.{}", std::process::id()));
        assert!(scheduler.lookup(&missing).unwrap().is_none());
        let sid = crate::startup::consent::sid().unwrap();
        let exe = protected::exe_path(&sid).unwrap();
        let task = definition(&scheduler.service, &sid, &exe).unwrap();
        unsafe {
            // VALIDATE_ONLY returns S_OK and a null task. The generated
            // convenience method requires a non-null IRegisteredTask, so
            // inspect the native HRESULT and out parameter directly.
            let mut registered = null_mut();
            (scheduler.root.vtable().RegisterTaskDefinition)(
                scheduler.root.as_raw(),
                missing.as_ptr().cast_mut().cast(),
                task.as_raw(),
                TASK_VALIDATE_ONLY.0,
                VARIANT::from(sid.as_str()),
                VARIANT::default(),
                TASK_LOGON_INTERACTIVE_TOKEN,
                task_acl(&sid),
                &mut registered,
            )
            .ok()
            .unwrap();
            assert!(registered.is_null());
            assert!(scheduler.lookup(&missing).unwrap().is_none());
            let xml: BSTR = property(|out| task.XmlText(out)).unwrap();
            let copy = scheduler.service.NewTask(0).unwrap();
            copy.SetXmlText(&xml).unwrap();
            assert!(matches_definition(&copy, &sid, &exe).unwrap());
            let action: IExecAction = copy.Actions().unwrap().get_Item(1).unwrap().cast().unwrap();
            action.SetArguments(&BSTR::from("unexpected")).unwrap();
            assert!(!matches_definition(&copy, &sid, &exe).unwrap());
        }
    }

    #[test]
    fn task_permissions_exclude_unprivileged_writers_and_owners() {
        let sid = crate::startup::consent::sid().unwrap();
        let acl = |owner: &str, rights: &str| {
            BSTR::from(format!(
                "O:{owner}G:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;{rights};;;{sid})"
            ))
        };
        assert!(protected_acl(&acl("BA", "GR")));
        assert!(!protected_acl(&acl("BA", "GW")));
        assert!(!protected_acl(&acl(&sid, "GR")));
    }
}
