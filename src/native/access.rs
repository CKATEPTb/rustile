use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::sync::OnceLock;
use windows_sys::Win32::{
    Foundation::HANDLE,
    Security::{
        GetLengthSid, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, IsValidSid,
        PSID, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
        TokenIntegrityLevel, TokenUser,
    },
    System::{
        SystemServices::{SECURITY_MANDATORY_HIGH_RID, SECURITY_MANDATORY_SYSTEM_RID},
        Threading::{
            GetCurrentProcess, GetProcessInformation, OpenProcessToken,
            PROCESS_PROTECTION_LEVEL_INFORMATION, PROTECTION_LEVEL_NONE,
            ProcessProtectionLevelInfo,
        },
    },
};

const HIGH: u32 = SECURITY_MANDATORY_HIGH_RID as u32;
const SYSTEM: u32 = SECURITY_MANDATORY_SYSTEM_RID as u32;
type UserSid = [u8; SECURITY_MAX_SID_SIZE as usize];

// TOKEN_USER and TOKEN_MANDATORY_LABEL both contain one SID_AND_ATTRIBUTES,
// followed in GetTokenInformation output by the SID its pointer refers to.
#[repr(C)]
struct TokenSid {
    header: SID_AND_ATTRIBUTES,
    storage: UserSid,
}

impl TokenSid {
    fn read(&mut self, token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Option<PSID> {
        let mut used = 0;
        let ok = unsafe {
            GetTokenInformation(
                token,
                class,
                (&raw mut *self).cast(),
                size_of::<Self>() as u32,
                &mut used,
            )
        };
        (ok != 0 && !self.header.Sid.is_null() && unsafe { IsValidSid(self.header.Sid) } != 0)
            .then_some(self.header.Sid)
    }
}

pub(crate) struct ProcessAccess {
    integrity: u32,
    user: UserSid,
}

impl ProcessAccess {
    pub(crate) fn current() -> Option<&'static Self> {
        static CURRENT: OnceLock<Option<ProcessAccess>> = OnceLock::new();
        CURRENT
            .get_or_init(|| Self::query(unsafe { GetCurrentProcess() }))
            .as_ref()
    }

    pub(crate) fn query(process: HANDLE) -> Option<Self> {
        let mut protection = PROCESS_PROTECTION_LEVEL_INFORMATION::default();
        if unsafe {
            GetProcessInformation(
                process,
                ProcessProtectionLevelInfo,
                (&raw mut protection).cast(),
                size_of_val(&protection) as u32,
            )
        } == 0
            || protection.ProtectionLevel != PROTECTION_LEVEL_NONE
        {
            return None;
        }

        let mut token = null_mut();
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut buffer = TokenSid {
            header: SID_AND_ATTRIBUTES::default(),
            storage: [0; SECURITY_MAX_SID_SIZE as usize],
        };
        let label = buffer.read(token.as_raw_handle(), TokenIntegrityLevel)?;
        let last = unsafe { *GetSidSubAuthorityCount(label) }.checked_sub(1)?;
        let integrity = unsafe { *GetSidSubAuthority(label, u32::from(last)) };

        let sid = buffer.read(token.as_raw_handle(), TokenUser)?;
        let len = unsafe { GetLengthSid(sid) } as usize;
        let mut user = [0; SECURITY_MAX_SID_SIZE as usize];
        user.get_mut(..len)?
            .copy_from_slice(unsafe { std::slice::from_raw_parts(sid.cast(), len) });
        Some(Self { integrity, user })
    }

    pub(super) fn can_tile(&self, target: &Self) -> bool {
        target.integrity <= self.integrity
            && target.integrity < SYSTEM
            && (target.integrity < HIGH || self.user == target.user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::SystemServices::SECURITY_MANDATORY_MEDIUM_RID;
    const MEDIUM: u32 = SECURITY_MANDATORY_MEDIUM_RID as u32;

    fn process(integrity: u32, account: u8) -> ProcessAccess {
        ProcessAccess {
            integrity,
            user: [account; SECURITY_MAX_SID_SIZE as usize],
        }
    }

    #[test]
    fn elevation_controls_which_windows_can_tile() {
        let normal = process(MEDIUM, 1);
        let admin = process(HIGH, 1);
        assert!(normal.can_tile(&normal));
        assert!(!normal.can_tile(&admin));
        assert!(admin.can_tile(&normal));
        assert!(admin.can_tile(&admin));
        assert!(!admin.can_tile(&process(HIGH, 2)));
        assert!(!admin.can_tile(&process(SYSTEM, 1)));
    }

    #[test]
    fn can_inspect_own_primary_process_token() {
        let current = ProcessAccess::current().expect("current process token");
        assert_ne!(current.integrity, 0);
        assert!(current.can_tile(current));
    }
}
