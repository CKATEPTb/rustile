//! Downloadable Windows release. Only this process opens the LZX cabinet.
//! The extracted Rustile handles installation, CLI commands and elevation as usual.

#[path = "../../src/launch_context.rs"]
mod launch_context;
mod stage;

use std::{
    env,
    fs::OpenOptions,
    io::{self, Cursor},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    process::Command,
    ptr::null_mut,
};

use cab::Cabinet;
use sha2::{Digest, Sha256};
use windows_sys::Win32::{
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    },
    System::{
        LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories},
        Threading::CREATE_NO_WINDOW,
    },
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
};
use windows_sys::core::w;

const CABINET: &[u8] = include_bytes!(env!("RUSTILE_RELEASE_CAB"));
const DIGEST: &[u8; 32] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.sha256"));

fn run(from_terminal: bool) -> io::Result<i32> {
    if unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let stage = stage::Stage::new()?;
    let mut cabinet = Cabinet::new(Cursor::new(CABINET))?;
    {
        let mut file = stage.create_file()?;
        io::copy(&mut cabinet.read_file("rustile.exe")?, &mut file)?;
        file.sync_all()?;
    }
    // Hash the actual file opened without write/delete sharing. Hold that
    // handle across the child's installation and any UAC round trip.
    let mut image = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(stage.path())?;
    let metadata = image.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unexpected Rustile payload file",
        ));
    }
    let mut sha = Sha256::new();
    io::copy(&mut image, &mut sha)?;
    if &<[u8; 32]>::from(sha.finalize()) != DIGEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Rustile payload checksum mismatch",
        ));
    }
    let mut child = Command::new(stage.path());
    child
        .args(env::args_os().skip(1))
        .env(launch_context::PACKAGE_ORIGIN, env::current_exe()?);
    if !from_terminal {
        child.creation_flags(CREATE_NO_WINDOW);
    }
    let status = child.status()?;
    Ok(status.code().unwrap_or(1))
}

fn main() {
    let from_terminal = launch_context::launched_from_terminal();
    match run(from_terminal) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            let message = format!("Could not open the Rustile release: {error}");
            if from_terminal {
                eprintln!("{message}");
            } else {
                let message: Vec<_> = message.encode_utf16().chain([0]).collect();
                unsafe {
                    MessageBoxW(
                        null_mut(),
                        message.as_ptr(),
                        w!("Rustile"),
                        MB_OK | MB_ICONERROR,
                    )
                };
            }
            std::process::exit(1);
        }
    }
}
