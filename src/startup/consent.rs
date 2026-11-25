use std::io;
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::process::ExitCode;
use std::ptr::{null, null_mut};

use windows_sys::Win32::{
    Foundation::{ERROR_CANCELLED, LocalFree, WAIT_OBJECT_0},
    Security::{
        Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY,
        TOKEN_USER, TokenElevation, TokenUser,
    },
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        Threading::{
            GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
        },
    },
    UI::{
        Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            ShellExecuteExW,
        },
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};
use windows_sys::core::w;

use super::{protected, scheduler};

const ENABLE: &str = "--install-admin-startup";
const DISABLE: &str = "--remove-admin-startup";
const ACCOUNT_MISMATCH: u8 = 2;
const NEED_SAME_ADMIN: &str = "Для автозапуска с повышенными правами подтвердите запрос из своей учётной записи администратора.";

fn token() -> io::Result<OwnedHandle> {
    let mut handle = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub(super) fn sid() -> Result<String, String> {
    let token = token().map_err(|e| e.to_string())?;
    let mut bytes = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes) };
    if bytes == 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // TOKEN_USER has pointer alignment. Its SID is stored in the same buffer.
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    let mut raw = null_mut();
    unsafe {
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ) == 0
            || ConvertSidToStringSidW((*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid, &mut raw)
                == 0
        {
            return Err(io::Error::last_os_error().to_string());
        }
        let mut len = 0;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
        LocalFree(raw.cast());
        Ok(sid)
    }
}

fn elevated() -> Result<bool, String> {
    let token = token().map_err(|e| e.to_string())?;
    let mut info = TOKEN_ELEVATION::default();
    let mut bytes = 0;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut info).cast(),
            size_of_val(&info) as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(info.TokenIsElevated != 0)
}

fn change_task(enable: bool, sid: &str) -> Result<(), String> {
    if enable {
        protected::install(sid)?;
        scheduler::install(sid)
    } else {
        scheduler::remove(sid)
    }
}

// This entry point accepts only a fixed operation for the caller's own SID.
// No source, destination, scheduled command or task name comes from the CLI.
pub(crate) fn handle_helper(args: &[String]) -> Option<ExitCode> {
    let [flag, requested_sid] = args else {
        return None;
    };
    let enable = match flag.as_str() {
        ENABLE => true,
        DISABLE => false,
        _ => return None,
    };
    if elevated() != Ok(true) || sid().as_ref().ok() != Some(requested_sid) {
        return Some(ExitCode::from(ACCOUNT_MISMATCH));
    }
    Some(match change_task(enable, requested_sid) {
        Ok(()) => ExitCode::SUCCESS,
        Err(detail) => {
            crate::runtime::show_error(&detail);
            ExitCode::FAILURE
        }
    })
}

pub(super) fn configure(enable: bool) -> Result<bool, String> {
    let sid = sid()?;
    if elevated()? {
        change_task(enable, &sid)?;
        return Ok(true);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file: Vec<_> = exe.as_os_str().encode_wide().chain([0]).collect();
    let flag = if enable { ENABLE } else { DISABLE };
    let params: Vec<_> = format!("{flag} {sid}").encode_utf16().chain([0]).collect();
    // Called from a dedicated worker; ShellExecute can require an STA.
    let apartment = unsafe {
        CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if apartment < 0 {
        return Err(format!(
            "Не удалось подготовить запрос прав Windows ({apartment:#x})."
        ));
    }
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    let err = (unsafe { ShellExecuteExW(&mut launch) } == 0).then(io::Error::last_os_error);
    unsafe { CoUninitialize() };
    if let Some(err) = err {
        return if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            Ok(false)
        } else {
            Err(format!("Не удалось запросить права администратора: {err}"))
        };
    }
    if launch.hProcess.is_null() {
        return Err("Windows не вернула процесс настройки автозапуска.".into());
    }
    let child = unsafe { OwnedHandle::from_raw_handle(launch.hProcess) };
    let mut exit = 0;
    if unsafe { WaitForSingleObject(child.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0
        || unsafe { GetExitCodeProcess(child.as_raw_handle(), &mut exit) } == 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    match exit {
        0 => Ok(true),
        n if n == u32::from(ACCOUNT_MISMATCH) => Err(NEED_SAME_ADMIN.into()),
        // The helper displays the specific installation or scheduler error.
        1 => Ok(false),
        _ => Err(format!("Настройка автозапуска прервалась (код {exit}).")),
    }
}
