//! Match the installed implementation of the private desktop service. Several
//! Windows releases share this DLL even though their OS build numbers differ.

use super::{ManagerLayout, shell_window};
use std::ptr::null_mut;
use std::sync::Mutex;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VFT_DLL, VS_FFI_SIGNATURE, VS_FFI_STRUCVERSION,
    VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemDirectoryW, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows_sys::core::w;

pub(super) fn installed_layout() -> Option<ManagerLayout> {
    struct Image {
        taskbar: usize,
        process: u32,
        layout: Option<ManagerLayout>,
    }
    static CACHE: Mutex<Option<Image>> = Mutex::new(None);
    let taskbar = shell_window();
    let mut process = 0;
    if !taskbar.is_null() {
        unsafe { GetWindowThreadProcessId(taskbar, &mut process) };
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(image) = &*cache
        && image.taskbar == taskbar as usize
        && image.process == process
    {
        return image.layout;
    }
    let layout = read_layout();
    *cache = Some(Image {
        taskbar: taskbar as usize,
        process,
        layout,
    });
    layout
}

fn read_layout() -> Option<ManagerLayout> {
    let mut process_machine = 0;
    let mut native_machine = 0;
    if unsafe {
        IsWow64Process2(
            GetCurrentProcess(),
            &mut process_machine,
            &mut native_machine,
        )
    } == 0
    {
        return None;
    }
    layout_for_version(&shell_file_version()?, native_machine)
}

fn shell_file_version() -> Option<VS_FIXEDFILEINFO> {
    // Use the OS system directory independently of the environment or DLL search path.
    let capacity = unsafe { GetSystemDirectoryW(null_mut(), 0) };
    if capacity == 0 {
        return None;
    }
    let mut path = vec![0; capacity as usize];
    let length = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), capacity) };
    if length == 0 || length >= capacity {
        return None;
    }
    path.truncate(length as usize);
    if path.last() != Some(&u16::from(b'\\')) {
        path.push(u16::from(b'\\'));
    }
    path.extend("twinui.pcshell.dll".encode_utf16().chain([0]));

    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), null_mut()) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    let mut fixed = null_mut();
    let mut fixed_len = 0;
    unsafe {
        if GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0
            || VerQueryValueW(data.as_ptr().cast(), w!("\\"), &mut fixed, &mut fixed_len) == 0
            || fixed.is_null()
            || (fixed_len as usize) < size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        // The root version value points into data. Copy it before releasing the buffer.
        Some(fixed.cast::<VS_FIXEDFILEINFO>().read_unaligned())
    }
}

fn layout_for_version(info: &VS_FIXEDFILEINFO, native_machine: u16) -> Option<ManagerLayout> {
    if info.dwSignature != VS_FFI_SIGNATURE as u32
        || info.dwStrucVersion != VS_FFI_STRUCVERSION as u32
        || info.dwFileType != VFT_DLL as u32
        || info.dwFileVersionMS != 10 << 16
    {
        return None;
    }
    let build = info.dwFileVersionLS >> 16;
    let revision = info.dwFileVersionLS as u16;
    match (build, native_machine) {
        // The service must also expose a recognized manager IID on this family.
        (22621 | 22631, IMAGE_FILE_MACHINE_AMD64 | IMAGE_FILE_MACHINE_ARM64) => {
            Some(ManagerLayout::Legacy)
        }
        // KB5048667 ships twinui.pcshell.dll 26100.2454 on x64 and 26100.2605 on ARM64.
        (26100, IMAGE_FILE_MACHINE_AMD64) if revision >= 2454 => Some(ManagerLayout::Modern),
        (26100, IMAGE_FILE_MACHINE_ARM64) if revision >= 2605 => Some(ManagerLayout::Modern),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(build: u32, revision: u16) -> VS_FIXEDFILEINFO {
        VS_FIXEDFILEINFO {
            dwSignature: VS_FFI_SIGNATURE as u32,
            dwStrucVersion: VS_FFI_STRUCVERSION as u32,
            dwFileType: VFT_DLL as u32,
            dwFileVersionMS: 10 << 16,
            dwFileVersionLS: (build << 16) | u32::from(revision),
            ..Default::default()
        }
    }

    #[test]
    fn selects_known_shell_image_layouts() {
        for build in [22621, 22631] {
            assert_eq!(
                layout_for_version(&file(build, 3007), IMAGE_FILE_MACHINE_AMD64),
                Some(ManagerLayout::Legacy)
            );
        }
        for revision in [2454, 2605, 9974] {
            assert_eq!(
                layout_for_version(&file(26100, revision), IMAGE_FILE_MACHINE_AMD64),
                Some(ManagerLayout::Modern)
            );
        }
        for (build, revision) in [(22000, 3007), (26100, 2453), (26200, 9974), (26300, 9974)] {
            assert_eq!(
                layout_for_version(&file(build, revision), IMAGE_FILE_MACHINE_AMD64),
                None
            );
        }
        assert_eq!(
            layout_for_version(&file(26100, 2604), IMAGE_FILE_MACHINE_ARM64),
            None
        );
        assert_eq!(
            layout_for_version(&file(26100, 2605), IMAGE_FILE_MACHINE_ARM64),
            Some(ManagerLayout::Modern)
        );
    }

    #[test]
    fn rejects_unrecognized_or_invalid_fixed_version_info() {
        let valid = file(26100, 9974);
        for invalid in [
            VS_FIXEDFILEINFO {
                dwSignature: 0,
                ..valid
            },
            VS_FIXEDFILEINFO {
                dwStrucVersion: 0,
                ..valid
            },
            VS_FIXEDFILEINFO {
                dwFileType: 0,
                ..valid
            },
            VS_FIXEDFILEINFO {
                dwFileVersionMS: 11 << 16,
                ..valid
            },
        ] {
            assert_eq!(layout_for_version(&invalid, IMAGE_FILE_MACHINE_AMD64), None);
        }
    }
}
