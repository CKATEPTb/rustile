//! Runs operations on the user's writable profile with that user's ordinary
//! privileges. In an elevated manager the scope is thread-local. On leaving
//! the scope, the guard restores the thread's prior impersonation state.

use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{ERROR_BAD_IMPERSONATION_LEVEL, ERROR_NO_TOKEN, HANDLE},
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

fn thread_token(access: TOKEN_ACCESS_MASK) -> io::Result<Option<OwnedHandle>> {
    let mut token = null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), access, 1, &mut token) } != 0 {
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

struct Restore<'a>(Option<&'a OwnedHandle>);
impl Drop for Restore<'_> {
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

fn require_io_level(token: HANDLE) -> io::Result<()> {
    let (mut level, mut bytes) = (SecurityAnonymous, 0);
    if unsafe {
        GetTokenInformation(
            token,
            TokenImpersonationLevel,
            (&raw mut level).cast(),
            size_of_val(&level) as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if level < SecurityImpersonation {
        return Err(io::Error::from_raw_os_error(
            ERROR_BAD_IMPERSONATION_LEVEL as i32,
        ));
    }
    Ok(())
}

fn impersonate_for_io(token: HANDLE, previous: Option<&OwnedHandle>) -> io::Result<Restore<'_>> {
    if unsafe { ImpersonateLoggedOnUser(token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let restore = Restore(previous);
    // Windows can successfully install a linked identification token. The thread
    // then cannot open files. Restore its previous identity before trying a fallback.
    let installed = thread_token(TOKEN_QUERY)?
        .ok_or_else(|| io::Error::from_raw_os_error(ERROR_NO_TOKEN as i32))?;
    require_io_level(installed.as_raw_handle())?;
    Ok(restore)
}

fn profile_context(
    token: HANDLE,
    linked: Option<OwnedHandle>,
    previous: Option<&OwnedHandle>,
) -> io::Result<Restore<'_>> {
    if let Some(linked) = linked
        && elevated(linked.as_raw_handle()).is_ok_and(|v| !v)
        && let Ok(restore) = impersonate_for_io(linked.as_raw_handle(), previous)
    {
        return Ok(restore);
    }
    impersonate_for_io(restricted_token(token)?.as_raw_handle(), previous)
}

/// Run file or registry operations on a user-writable profile without giving
/// them the additional privileges of the scheduled window manager.
pub(crate) fn with_user<T>(op: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let previous = thread_token(TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE)?;
    let process;
    let token = match &previous {
        Some(existing) => {
            require_io_level(existing.as_raw_handle())?;
            existing.as_raw_handle()
        }
        None => {
            process = process_token()?;
            process.as_raw_handle()
        }
    };
    if !elevated(token)? {
        return op();
    }
    let _restore = profile_context(token, linked_token(token), previous.as_ref())?;
    op()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_profile_token_can_save_and_preserves_impersonation() {
        let previous = thread_token(TOKEN_QUERY | TOKEN_IMPERSONATE).unwrap();
        let process = process_token().unwrap();
        let limited = restricted_token(process.as_raw_handle()).unwrap();
        let dir =
            std::env::temp_dir().join(format!("rustile-profile-token-{}", std::process::id()));
        {
            assert_ne!(
                unsafe { ImpersonateLoggedOnUser(limited.as_raw_handle()) },
                0
            );
            let _restore = Restore(previous.as_ref());
            std::fs::create_dir(&dir).unwrap();
            with_user(|| std::fs::write(dir.join("settings"), b"profile")).unwrap();
            assert!(thread_token(TOKEN_QUERY).unwrap().is_some());
            std::fs::remove_file(dir.join("settings")).unwrap();
            std::fs::remove_dir(dir).unwrap();
        }
    }

    #[test]
    fn identification_only_linked_token_falls_back_before_profile_io() {
        let previous = thread_token(TOKEN_QUERY | TOKEN_IMPERSONATE).unwrap();
        let process = process_token().unwrap();
        let standard = restricted_token(process.as_raw_handle()).unwrap();
        let mut identification = null_mut();
        assert_ne!(
            unsafe {
                DuplicateTokenEx(
                    standard.as_raw_handle(),
                    TOKEN_QUERY | TOKEN_IMPERSONATE,
                    null(),
                    SecurityIdentification,
                    TokenImpersonation,
                    &mut identification,
                )
            },
            0
        );
        let identification = unsafe { OwnedHandle::from_raw_handle(identification) };
        assert!(!elevated(identification.as_raw_handle()).unwrap());
        assert_eq!(
            require_io_level(identification.as_raw_handle())
                .unwrap_err()
                .raw_os_error(),
            Some(ERROR_BAD_IMPERSONATION_LEVEL as i32)
        );
        let path = std::env::temp_dir().join(format!(
            "rustile-linked-profile-{}.toml",
            std::process::id()
        ));
        {
            let _restore = profile_context(
                process.as_raw_handle(),
                Some(identification),
                previous.as_ref(),
            )
            .unwrap();
            let installed = thread_token(TOKEN_QUERY).unwrap().unwrap();
            assert_ne!(unsafe { IsTokenRestricted(installed.as_raw_handle()) }, 0);
            let mut file = std::fs::File::create_new(&path).unwrap();
            io::Write::write_all(&mut file, b"test profile").unwrap();
            drop(file);
            assert_eq!(std::fs::read(&path).unwrap(), b"test profile");
            std::fs::remove_file(&path).unwrap();
        }
        assert_eq!(
            thread_token(TOKEN_QUERY).unwrap().is_some(),
            previous.is_some()
        );
    }
}
