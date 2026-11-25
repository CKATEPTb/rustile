use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::{
    ffi::{OsStrExt, OsStringExt},
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::{
    Foundation::{
        ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
        RPC_E_CHANGED_MODE,
    },
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
            RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::{
        Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};
use windows_sys::core::{PCWSTR, w};

pub(crate) const ELEVATED_FLAG: &str = "--startup-elevated";
const RUN: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const APPROVAL: PCWSTR =
    w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
const NAME: PCWSTR = w!("Rustile");
const COMMAND_LIMIT: usize = 260;
const FOREIGN: &str = "Имя Rustile в автозагрузке Windows уже занято другой командой.";
const DISABLED: &str =
    "Включите Rustile в параметрах Windows в разделе «Приложения → Автозагрузка».";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Startup {
    #[default]
    Off,
    Normal,
    Elevated,
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|err| format!("Не удалось определить путь к Rustile: {err}"))
}

fn registry_error(status: u32) -> String {
    format!(
        "Автозагрузка Rustile: {}",
        io::Error::from_raw_os_error(status as i32)
    )
}

fn missing(status: u32) -> bool {
    matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
}

fn command(exe: &Path, mode: Startup) -> Result<Vec<u16>, String> {
    let mut text = OsString::from("\"");
    text.push(exe);
    text.push("\"");
    if mode == Startup::Elevated {
        text.push(" ");
        text.push(ELEVATED_FLAG);
    }
    let mut wide: Vec<_> = text.encode_wide().collect();
    if wide.len() > COMMAND_LIMIT {
        return Err("Путь к Rustile слишком длинный для автозагрузки Windows.".into());
    }
    wide.push(0);
    Ok(wide)
}

// Accept only the exact two command shapes this application writes. An entry
// from a moved Rustile executable can be replaced on an explicit enable.
fn decode(value: &[u16], exe: &Path) -> Result<Startup, String> {
    if value.contains(&0) {
        return Err(FOREIGN.into());
    }
    let quoted = value.strip_prefix(&[b'"' as u16]).ok_or(FOREIGN)?;
    let end = quoted
        .iter()
        .position(|&c| c == b'"' as u16)
        .ok_or(FOREIGN)?;
    let stored = PathBuf::from(OsString::from_wide(&quoted[..end]));
    if !stored.is_absolute()
        || stored
            .file_name()
            .zip(exe.file_name())
            .is_none_or(|(a, b)| !a.eq_ignore_ascii_case(b))
    {
        return Err(FOREIGN.into());
    }
    let suffix = OsString::from_wide(&quoted[end + 1..]);
    let mode = if suffix.is_empty() {
        Startup::Normal
    } else if suffix == OsStr::new(&format!(" {ELEVATED_FLAG}")) {
        Startup::Elevated
    } else {
        return Err(FOREIGN.into());
    };
    Ok(
        if stored.as_os_str().eq_ignore_ascii_case(exe.as_os_str()) {
            mode
        } else {
            Startup::Off
        },
    )
}

fn registration(subkey: PCWSTR, exe: &Path) -> Result<Startup, String> {
    let mut data = [0u16; COMMAND_LIMIT + 1];
    let mut bytes = size_of_val(&data) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            NAME,
            RRF_RT_REG_SZ | RRF_NOEXPAND,
            null_mut(),
            data.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if missing(status) {
        return Ok(Startup::Off);
    }
    if status != ERROR_SUCCESS {
        return Err(registry_error(status));
    }
    let value = &data[..bytes as usize / 2];
    decode(value.strip_suffix(&[0]).ok_or(FOREIGN)?, exe)
}

// Explorer can independently disable a Run entry in Task Manager. Only read
// the marker; its absence is the normal state on first registration.
fn windows_allows(subkey: PCWSTR) -> Result<bool, String> {
    let mut marker = [0u8; 12];
    let mut bytes = marker.len() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            NAME,
            RRF_RT_REG_BINARY,
            null_mut(),
            marker.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if missing(status) {
        return Ok(true);
    }
    if status != ERROR_SUCCESS {
        return Err(registry_error(status));
    }
    Ok(bytes >= 4 && marker[0] & 1 == 0)
}

impl Startup {
    pub fn read() -> Result<Self, String> {
        Self::read_at(RUN, APPROVAL, &current_exe()?)
    }

    fn read_at(run: PCWSTR, approval: PCWSTR, exe: &Path) -> Result<Self, String> {
        let mode = registration(run, exe)?;
        Ok(if mode != Self::Off && windows_allows(approval)? {
            mode
        } else {
            Self::Off
        })
    }

    pub fn apply(self) -> Result<(), String> {
        self.apply_at(RUN, APPROVAL, &current_exe()?)
    }

    fn apply_at(self, run: PCWSTR, approval: PCWSTR, exe: &Path) -> Result<(), String> {
        let previous = registration(run, exe)?;
        if self == Self::Off {
            if previous != Self::Off {
                let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, run, NAME) };
                if status != ERROR_SUCCESS && !missing(status) {
                    return Err(registry_error(status));
                }
            }
            return Ok(());
        }
        let allowed = windows_allows(approval)?;
        if self != previous {
            let data = command(exe, self)?;
            let status = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    run,
                    NAME,
                    REG_SZ,
                    data.as_ptr().cast(),
                    size_of_val(data.as_slice()) as u32,
                )
            };
            if status != ERROR_SUCCESS {
                return Err(registry_error(status));
            }
        }
        // Keep the opted-in Run item visible in Windows Settings, so the
        // user can restore its separate OS-level permission there.
        if allowed {
            Ok(())
        } else {
            Err(DISABLED.into())
        }
    }
}

fn is_elevated() -> io::Result<bool> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut bytes = 0;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of_val(&elevation) as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(elevation.TokenIsElevated != 0)
}

// Only the opt-in login command takes this path, before acquiring the instance
// lock. The consented child has no arguments, so it cannot request UAC again.
pub(crate) fn launch_elevated() -> Result<bool, String> {
    if is_elevated().map_err(|err| format!("Не удалось проверить права Rustile: {err}"))?
    {
        return Ok(false);
    }
    let exe: Vec<_> = current_exe()?
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    let apartment = unsafe {
        CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if apartment < 0 && apartment != RPC_E_CHANGED_MODE {
        return Err(format!(
            "Не удалось подготовить запуск Rustile ({apartment:#x})."
        ));
    }
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: exe.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    let error = (unsafe { ShellExecuteExW(&mut launch) } == 0).then(io::Error::last_os_error);
    if apartment >= 0 {
        unsafe { CoUninitialize() };
    }
    match error {
        None => Ok(true),
        Some(err) if err.raw_os_error() == Some(ERROR_CANCELLED as i32) => Ok(false),
        Some(err) => Err(format!(
            "Не удалось запустить Rustile от имени администратора: {err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::System::Registry::{REG_BINARY, RegDeleteKeyW};

    // Sibling test-only keys under HKCU\Software. Never use Explorer's real
    // Run or StartupApproved keys, even if a test assertion aborts the test.
    struct TestKey(Vec<u16>);

    impl TestKey {
        fn new(kind: &str) -> Self {
            let clock = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let name = format!(
                r"Software\Rustile.Test.{}.{}.{kind}",
                std::process::id(),
                clock
            );
            Self(name.encode_utf16().chain([0]).collect())
        }
        fn ptr(&self) -> PCWSTR {
            self.0.as_ptr()
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, self.ptr()) };
        }
    }

    #[test]
    fn register_switch_move_disable_and_preserve_foreign_command() {
        let run = TestKey::new("Run");
        let approval = TestKey::new("Approval");
        let exe = Path::new(r"C:\Два окна & Co\rustile.exe");
        let moved = Path::new(r"C:\Another directory\rustile.exe");
        let read = |p| Startup::read_at(run.ptr(), approval.ptr(), p);
        let apply = |mode: Startup, p| mode.apply_at(run.ptr(), approval.ptr(), p);
        assert_eq!(read(exe).unwrap(), Startup::Off);
        for mode in [Startup::Normal, Startup::Elevated, Startup::Normal] {
            apply(mode, exe).unwrap();
            apply(mode, exe).unwrap();
            assert_eq!(read(exe).unwrap(), mode);
        }
        assert_eq!(read(moved).unwrap(), Startup::Off);
        apply(Startup::Elevated, moved).unwrap();
        assert_eq!(read(moved).unwrap(), Startup::Elevated);
        assert_eq!(read(exe).unwrap(), Startup::Off);
        apply(Startup::Off, moved).unwrap();
        apply(Startup::Off, moved).unwrap();
        assert_eq!(read(moved).unwrap(), Startup::Off);

        let other = Path::new(r"C:\Somewhere\other.exe");
        apply(Startup::Normal, other).unwrap();
        assert!(apply(Startup::Normal, exe).is_err());
        assert!(apply(Startup::Off, exe).is_err());
        assert_eq!(read(other).unwrap(), Startup::Normal);
        let long_path = PathBuf::from(format!(r"C:\{}\other.exe", "a".repeat(250)));
        assert!(apply(Startup::Elevated, &long_path).is_err());
        assert_eq!(read(other).unwrap(), Startup::Normal);
    }

    #[test]
    fn externally_disabled_entry_stays_under_windows_control() {
        let run = TestKey::new("Run");
        let approval = TestKey::new("Approval");
        let exe = Path::new(r"C:\Program Files\Rustile\rustile.exe");
        let mut marker = [0u8; 12];
        marker[0] = 3;
        assert_eq!(
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    approval.ptr(),
                    NAME,
                    REG_BINARY,
                    marker.as_ptr().cast(),
                    marker.len() as u32,
                )
            },
            ERROR_SUCCESS
        );
        assert_eq!(
            Startup::Normal
                .apply_at(run.ptr(), approval.ptr(), exe)
                .unwrap_err(),
            DISABLED
        );
        assert_eq!(registration(run.ptr(), exe).unwrap(), Startup::Normal);
        assert_eq!(
            Startup::read_at(run.ptr(), approval.ptr(), exe).unwrap(),
            Startup::Off
        );
        assert!(!windows_allows(approval.ptr()).unwrap());
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, approval.ptr(), NAME) };
        assert_eq!(
            Startup::read_at(run.ptr(), approval.ptr(), exe).unwrap(),
            Startup::Normal
        );
    }
}
