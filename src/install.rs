//! Installs or replaces the single executable and forwards the original invocation.
//!
//! A copy launched outside the installation directory enters the update path,
//! even for help or version requests. The installer serializes replacements,
//! registers the CLI on PATH and hands control to the installed copy.
//! The caller retains its own token when an elevated helper performs the copy.

#[cfg(feature = "installation-fixture")]
mod fixture;
mod path_env;

use crate::{cli, cli_error, launch_context::PACKAGE_ORIGIN, runtime, startup};
use startup::consent;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const INSTALL: &str = "--install-application";
const UPDATED_ELEVATED_OWNER: u8 = 3;

struct Destination {
    exe: PathBuf,
    fixture: bool,
}

impl Destination {
    fn resolve() -> Result<Self, String> {
        #[cfg(feature = "installation-fixture")]
        let exe = fixture::destination()?;
        #[cfg(not(feature = "installation-fixture"))]
        let exe = startup::installed_exe()?;
        Ok(Self {
            exe,
            fixture: cfg!(feature = "installation-fixture"),
        })
    }

    fn directory(&self) -> &Path {
        self.exe
            .parent()
            .expect("the installed executable has a parent")
    }

    fn registered(&self) -> Result<bool, String> {
        Ok(self.fixture || path_env::registered(self.directory()).map_err(cli_error::io)?)
    }

    fn install(
        &self,
        source: &Path,
        previous: Option<&runtime::InstallOwner>,
        logon: bool,
    ) -> Result<bool, String> {
        #[cfg(feature = "installation-fixture")]
        if self.fixture {
            fixture::publish(source, &self.exe)?;
            return Ok(false);
        }
        let sid = consent::sid()?;
        let stopped = if consent::elevated()? {
            install_elevated(&sid, 0)?
        } else {
            let pid = previous
                .filter(|owner| owner.elevated && !logon)
                .map_or(0, |owner| owner.pid);
            match consent::run_elevated(&format!("{INSTALL} {sid} {pid}"), true)? {
                Some(0) => false,
                Some(code) if code == u32::from(UPDATED_ELEVATED_OWNER) => true,
                None => return Err("Installation cancelled. Administrator approval is required to install Rustile in Program Files and register its CLI.".into()),
                Some(code) => return Err(format!("The Rustile installer exited with code {code}.")),
            }
        };
        startup::migrate_user_startup(source)?;
        if let Some(origin) = std::env::var_os(PACKAGE_ORIGIN).map(PathBuf::from)
            && origin.is_absolute()
            && origin != source
        {
            startup::migrate_user_startup(&origin)?;
        }
        if let Some(owner) = previous
            && owner.image != source
            && owner.image != self.exe
        {
            startup::migrate_user_startup(&owner.image)?;
        }
        Ok(stopped)
    }
}

// The privileged helper only installs its own image at the fixed system
// location. The original process retains its token, arguments and stdio.
fn install_elevated(original_sid: &str, prior_pid: u32) -> Result<bool, String> {
    if cfg!(feature = "installation-fixture") || !consent::elevated()? {
        return Err("The machine installer requires administrator approval.".into());
    }
    let exe = startup::installed_exe()?;
    startup::install_current_exe()?;
    path_env::register(exe.parent().unwrap()).map_err(cli_error::io)?;
    if consent::sid()? == original_sid {
        startup::migrate_elevated_startup()?;
        // An ordinary launcher cannot send quit to a high-integrity owner.
        // Only address the same elevated tray observed before this UAC prompt.
        if prior_pid != 0
            && runtime::owner_image().is_some_and(|owner| owner.pid == prior_pid && owner.elevated)
        {
            runtime::quit_owner(prior_pid)?;
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn handle_helper(args: &[String]) -> Option<ExitCode> {
    if args.first().is_none_or(|flag| flag != INSTALL) {
        return None;
    }
    let [_, sid, pid] = args else {
        return Some(ExitCode::FAILURE);
    };
    let Ok(pid) = pid.parse::<u32>() else {
        return Some(ExitCode::FAILURE);
    };
    Some(match install_elevated(sid, pid) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::from(UPDATED_ELEVATED_OWNER),
        Err(detail) => {
            runtime::show_error(&detail);
            ExitCode::FAILURE
        }
    })
}

fn at_destination(source: &Path, destination: &Path) -> bool {
    source == destination
        || std::fs::canonicalize(source)
            .ok()
            .zip(std::fs::canonicalize(destination).ok())
            .is_some_and(|(a, b)| a == b)
}

fn start_quietly(exe: &Path, args: &[String]) -> Result<(), String> {
    let result = Command::new(exe)
        .env_remove(PACKAGE_ORIGIN)
        .arg("start")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(cli_error::io)?;
    if result.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&result.stderr).trim().to_owned())
    }
}

pub(crate) fn launch(args: &[String], from_terminal: bool) -> Result<ExitCode, String> {
    #[cfg(feature = "installation-fixture")]
    if matches!(args, [flag] if flag == "--installation-fixture") {
        fixture::destination()?;
        println!("installation-fixture");
        return Ok(ExitCode::SUCCESS);
    }
    let logon = matches!(args, [flag] if matches!(flag.as_str(), startup::STARTUP_FLAG | startup::LEGACY_ELEVATED_FLAG));
    let background = matches!(args, [flag] if flag == "--background");
    let invocation = if logon || background {
        None
    } else {
        Some(cli::parse(args)?)
    };
    let dest = Destination::resolve()?;
    let source = std::env::current_exe().map_err(cli_error::io)?;
    if at_destination(&source, &dest.exe) {
        if !dest.registered()? {
            let _lock = runtime::installation_lock()?;
            if !dest.registered()? {
                dest.install(&source, None, logon)?;
            }
        }
        runtime::run_with_args(args, from_terminal)?;
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = runtime::installation_lock()?;
    let previous = runtime::owner_image();
    let helper_stopped_owner = dest.install(&source, previous.as_ref(), logon)?;
    if logon && !startup::run_at_logon()? {
        return Ok(ExitCode::SUCCESS);
    }
    let was_running = runtime::stop_for_update()? || helper_stopped_owner;
    if logon || background {
        start_quietly(&dest.exe, &[])?;
    } else if !from_terminal
        && matches!(invocation, Some(cli::Invocation::Start(_)))
        && args.first().is_none_or(|arg| arg != "start")
    {
        start_quietly(&dest.exe, args)?;
    } else {
        // Help and version normally do not elect an owner. Preserve an
        // existing session if its binary was just updated by such a call.
        if was_running && matches!(invocation, Some(cli::Invocation::Print(_))) {
            start_quietly(&dest.exe, &[])?;
        }
        let status = Command::new(&dest.exe)
            .env_remove(PACKAGE_ORIGIN)
            .args(args)
            .status()
            .map_err(cli_error::io)?;
        return Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    Ok(ExitCode::SUCCESS)
}
