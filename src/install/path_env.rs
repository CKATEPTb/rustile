//! Registers the installation directory in the machine PATH. The duplicate
//! check recognizes the Program Files variable aliases in expandable values.
//! Appending preserves the original registry type and existing entries.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use windows_sys::w;

const MAX_ENV_UNITS: usize = 32_767;

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn check(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

fn environment_key(access: u32) -> io::Result<Key> {
    let mut key = null_mut();
    check(unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
            0,
            access,
            &mut key,
        )
    })?;
    Ok(Key(key))
}

#[derive(Debug, Eq, PartialEq)]
struct PathValue {
    kind: u32,
    text: Vec<u16>,
}

fn read(key: &Key) -> io::Result<PathValue> {
    let mut text = vec![0u16; 512];
    loop {
        let mut bytes = (text.len() * 2) as u32;
        let mut kind = 0;
        let code = unsafe {
            RegQueryValueExW(
                key.0,
                w!("Path"),
                null(),
                &mut kind,
                text.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(PathValue {
                kind: REG_EXPAND_SZ,
                text: vec![],
            });
        }
        if code == ERROR_MORE_DATA {
            let units = (bytes as usize).div_ceil(2);
            if units > MAX_ENV_UNITS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Machine PATH exceeds the Windows environment variable size limit",
                ));
            }
            text.resize(units, 0);
            continue;
        }
        check(code)?;
        if !matches!(kind, REG_SZ | REG_EXPAND_SZ) || !bytes.is_multiple_of(2) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Machine PATH is not a registry string",
            ));
        }
        text.truncate(bytes as usize / 2);
        if text.last() == Some(&0) {
            text.pop();
        }
        if text.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Machine PATH contains an embedded NUL",
            ));
        }
        return Ok(PathValue { kind, text });
    }
}

fn equal(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && (left.is_empty()
            || unsafe {
                CompareStringOrdinal(
                    left.as_ptr(),
                    left.len() as i32,
                    right.as_ptr(),
                    right.len() as i32,
                    1,
                ) == CSTR_EQUAL
            })
}

fn normalize(entry: &[u16]) -> Vec<u16> {
    let whitespace = |ch: &u16| matches!(*ch, 9 | 32);
    let start = entry.iter().take_while(|ch| whitespace(ch)).count();
    let entry = &entry[start..];
    let end = entry.iter().rev().take_while(|ch| whitespace(ch)).count();
    let entry = &entry[..entry.len() - end];
    let quote = [b'"' as u16];
    let entry = entry
        .strip_prefix(&quote)
        .and_then(|s| s.strip_suffix(&quote))
        .unwrap_or(entry);
    let mut path: Vec<u16> = entry
        .iter()
        .map(|&ch| if ch == b'/' as u16 { b'\\' as u16 } else { ch })
        .collect();
    while path.last() == Some(&(b'\\' as u16)) {
        path.pop();
    }
    path
}

fn contains(value: &PathValue, directory: &Path) -> bool {
    let wanted = normalize(&directory.as_os_str().encode_wide().collect::<Vec<_>>());
    // The install location is <native Program Files>\CKATEPTb\Rustile.
    let mut aliases = Vec::new();
    if value.kind == REG_EXPAND_SZ
        && let Some(root) = directory.ancestors().nth(2)
        && let Ok(tail) = directory.strip_prefix(root)
    {
        for var in ["%ProgramFiles%", "%ProgramW6432%"] {
            let alias = Path::new(var).join(tail);
            aliases.push(normalize(
                &alias.as_os_str().encode_wide().collect::<Vec<_>>(),
            ));
        }
    }
    value.text.split(|&ch| ch == b';' as u16).any(|entry| {
        let entry = normalize(entry);
        equal(&entry, &wanted) || aliases.iter().any(|alias| equal(&entry, alias))
    })
}

pub(super) fn registered(directory: &Path) -> io::Result<bool> {
    Ok(contains(
        &read(&environment_key(KEY_QUERY_VALUE)?)?,
        directory,
    ))
}

fn register_key(key: &Key, directory: &Path) -> io::Result<bool> {
    let addition: Vec<u16> = directory.as_os_str().encode_wide().collect();
    if !directory.is_absolute() || addition.iter().any(|&ch| ch == 0 || ch == b';' as u16) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Install directory cannot be represented in PATH",
        ));
    }
    for _ in 0..8 {
        let before = read(key)?;
        if contains(&before, directory) {
            return Ok(false);
        }
        let mut updated = before.text.clone();
        if !updated.is_empty() && updated.last() != Some(&(b';' as u16)) {
            updated.push(b';' as u16);
        }
        updated.extend_from_slice(&addition);
        updated.push(0);
        if updated.len() > MAX_ENV_UNITS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Adding the install directory would exceed the Windows PATH size limit",
            ));
        }
        // Pick up edits by other installers while we were preparing our value.
        if read(key)? != before {
            continue;
        }
        check(unsafe {
            RegSetValueExW(
                key.0,
                w!("Path"),
                0,
                before.kind,
                updated.as_ptr().cast(),
                (updated.len() * 2) as u32,
            )
        })?;
        return Ok(true);
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "Machine PATH is being modified by another process",
    ))
}

pub(super) fn register(directory: &Path) -> io::Result<()> {
    if register_key(
        &environment_key(KEY_QUERY_VALUE | KEY_SET_VALUE)?,
        directory,
    )? {
        // This runs in the installer, outside the window manager's message loop.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                w!("Environment") as isize,
                SMTO_ABORTIFHUNG,
                250,
                null_mut(),
            )
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_BINARY, REG_OPTION_VOLATILE, RegCreateKeyExW, RegDeleteKeyW,
    };

    const DIRECTORY: &str = r"C:\Program Files\CKATEPTb\Rustile";

    struct TestKey {
        key: Key,
        name: Vec<u16>,
    }

    impl TestKey {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name: Vec<_> = format!(
                "Software\\Rustile.PathTest.{}.{}.{}\0",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )
            .encode_utf16()
            .collect();
            let mut key = null_mut();
            check(unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    name.as_ptr(),
                    0,
                    null(),
                    REG_OPTION_VOLATILE,
                    KEY_QUERY_VALUE | KEY_SET_VALUE,
                    null(),
                    &mut key,
                    null_mut(),
                )
            })
            .unwrap();
            Self {
                key: Key(key),
                name,
            }
        }

        fn write(&self, kind: u32, text: &str) {
            let data: Vec<_> = text.encode_utf16().chain([0]).collect();
            check(unsafe {
                RegSetValueExW(
                    self.key.0,
                    w!("Path"),
                    0,
                    kind,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            })
            .unwrap();
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, self.name.as_ptr()) };
        }
    }

    #[test]
    fn appends_once_and_preserves_unexpanded_value_type_and_entries() {
        let key = TestKey::new();
        let dir = Path::new(DIRECTORY);
        assert!(register_key(&key.key, dir).unwrap());
        assert_eq!(read(&key.key).unwrap().kind, REG_EXPAND_SZ);
        for kind in [REG_SZ, REG_EXPAND_SZ] {
            let original = r"%SystemRoot%\System32;;C:\工具\bin;C:\Архив;";
            key.write(kind, original);
            assert!(register_key(&key.key, dir).unwrap());
            assert!(!register_key(&key.key, dir).unwrap());
            assert_eq!(
                read(&key.key).unwrap(),
                PathValue {
                    kind,
                    text: format!("{original}{DIRECTORY}").encode_utf16().collect()
                }
            );
        }
    }

    #[test]
    fn recognizes_case_slashes_and_expandable_program_files_aliases() {
        let key = TestKey::new();
        let dir = Path::new(DIRECTORY);
        for existing in [
            r#"C:\bin; "C:/PROGRAM FILES/ckateptb/rustile/" "#,
            r"%PROGRAMFILES%\ckateptb\RuStile",
            r"%programw6432%\CKATEPTB\Rustile\",
        ] {
            key.write(REG_EXPAND_SZ, existing);
            assert!(!register_key(&key.key, dir).unwrap());
            assert_eq!(
                read(&key.key).unwrap().text,
                existing.encode_utf16().collect::<Vec<_>>()
            );
        }
        key.write(REG_SZ, r"%ProgramFiles%\CKATEPTb\Rustile");
        assert!(register_key(&key.key, dir).unwrap());
        key.write(REG_EXPAND_SZ, r"C:\Program Files\CKATEPTb\Rustile-old");
        assert!(register_key(&key.key, dir).unwrap());
    }

    #[test]
    fn rejects_malformed_or_oversize_path_without_replacing_it() {
        let key = TestKey::new();
        let dir = Path::new(DIRECTORY);
        key.write(REG_BINARY, "keep");
        assert!(register_key(&key.key, dir).is_err());
        key.write(REG_SZ, "one\0two");
        assert!(register_key(&key.key, dir).is_err());
        let original = "a".repeat(MAX_ENV_UNITS - 2);
        key.write(REG_SZ, &original);
        assert!(register_key(&key.key, dir).is_err());
        assert_eq!(read(&key.key).unwrap().text.len(), original.len());
    }
}
