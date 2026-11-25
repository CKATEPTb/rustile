use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum Modifier {
    #[serde(rename = "win")]
    Win,
    #[serde(rename = "win+shift")]
    WinShift,
    #[serde(rename = "alt")]
    Alt,
    #[serde(rename = "alt+shift")]
    AltShift,
    #[serde(rename = "ctrl+alt")]
    CtrlAlt,
    #[serde(rename = "ctrl+alt+shift")]
    CtrlAltShift,
    #[serde(rename = "ctrl+shift")]
    CtrlShift,
}

impl Modifier {
    pub const ALL: [Self; 7] = [
        Self::Win,
        Self::WinShift,
        Self::Alt,
        Self::AltShift,
        Self::CtrlAlt,
        Self::CtrlAltShift,
        Self::CtrlShift,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Win => "Win",
            Self::WinShift => "Win+Shift",
            Self::Alt => "Alt",
            Self::AltShift => "Alt+Shift",
            Self::CtrlAlt => "Ctrl+Alt",
            Self::CtrlAltShift => "Ctrl+Alt+Shift",
            Self::CtrlShift => "Ctrl+Shift",
        }
    }

    pub const fn bits(self) -> u32 {
        match self {
            Self::Win => MOD_WIN,
            Self::WinShift => MOD_WIN | MOD_SHIFT,
            Self::Alt => MOD_ALT,
            Self::AltShift => MOD_ALT | MOD_SHIFT,
            Self::CtrlAlt => MOD_CONTROL | MOD_ALT,
            Self::CtrlAltShift => MOD_CONTROL | MOD_ALT | MOD_SHIFT,
            Self::CtrlShift => MOD_CONTROL | MOD_SHIFT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    Focus,
    Move,
    Resize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Toggle,
    Outline(u8),
    Gap(u8),
    Padding(u8),
    Shortcut(Binding, Modifier),
    Restore,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    version: u16,
    pub enabled: bool,
    pub outline: u8,
    pub gap: u8,
    pub padding: u8,
    pub focus: Modifier,
    pub movement: Modifier,
    pub resize: Modifier,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            outline: 1,
            gap: 8,
            padding: 8,
            focus: Modifier::Win,
            movement: Modifier::WinShift,
            resize: Modifier::Win,
        }
    }
}

impl Settings {
    fn validated(self) -> Result<Self, String> {
        if self.version != 1 {
            return Err(format!("Неизвестная версия настроек: {}.", self.version));
        }
        if self.gap > 32 || self.padding > 32 || self.outline > 4 {
            return Err(
                "Размер отступов должен быть от 0 до 32, толщина обводки — от 0 до 4.".into(),
            );
        }
        if self.focus == self.movement {
            return Err("Фокус и перемещение используют одни и те же стрелки. Выберите разные модификаторы.".into());
        }
        Ok(self)
    }

    pub fn changed(mut self, command: Command) -> Result<Self, String> {
        match command {
            Command::Toggle => self.enabled = !self.enabled,
            Command::Restore => self.enabled = false,
            Command::Outline(value) => self.outline = value,
            Command::Gap(value) => self.gap = value,
            Command::Padding(value) => self.padding = value,
            Command::Shortcut(Binding::Focus, m) => self.focus = m,
            Command::Shortcut(Binding::Move, m) => self.movement = m,
            Command::Shortcut(Binding::Resize, m) => self.resize = m,
            Command::Exit => {}
        }
        self.validated()
    }

    pub fn path() -> Result<PathBuf, String> {
        let local = std::env::var_os("LOCALAPPDATA")
            .ok_or("Windows не указала расположение папки LOCALAPPDATA.")?;
        Ok(PathBuf::from(local).join("Rustile").join("settings.toml"))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("Не удалось прочитать {}: {e}", path.display())),
        };
        let parsed: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        parsed.validated()
    }

    // Prepare the file before changing live geometry; validation or I/O failure
    // must leave the previous settings file intact.
    pub fn prepare(&self, path: &Path) -> Result<PendingSave, String> {
        let content = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        PendingSave::new(path, content.as_bytes())
            .map_err(|e| format!("Не удалось записать {}: {e}", path.display()))
    }
}

pub(crate) struct PendingSave {
    tmp: PathBuf,
    path: PathBuf,
}

impl PendingSave {
    fn new(path: &Path, contents: &[u8]) -> io::Result<Self> {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        fs::create_dir_all(parent)?;
        let (pending, mut file) = loop {
            let n = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let tmp = parent.join(format!(".settings-{}-{n}.tmp", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&tmp) {
                Ok(file) => {
                    break (
                        Self {
                            tmp,
                            path: path.to_owned(),
                        },
                        file,
                    );
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(pending)
    }

    pub fn commit(self) -> Result<(), String> {
        let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let src = wide(self.tmp.as_os_str());
        let dst = wide(self.path.as_os_str());
        if unsafe {
            MoveFileExW(
                src.as_ptr(),
                dst.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(format!(
                "Не удалось заменить {}: {}",
                self.path.display(),
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

impl Drop for PendingSave {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_and_menu_share_validation_and_unset_fields_have_defaults() {
        let partial: Settings = toml::from_str("version = 1\ngap = 12").unwrap();
        assert_eq!(
            partial.validated().unwrap(),
            Settings {
                gap: 12,
                ..Settings::default()
            }
        );
        assert!(
            partial
                .changed(Command::Shortcut(Binding::Move, Modifier::Win))
                .is_err()
        );
        assert!(partial.changed(Command::Outline(5)).is_err());
        assert!(toml::from_str::<Settings>("focus = \"invalid\"").is_err());
    }

    #[test]
    fn publishing_and_abandoning_settings_are_atomic() {
        let dir = std::env::temp_dir().join(format!(
            "rustile-settings-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("settings.toml");
        let first = Settings::load(&path).unwrap();
        first.prepare(&path).unwrap().commit().unwrap();
        let next = first.changed(Command::Gap(12)).unwrap();
        drop(next.prepare(&path).unwrap());
        assert_eq!(Settings::load(&path).unwrap(), first);
        next.prepare(&path).unwrap().commit().unwrap();
        assert_eq!(Settings::load(&path).unwrap(), next);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
