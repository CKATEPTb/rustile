use crate::i18n::{Msg, render, tr};
use std::{
    ffi::{OsString, c_void},
    fs::{self, File, OpenOptions},
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_INVALID_FUNCTION,
            ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_SHARING_VIOLATION, GENERIC_ALL,
            GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
        },
        Security::{self, Authorization::*},
        Storage::FileSystem::*,
        System::{
            Com::CoTaskMemFree,
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            Memory::*,
        },
        UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath},
    },
    core::{PCWSTR, s, w},
};

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn program_files() -> Result<PathBuf, String> {
    let mut name = null_mut();
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, null_mut(), &mut name) };
    if hr < 0 {
        return Err(render(
            Msg::SystemProgramFiles,
            &[("detail", &format!("{hr:#x}"))],
        ));
    }
    let dir = unsafe {
        let mut len = 0;
        while *name.add(len) != 0 {
            len += 1;
        }
        let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(name, len)));
        CoTaskMemFree(name.cast());
        path
    };
    if !matches!(dir.components().next(), Some(std::path::Component::Prefix(p)) if matches!(p.kind(), std::path::Prefix::Disk(_)))
        || !dir.is_absolute()
    {
        return Err(tr(Msg::SystemLocalFolder).into());
    }
    Ok(dir)
}

pub(crate) fn installed_exe() -> Result<PathBuf, String> {
    Ok(program_files()?
        .join("CKATEPTb")
        .join("Rustile")
        .join("rustile.exe"))
}

pub(super) fn legacy_exe(sid: &str) -> Result<PathBuf, String> {
    let fields = sid.strip_prefix("S-1-").ok_or(tr(Msg::SystemBadSid))?;
    if sid.len() > 184
        || !fields
            .split('-')
            .all(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(tr(Msg::SystemBadSid).into());
    }
    Ok(program_files()?
        .join("Rustile")
        .join(sid)
        .join("rustile.exe"))
}

struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn descriptor(sddl: PCWSTR) -> io::Result<Local> {
    let mut ptr = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, 1, &mut ptr, null_mut())
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Local(ptr))
}

fn attributes(sd: &Local) -> Security::SECURITY_ATTRIBUTES {
    Security::SECURITY_ATTRIBUTES {
        nLength: size_of::<Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    }
}

fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, tr(Msg::SystemUnsafeFolder))
}

// Inspect the object itself, not a junction's target. Owners can change an
// object's DACL, even when its existing access rules appear read-only.
fn check_path(path: &Path, is_dir: bool, ancestor: bool, installer: &Local) -> io::Result<()> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let meta = file.metadata()?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || meta.is_dir() != is_dir {
        return Err(denied());
    }
    let (mut owner, mut acl, mut sd) = (null_mut(), null_mut(), null_mut());
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            Security::OWNER_SECURITY_INFORMATION | Security::DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut sd,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _sd = Local(sd);
    let trusted = |sid| unsafe {
        Security::IsWellKnownSid(sid, Security::WinBuiltinAdministratorsSid) != 0
            || Security::IsWellKnownSid(sid, Security::WinLocalSystemSid) != 0
            || Security::EqualSid(sid, installer.0) != 0
    };
    if owner.is_null()
        || !trusted(owner)
        || acl.is_null()
        || unsafe { Security::IsValidAcl(acl) } == 0
    {
        return Err(denied());
    }
    let mut mutable = GENERIC_ALL
        | GENERIC_WRITE
        | WRITE_DAC
        | WRITE_OWNER
        | DELETE
        | FILE_DELETE_CHILD
        | FILE_WRITE_ATTRIBUTES
        | FILE_WRITE_EA;
    // Windows allows users to create sibling directories in the drive root.
    // They must never be able to rename/delete an existing path component.
    if !ancestor {
        mutable |= FILE_WRITE_DATA | FILE_APPEND_DATA;
    }
    for i in 0..unsafe { (*acl).AceCount } {
        let mut raw = null_mut();
        if unsafe { Security::GetAce(acl, i.into(), &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let head = unsafe { &*raw.cast::<Security::ACE_HEADER>() };
        if head.AceFlags as u32 & Security::INHERIT_ONLY_ACE != 0 || head.AceType == 1 {
            continue;
        }
        if head.AceType != 0 {
            return Err(denied());
        }
        let ace = unsafe { &*raw.cast::<Security::ACCESS_ALLOWED_ACE>() };
        if ace.Mask & mutable != 0 && !trusted((&raw const ace.SidStart).cast_mut().cast()) {
            return Err(denied());
        }
    }
    Ok(())
}

fn secure_directory(dir: &Path, sd: &Local, installer: &Local) -> io::Result<()> {
    if unsafe { CreateDirectoryW(wide(dir).as_ptr(), &attributes(sd)) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    check_path(dir, true, false, installer)
}

// A workspace path can be renamed while UAC is open. Match an immutable
// read handle to the image that actually received consent before copying it.
fn is_running_image(source: &File) -> io::Result<bool> {
    let compare = unsafe {
        GetProcAddress(
            GetModuleHandleW(w!("ntdll.dll")),
            s!("NtAreMappedFilesTheSame"),
        )
    }
    .ok_or_else(|| io::Error::other(tr(Msg::SystemVerifyImage)))?;
    let compare: unsafe extern "system" fn(*const c_void, *const c_void) -> i32 =
        unsafe { std::mem::transmute(compare) };
    let map =
        unsafe { CreateFileMappingW(source.as_raw_handle(), null(), PAGE_READONLY, 0, 0, null()) };
    if map.is_null() {
        return Err(io::Error::last_os_error());
    }
    let map = unsafe { OwnedHandle::from_raw_handle(map) };
    let view = unsafe { MapViewOfFile(map.as_raw_handle(), FILE_MAP_READ, 0, 0, 1) };
    if view.Value.is_null() {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        let same = compare(GetModuleHandleW(null()).cast(), view.Value) == 0;
        UnmapViewOfFile(view);
        Ok(same)
    }
}

fn rename_posix(staging: &Path, exe: &Path) -> io::Result<()> {
    const REPLACE_IF_EXISTS: u32 = 0x1;
    const POSIX_SEMANTICS: u32 = 0x2;
    let name = wide(exe);
    let bytes = size_of::<FILE_RENAME_INFO>() + size_of_val(name.as_slice());
    let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let rename = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    let file = OpenOptions::new().access_mode(DELETE).open(staging)?;
    unsafe {
        (*rename).Anonymous.Flags = REPLACE_IF_EXISTS | POSIX_SEMANTICS;
        (*rename).FileNameLength = ((name.len() - 1) * size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            (&raw mut (*rename).FileName).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfoEx,
            rename.cast(),
            bytes as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Publish a complete stage without ever unlinking the installed name first.
pub(crate) fn replace_staged_exe(staging: &Path, exe: &Path) -> io::Result<()> {
    match rename_posix(staging, exe) {
        Err(e)
            if matches!(
                e.raw_os_error().map(|s| s as u32),
                Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
            ) =>
        {
            // Windows refuses to unlink the *last* name of an executing image.
            // Keep a second link until its section closes. New opens see the
            // replacement atomically while the original process keeps running.
            let mut sequence = 0u32;
            let old = loop {
                let old =
                    exe.with_file_name(format!("{}.{}.retired", std::process::id(), sequence));
                match fs::hard_link(exe, &old) {
                    Ok(()) => break old,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => sequence += 1,
                    Err(_) => return Err(e),
                }
            };
            let published = rename_posix(staging, exe);
            let _ = fs::remove_file(old);
            published
        }
        Err(e)
            if matches!(
                e.raw_os_error().map(|s| s as u32),
                Some(ERROR_INVALID_PARAMETER | ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)
            ) =>
        {
            if unsafe {
                MoveFileExW(
                    wide(staging).as_ptr(),
                    wide(exe).as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        result => result,
    }
}

fn retired_owner(name: &str) -> Option<u32> {
    let (pid, sequence) = name.strip_suffix(".retired")?.split_once('.')?;
    sequence.parse::<u32>().ok()?;
    pid.parse().ok()
}

fn install_at(exe: &Path) -> io::Result<()> {
    let install_dir = exe.parent().unwrap();
    let root = install_dir.parent().unwrap();
    let program_files = root.parent().unwrap();
    let mut installer = null_mut();
    // The Windows servicing account normally owns Program Files.
    if unsafe {
        ConvertStringSidToSidW(
            w!("S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"),
            &mut installer,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let installer = Local(installer);
    for dir in program_files
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        check_path(dir, true, dir != program_files, &installer)?;
    }
    let dir_sd = descriptor(w!(
        "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)"
    ))?;
    secure_directory(root, &dir_sd, &installer)?;
    secure_directory(install_dir, &dir_sd, &installer)?;
    // The scheduler uses this directory as its working directory. Accept
    // only our executable, stages, and previous image links.
    let staging = install_dir.join(format!("{}.pending", std::process::id()));
    for entry in fs::read_dir(install_dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let pending_pid = name
            .strip_suffix(".pending")
            .and_then(|s| s.parse::<u32>().ok());
        let retired = retired_owner(name);
        if !path.as_os_str().eq_ignore_ascii_case(exe.as_os_str())
            && pending_pid.is_none()
            && retired.is_none()
        {
            return Err(denied());
        }
        if let Err(error) = check_path(&path, false, false, &installer)
            && !((pending_pid.is_some() || retired.is_some())
                && error.kind() == io::ErrorKind::NotFound)
        {
            return Err(error);
        }
        // A live publisher needs its link until the stage takes the target
        // name. An older process can still be mapped; retry cleanup next time.
        if let Some(pid) = retired
            && !install_dir.join(format!("{pid}.pending")).try_exists()?
        {
            let _ = fs::remove_file(path);
        }
    }
    let source_path = std::env::current_exe()?;
    let mut source = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&source_path)?;
    if !is_running_image(&source)? {
        return Err(denied());
    }
    if source_path
        .as_os_str()
        .eq_ignore_ascii_case(exe.as_os_str())
    {
        // The immutable source handle already matched the loaded image.
        return Ok(());
    }

    // A stage for this PID can survive an interrupted install. Concurrent
    // helpers have independent stages, so only complete files can be renamed.
    match fs::remove_file(&staging) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let file_sd = descriptor(w!("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;BU)"))?;
    let temp = unsafe {
        CreateFileW(
            wide(&staging).as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ,
            &attributes(&file_sd),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if temp == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut staged = unsafe { File::from_raw_handle(temp) };
    let result = (|| {
        io::copy(&mut source, &mut staged)?;
        drop(source);
        staged.sync_all()?;
        drop(staged);
        check_path(&staging, false, false, &installer)?;
        replace_staged_exe(&staging, exe)
    })();
    if result.is_err() {
        let _ = fs::remove_file(staging);
    }
    result
}

pub(crate) fn install_current_exe() -> Result<(), String> {
    let exe = installed_exe()?;
    install_at(&exe).map_err(|e| {
        render(
            Msg::SystemInstallProtected,
            &[("detail", &format!("{}: {e}", exe.display()))],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_only_accepts_the_loaded_image_and_sid_path_component() {
        assert!(is_running_image(&File::open(std::env::current_exe().unwrap()).unwrap()).unwrap());
        assert!(
            !is_running_image(
                &File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap()
            )
            .unwrap()
        );
        assert!(legacy_exe(r"S-1-5-21\..\other").is_err());
        let exe = legacy_exe("S-1-5-21-1-2-3-1001").unwrap();
        assert!(exe.ends_with(r"Rustile\S-1-5-21-1-2-3-1001\rustile.exe"));
        assert!(
            installed_exe()
                .unwrap()
                .ends_with(r"CKATEPTb\Rustile\rustile.exe")
        );
    }

    #[test]
    fn atomic_publication_replaces_a_running_executable() {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        let dir = std::env::temp_dir().join(format!(
            "rustile-publish-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let target = dir.join("running.exe");
        let stage = dir.join("staged");
        fs::write(&stage, b"first image").unwrap();
        replace_staged_exe(&stage, &target).unwrap();
        fs::write(&stage, b"replacement").unwrap();
        let exclusive = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&stage)
            .unwrap();
        assert!(replace_staged_exe(&stage, &target).is_err());
        drop(exclusive);
        assert_eq!(fs::read(&target).unwrap(), b"first image");
        assert_eq!(fs::read(&stage).unwrap(), b"replacement");
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        fs::copy(system.join(r"System32\findstr.exe"), &target).unwrap();
        let mut child = Command::new(&target)
            .arg("Rustile")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        fs::write(&stage, b"next image").unwrap();
        let published = replace_staged_exe(&stage, &target);
        let running = child.try_wait().unwrap().is_none();
        drop(child.stdin.take());
        child.wait().unwrap();
        let contents = fs::read(&target).unwrap();
        if stage.exists() {
            fs::remove_file(stage).unwrap();
        }
        fs::remove_file(target).unwrap();
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            assert!(retired_owner(entry.file_name().to_str().unwrap()).is_some());
            fs::remove_file(entry.path()).unwrap();
        }
        fs::remove_dir(dir).unwrap();
        published.unwrap();
        assert!(running);
        assert_eq!(contents, b"next image");
    }
}
