//! Private-desktop installation target used by the Windows smoke harness.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

const PREFIX: &str = "rustile-smoke-";
const INVALID_FIXTURE: &str =
    "The installation fixture requires its private smoke desktop and temporary directory.";

fn valid_fixture_dir(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|guid| {
        guid.len() == 36
            && guid.bytes().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => b == b'-',
                _ => b.is_ascii_hexdigit(),
            })
    })
}

pub(super) fn destination() -> Result<PathBuf, String> {
    let desktop =
        crate::native::desktops::object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })
            .map_err(|e| e.to_string())?;
    if !desktop
        .strip_prefix(PREFIX)
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        || env::var("RUSTILE_SMOKE_DESKTOP").as_deref() != Ok(&desktop)
    {
        return Err(INVALID_FIXTURE.into());
    }

    let program_files = env::var_os("RUSTILE_FIXTURE_PROGRAM_FILES")
        .map(PathBuf::from)
        .ok_or(INVALID_FIXTURE)?;
    let root = program_files.parent().ok_or(INVALID_FIXTURE)?;
    let temp = fs::canonicalize(env::temp_dir()).map_err(|e| e.to_string())?;
    if !program_files.is_absolute()
        || program_files.file_name() != Some("Program Files".as_ref())
        || !root
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(valid_fixture_dir)
    {
        return Err(INVALID_FIXTURE.into());
    }
    let expected_root = temp.join(root.file_name().ok_or(INVALID_FIXTURE)?);
    let actual = fs::canonicalize(&program_files).map_err(|e| e.to_string())?;
    let source = env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| e.to_string())?;
    if actual != expected_root.join("Program Files") || !source.starts_with(&expected_root) {
        return Err(INVALID_FIXTURE.into());
    }
    Ok(program_files
        .join("CKATEPTb")
        .join("Rustile")
        .join("rustile.exe"))
}

fn ensure_fixture_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        result => result?,
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(INVALID_FIXTURE));
    }
    Ok(())
}

pub(super) fn publish(source: &Path, dest: &Path) -> Result<(), String> {
    if dest != destination()? {
        return Err(INVALID_FIXTURE.into());
    }
    let install = || -> io::Result<()> {
        if fs::canonicalize(source)? != fs::canonicalize(env::current_exe()?)? {
            return Err(io::Error::other(INVALID_FIXTURE));
        }
        let dir = dest
            .parent()
            .ok_or_else(|| io::Error::other(INVALID_FIXTURE))?;
        let vendor = dir
            .parent()
            .ok_or_else(|| io::Error::other(INVALID_FIXTURE))?;
        ensure_fixture_directory(vendor)?;
        ensure_fixture_directory(dir)?;
        let mut incoming = File::open(source)?;
        let stage = dir.join(format!("{}.pending", std::process::id()));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)?;
        let copied = io::copy(&mut incoming, &mut output).and_then(|_| output.sync_all());
        drop(output);
        let result = copied.and_then(|()| crate::startup::replace_staged_exe(&stage, dest));
        if result.is_err() {
            let _ = fs::remove_file(stage);
        }
        result
    };
    install().map_err(|e| e.to_string())
}
