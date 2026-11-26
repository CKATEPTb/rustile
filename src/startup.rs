pub(crate) mod consent;
mod protected;
mod scheduler;

use crate::i18n::{Msg, render, tr};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    },
};
use windows_sys::core::{PCWSTR, w};

pub(crate) const LEGACY_ELEVATED_FLAG: &str = "--startup-elevated";
pub(crate) const STARTUP_FLAG: &str = "--startup";
const RUN: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const APPROVAL: PCWSTR =
    w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
const NAME: PCWSTR = w!("Rustile");
const COMMAND_LIMIT: usize = 260;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Startup {
    #[default]
    Off,
    Normal,
    Elevated,
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|err| render(Msg::SystemExePath, &[("detail", &err.to_string())]))
}

fn registry_error(status: u32) -> String {
    render(
        Msg::SystemStartup,
        &[(
            "detail",
            &io::Error::from_raw_os_error(status as i32).to_string(),
        )],
    )
}

fn missing(status: u32) -> bool {
    matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    flag: &'static str,
}

fn command(entry: &Entry) -> Result<Vec<u16>, String> {
    let mut text = OsString::from("\"");
    text.push(&entry.path);
    text.push("\"");
    if !entry.flag.is_empty() {
        text.push(" ");
        text.push(entry.flag);
    }
    let mut wide: Vec<_> = text.encode_wide().collect();
    if wide.len() > COMMAND_LIMIT {
        return Err(tr(Msg::SystemLongPath).into());
    }
    wide.push(0);
    Ok(wide)
}

// Previous releases also wrote a bare path and an opt-in flag which used to
// elevate on every login. Both now represent ordinary startup.
fn decode(value: &[u16], exe: &Path) -> Result<Entry, String> {
    if value.contains(&0) {
        return Err(tr(Msg::SystemOccupied).into());
    }
    let quoted = value
        .strip_prefix(&[b'"' as u16])
        .ok_or(tr(Msg::SystemOccupied))?;
    let end = quoted
        .iter()
        .position(|&c| c == b'"' as u16)
        .ok_or(tr(Msg::SystemOccupied))?;
    let stored = PathBuf::from(OsString::from_wide(&quoted[..end]));
    if !stored.is_absolute()
        || stored
            .file_name()
            .zip(exe.file_name())
            .is_none_or(|(a, b)| !a.eq_ignore_ascii_case(b))
    {
        return Err(tr(Msg::SystemOccupied).into());
    }
    let suffix = OsString::from_wide(&quoted[end + 1..]);
    let flag = if suffix.is_empty() {
        ""
    } else if suffix == OsStr::new(&format!(" {LEGACY_ELEVATED_FLAG}")) {
        LEGACY_ELEVATED_FLAG
    } else if suffix == OsStr::new(&format!(" {STARTUP_FLAG}")) {
        STARTUP_FLAG
    } else {
        return Err(tr(Msg::SystemOccupied).into());
    };
    Ok(Entry { path: stored, flag })
}

fn registration(subkey: PCWSTR, exe: &Path) -> Result<Option<Entry>, String> {
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
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(registry_error(status));
    }
    let value = &data[..bytes as usize / 2];
    decode(
        value.strip_suffix(&[0]).ok_or(tr(Msg::SystemOccupied))?,
        exe,
    )
    .map(Some)
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

struct RunRegistration {
    run: PCWSTR,
    approval: PCWSTR,
    exe: PathBuf,
    protected: PathBuf,
}

impl RunRegistration {
    fn for_user(sid: &str) -> Result<Self, String> {
        Ok(Self {
            run: RUN,
            approval: APPROVAL,
            exe: current_exe()?,
            protected: protected::exe_path(sid)?,
        })
    }

    fn owned(&self, entry: &Entry) -> bool {
        let eq = |a: &Path, b: &Path| a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
        // decode already verified the executable's name. The protected copy
        // also manages the ordinary registration of the portable Rustile.
        eq(&entry.path, &self.exe)
            || eq(&entry.path, &self.protected)
            || eq(&self.exe, &self.protected)
    }

    fn read(&self, elevated: bool) -> Result<Startup, String> {
        if elevated {
            return Ok(Startup::Elevated);
        }
        let entry = registration(self.run, &self.exe)?;
        if entry.as_ref().is_some_and(|e| self.owned(e)) && windows_allows(self.approval)? {
            Ok(Startup::Normal)
        } else {
            Ok(Startup::Off)
        }
    }

    fn replace(&self, previous: Option<&Entry>, next: Option<&Entry>) -> Result<(), String> {
        if previous == next {
            return Ok(());
        }
        // A second copy or Windows Settings can change the value during UAC.
        // Avoid deleting or rolling back another writer's choice.
        if registration(self.run, &self.exe)?.as_ref() != previous {
            return Err(tr(Msg::SystemStartupChanged).into());
        }
        let data = next.map(command).transpose()?;
        crate::profile_io::with_user(|| {
            let status = if let Some(data) = data {
                unsafe {
                    RegSetKeyValueW(
                        HKEY_CURRENT_USER,
                        self.run,
                        NAME,
                        REG_SZ,
                        data.as_ptr().cast(),
                        size_of_val(data.as_slice()) as u32,
                    )
                }
            } else {
                unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, self.run, NAME) }
            };
            if status == ERROR_SUCCESS || (next.is_none() && missing(status)) {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(status as i32))
            }
        })
        .map_err(|e| render(Msg::SystemStartup, &[("detail", &e.to_string())]))
    }

    fn change(
        &self,
        mode: Startup,
        installed: impl Fn() -> Result<bool, String>,
        configure: impl Fn(bool) -> Result<bool, String>,
    ) -> Result<Startup, String> {
        let elevated = installed()?;
        if elevated && mode == Startup::Elevated {
            return Ok(Startup::Elevated);
        }
        let before = registration(self.run, &self.exe)?;
        let entry = before.as_ref();
        let owned = entry.filter(|e| self.owned(e));
        let normal = Entry {
            path: self.exe.clone(),
            flag: STARTUP_FLAG,
        };
        match mode {
            Startup::Elevated => {
                // The administrator switch belongs to an enabled normal entry.
                // A stale flyout must respect a subsequent Windows opt-out.
                let old = owned.ok_or(tr(Msg::SystemEnableFirst))?;
                if !windows_allows(self.approval)? {
                    return Err(tr(Msg::SystemEnableStartup).into());
                }
                // Upgrade old bare Run commands before asking. Even if the
                // process stops between task registration and cleanup, the
                // guarded normal launcher defers to the accepted task.
                self.replace(Some(old), Some(&normal))?;
                if !configure(true)? {
                    return self.read(installed()?);
                }
                if !installed()? {
                    return Err(tr(Msg::SystemCreateTask).into());
                }
                self.replace(Some(&normal), None)?;
            }
            Startup::Normal => {
                let allowed = windows_allows(self.approval)?;
                if elevated && !allowed {
                    return Err(tr(Msg::SystemEnableStartup).into());
                }
                // Prepare ordinary startup before removing a working task.
                self.replace(entry, Some(&normal))?;
                if !allowed {
                    // Leave the item visible in Windows Startup Apps for the
                    // user who explicitly tried to enable ordinary startup.
                    return Err(tr(Msg::SystemEnableStartup).into());
                }
                if elevated {
                    let result = configure(false);
                    if installed()? {
                        self.replace(Some(&normal), entry)?;
                        return match result {
                            Ok(false) => Ok(Startup::Elevated),
                            Ok(true) => Err(tr(Msg::SystemDeleteTask).into()),
                            Err(err) => Err(err),
                        };
                    }
                    result?;
                }
            }
            Startup::Off => {
                if elevated && !configure(false)? {
                    return self.read(installed()?);
                }
                if elevated && installed()? {
                    return Err(tr(Msg::SystemDeleteTask).into());
                }
                if owned.is_some() {
                    self.replace(entry, None)?;
                }
            }
        }
        self.read(installed()?)
    }
}

fn task_installed(sid: &str) -> Result<bool, String> {
    #[cfg(test)]
    if let Some(mock) = SMOKE_TASK.get() {
        return Ok(mock.installed.load(std::sync::atomic::Ordering::SeqCst));
    }
    scheduler::installed(sid)
}

fn configure_task(enable: bool) -> Result<bool, String> {
    #[cfg(test)]
    if let Some(mock) = SMOKE_TASK.get() {
        use std::sync::atomic::Ordering;
        if enable && mock.cancel_first.swap(false, Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(600));
            return Ok(false);
        }
        mock.installed.store(enable, Ordering::SeqCst);
        return Ok(true);
    }
    consent::configure(enable)
}

impl Startup {
    pub fn read() -> Result<Self, String> {
        let sid = consent::sid()?;
        RunRegistration::for_user(&sid)?.read(task_installed(&sid)?)
    }

    pub fn apply(self) -> Result<Self, String> {
        let sid = consent::sid()?;
        RunRegistration::for_user(&sid)?.change(self, || task_installed(&sid), configure_task)
    }
}

/// Both the new HKCU command and the legacy flag use this path. Neither
/// calls runas. A task launches the protected executable without this flag.
pub(crate) fn run_at_logon() -> Result<bool, String> {
    let sid = consent::sid()?;
    if RunRegistration::for_user(&sid)?.read(false)? != Startup::Normal {
        return Ok(false);
    }
    // A disabled or unavailable Task Scheduler must not prevent an ordinary
    // Run entry from starting. The singleton still guards the session.
    match task_installed(&sid) {
        Ok(installed) => Ok(!installed),
        Err(detail) => {
            eprintln!("Rustile: {detail}");
            Ok(true)
        }
    }
}

#[cfg(test)]
struct SmokeTask {
    installed: std::sync::atomic::AtomicBool,
    cancel_first: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
static SMOKE_TASK: std::sync::OnceLock<SmokeTask> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn mock_task_for_smoke() {
    assert!(
        SMOKE_TASK
            .set(SmokeTask {
                installed: false.into(),
                cancel_first: true.into(),
            })
            .is_ok()
    );
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

    struct Tasks {
        active: std::cell::Cell<bool>,
        cancel: std::cell::Cell<bool>,
        calls: std::cell::Cell<usize>,
    }

    impl Tasks {
        fn new() -> Self {
            Self {
                active: false.into(),
                cancel: false.into(),
                calls: 0.into(),
            }
        }

        fn apply(&self, run: &RunRegistration, mode: Startup) -> Result<Startup, String> {
            run.change(
                mode,
                || Ok(self.active.get()),
                |enable| {
                    self.calls.set(self.calls.get() + 1);
                    if self.cancel.replace(false) {
                        Ok(false)
                    } else {
                        self.active.set(enable);
                        Ok(true)
                    }
                },
            )
        }
    }

    fn fixture(run: &TestKey, approval: &TestKey, path: &str) -> RunRegistration {
        RunRegistration {
            run: run.ptr(),
            approval: approval.ptr(),
            exe: PathBuf::from(path),
            protected: PathBuf::from(r"C:\Program Files\Rustile\test-user\rustile.exe"),
        }
    }

    #[test]
    fn legacy_normal_and_elevated_flags_require_new_consent_and_never_launch_together() {
        let run = TestKey::new("Run");
        let approval = TestKey::new("Approval");
        let store = fixture(&run, &approval, r"C:\Два окна & Co\rustile.exe");
        let tasks = Tasks::new();
        for flag in ["", LEGACY_ELEVATED_FLAG] {
            let legacy = Entry {
                path: store.exe.clone(),
                flag,
            };
            store.replace(None, Some(&legacy)).unwrap();
            assert_eq!(store.read(false).unwrap(), Startup::Normal);
            tasks.cancel.set(true);
            assert_eq!(
                tasks.apply(&store, Startup::Elevated).unwrap(),
                Startup::Normal
            );
            let guarded = registration(run.ptr(), &store.exe).unwrap().unwrap();
            assert_eq!(guarded.flag, STARTUP_FLAG);
            assert_eq!(
                tasks.apply(&store, Startup::Elevated).unwrap(),
                Startup::Elevated
            );
            assert!(registration(run.ptr(), &store.exe).unwrap().is_none());
            tasks.cancel.set(true);
            assert_eq!(
                tasks.apply(&store, Startup::Normal).unwrap(),
                Startup::Elevated
            );
            assert!(registration(run.ptr(), &store.exe).unwrap().is_none());
            tasks.cancel.set(true);
            assert_eq!(
                tasks.apply(&store, Startup::Off).unwrap(),
                Startup::Elevated
            );
            assert_eq!(
                tasks.apply(&store, Startup::Normal).unwrap(),
                Startup::Normal
            );
            assert_eq!(registration(run.ptr(), &store.exe).unwrap(), Some(guarded));
            assert_eq!(tasks.apply(&store, Startup::Off).unwrap(), Startup::Off);
        }
    }

    #[test]
    fn helper_exit_alone_cannot_change_the_accepted_mode() {
        let run = TestKey::new("Run");
        let approval = TestKey::new("Approval");
        let store = fixture(&run, &approval, r"C:\Work\rustile.exe");
        let tasks = Tasks::new();
        tasks.apply(&store, Startup::Normal).unwrap();
        assert!(
            store
                .change(Startup::Elevated, || Ok(false), |_| Ok(true))
                .is_err()
        );
        assert_eq!(store.read(false).unwrap(), Startup::Normal);

        tasks.apply(&store, Startup::Elevated).unwrap();
        assert!(
            store
                .change(Startup::Normal, || Ok(true), |_| Ok(true))
                .is_err()
        );
        assert_eq!(store.read(true).unwrap(), Startup::Elevated);
        assert!(registration(run.ptr(), &store.exe).unwrap().is_none());
    }

    #[test]
    fn protected_copy_and_project_manage_the_same_startup_after_downgrade() {
        let run = TestKey::new("Run");
        let approval = TestKey::new("Approval");
        let project = fixture(&run, &approval, r"C:\Work\rustile.exe");
        let mut protected = fixture(&run, &approval, "unused");
        protected.exe = project.protected.clone();
        let tasks = Tasks::new();
        tasks.apply(&project, Startup::Normal).unwrap();
        assert_eq!(protected.read(false).unwrap(), Startup::Normal);
        tasks.apply(&project, Startup::Elevated).unwrap();
        tasks.apply(&protected, Startup::Normal).unwrap();
        assert_eq!(project.read(false).unwrap(), Startup::Normal);
        tasks.apply(&project, Startup::Off).unwrap();
        assert_eq!(protected.read(false).unwrap(), Startup::Off);
    }

    #[test]
    fn normal_move_foreign_command_and_length_limit() {
        let run = TestKey::new("Run");
        let approval = TestKey::new("Approval");
        let old = fixture(&run, &approval, r"C:\Два окна & Co\rustile.exe");
        let moved = fixture(&run, &approval, r"C:\Another directory\rustile.exe");
        let tasks = Tasks::new();
        assert_eq!(tasks.apply(&old, Startup::Normal).unwrap(), Startup::Normal);
        assert_eq!(moved.read(false).unwrap(), Startup::Off);
        tasks.apply(&moved, Startup::Normal).unwrap();
        assert_eq!(old.read(false).unwrap(), Startup::Off);
        tasks.apply(&moved, Startup::Off).unwrap();
        tasks.apply(&moved, Startup::Off).unwrap();

        let other = fixture(&run, &approval, r"C:\Somewhere\other.exe");
        tasks.apply(&other, Startup::Normal).unwrap();
        assert!(tasks.apply(&old, Startup::Normal).is_err());
        assert!(tasks.apply(&old, Startup::Off).is_err());
        let long = fixture(
            &run,
            &approval,
            &format!(r"C:\{}\other.exe", "a".repeat(250)),
        );
        assert!(tasks.apply(&long, Startup::Normal).is_err());
        assert_eq!(other.read(false).unwrap(), Startup::Normal);
        assert_eq!(tasks.calls.get(), 0);
    }

    #[test]
    fn externally_disabled_entry_stays_under_windows_control() {
        let run = TestKey::new("Run");
        let approval = TestKey::new("Approval");
        let store = fixture(&run, &approval, r"C:\Program Files\Rustile\rustile.exe");
        let tasks = Tasks::new();
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
            tasks.apply(&store, Startup::Normal).unwrap_err(),
            tr(Msg::SystemEnableStartup)
        );
        assert_eq!(store.read(false).unwrap(), Startup::Off);
        assert_eq!(
            tasks.apply(&store, Startup::Elevated).unwrap_err(),
            tr(Msg::SystemEnableStartup)
        );
        assert_eq!(tasks.calls.get(), 0);
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, approval.ptr(), NAME) };
        assert_eq!(store.read(false).unwrap(), Startup::Normal);
    }
}
