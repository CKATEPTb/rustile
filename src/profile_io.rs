use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_TOKEN, HANDLE},
    Security::*,
    System::Threading::{
        GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken, SetThreadToken,
    },
};

fn process_token() -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT,
            &mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

fn thread_token() -> io::Result<Option<OwnedHandle>> {
    let mut token = null_mut();
    if unsafe {
        OpenThreadToken(
            GetCurrentThread(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
            1,
            &mut token,
        )
    } != 0
    {
        return Ok(Some(unsafe { OwnedHandle::from_raw_handle(token) }));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_NO_TOKEN as i32) {
        Ok(None)
    } else {
        Err(err)
    }
}

fn elevated(token: HANDLE) -> io::Result<bool> {
    let (mut info, mut bytes) = (TOKEN_ELEVATION::default(), 0);
    if unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut info).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(info.TokenIsElevated != 0)
}

fn linked_token(token: HANDLE) -> Option<OwnedHandle> {
    let (mut info, mut bytes) = (TOKEN_LINKED_TOKEN::default(), 0);
    (unsafe {
        GetTokenInformation(
            token,
            TokenLinkedToken,
            (&raw mut info).cast(),
            size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut bytes,
        )
    } != 0)
        .then(|| unsafe { OwnedHandle::from_raw_handle(info.LinkedToken) })
}

// Administrators with UAC disabled have no linked standard token. Restrict
// their token to grants for the account itself and drop privileges and high
// integrity before opening a path inside their writable profile.
fn restricted_token(token: HANDLE) -> io::Result<OwnedHandle> {
    let mut buf = [0usize;
        (size_of::<TOKEN_USER>() + SECURITY_MAX_SID_SIZE as usize).div_ceil(size_of::<usize>())];
    let mut bytes = 0;
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    let restrict = SID_AND_ATTRIBUTES {
        Sid: user.User.Sid,
        Attributes: 0,
    };
    let mut out = null_mut();
    if unsafe {
        CreateRestrictedToken(
            token,
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN,
            0,
            null(),
            0,
            null(),
            1,
            &restrict,
            &mut out,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let out = unsafe { OwnedHandle::from_raw_handle(out) };
    #[repr(C)]
    #[derive(Default)]
    struct MediumIntegrity {
        label: TOKEN_MANDATORY_LABEL,
        sid: [u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(4)],
    }
    let mut medium = MediumIntegrity::default();
    let mut len = size_of_val(&medium.sid) as u32;
    if unsafe {
        CreateWellKnownSid(
            WinMediumLabelSid,
            null_mut(),
            medium.sid.as_mut_ptr().cast(),
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    medium.label.Label = SID_AND_ATTRIBUTES {
        Sid: medium.sid.as_mut_ptr().cast(),
        Attributes: 0x20, /* SE_GROUP_INTEGRITY */
    };
    if unsafe {
        SetTokenInformation(
            out.as_raw_handle(),
            TokenIntegrityLevel,
            (&raw const medium).cast(),
            size_of_val(&medium.label) as u32 + len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(out)
}

struct Restore(Option<OwnedHandle>);
impl Drop for Restore {
    fn drop(&mut self) {
        let ok = match &self.0 {
            Some(previous) => unsafe { SetThreadToken(null(), previous.as_raw_handle()) },
            None => unsafe { RevertToSelf() },
        };
        if ok == 0 {
            std::process::abort();
        }
    }
}

/// Run file or registry operations on a user-writable profile without giving
/// them the additional privileges of the scheduled window manager.
pub(crate) fn with_user<T>(op: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let previous = thread_token()?;
    let process;
    let token = match &previous {
        Some(existing) => existing.as_raw_handle(),
        None => {
            process = process_token()?;
            process.as_raw_handle()
        }
    };
    if !elevated(token)? {
        return op();
    }
    let using_linked = linked_token(token).is_some_and(|linked| {
        elevated(linked.as_raw_handle()).is_ok_and(|v| !v)
            && unsafe { ImpersonateLoggedOnUser(linked.as_raw_handle()) } != 0
    });
    if !using_linked {
        let limited = restricted_token(token)?;
        if unsafe { ImpersonateLoggedOnUser(limited.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let _restore = Restore(previous);
    op()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_profile_token_can_save_and_preserves_impersonation() {
        let previous = thread_token().unwrap();
        let process = process_token().unwrap();
        let limited = restricted_token(process.as_raw_handle()).unwrap();
        let dir =
            std::env::temp_dir().join(format!("rustile-profile-token-{}", std::process::id()));
        {
            assert_ne!(
                unsafe { ImpersonateLoggedOnUser(limited.as_raw_handle()) },
                0
            );
            let _restore = Restore(previous);
            std::fs::create_dir(&dir).unwrap();
            with_user(|| std::fs::write(dir.join("settings"), b"profile")).unwrap();
            assert!(thread_token().unwrap().is_some());
            std::fs::remove_file(dir.join("settings")).unwrap();
            std::fs::remove_dir(dir).unwrap();
        }
    }
}
