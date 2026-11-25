mod bindings;

use bindings::Overrides;
pub(crate) use bindings::{Chord, Shortcut};

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
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_RETURN,
};

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
pub(crate) enum Command {
    Toggle,
    Outline(u8),
    Gap(u8),
    Padding(u8),
    Shortcut(Shortcut, Chord),
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
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    shortcuts: Overrides,
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
            shortcuts: Overrides::default(),
        }
    }
}

impl Settings {
    pub const MAX_OUTLINE: u8 = 4;
    pub const MAX_SPACING: u8 = 32;

    fn validated(self) -> Result<Self, String> {
        if self.version != 1 {
            return Err(format!("Неизвестная версия настроек: {}.", self.version));
        }
        if self.gap > Self::MAX_SPACING
            || self.padding > Self::MAX_SPACING
            || self.outline > Self::MAX_OUTLINE
        {
            return Err(format!(
                "Размер отступов должен быть от 0 до {}, толщина обводки — от 0 до {}.",
                Self::MAX_SPACING,
                Self::MAX_OUTLINE
            ));
        }
        self.validate_shortcuts()?;
        Ok(self)
    }

    pub fn changed(mut self, command: Command) -> Result<Self, String> {
        match command {
            Command::Toggle => self.enabled = !self.enabled,
            Command::Outline(value) => self.outline = value,
            Command::Gap(value) => self.gap = value,
            Command::Padding(value) => self.padding = value,
            Command::Shortcut(action, chord) => self.assign(action, chord),
            Command::Exit => {}
        }
        self.validated()
    }

    pub fn with_resize_override(self, modifier: Modifier) -> Result<Self, String> {
        self.changed(Command::Shortcut(
            Shortcut::StartResize,
            Chord {
                mods: modifier.bits(),
                key: VK_RETURN,
            },
        ))
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
                .changed(Command::Shortcut(
                    Shortcut::MoveLeft,
                    partial.chord(Shortcut::FocusLeft)
                ))
                .is_err()
        );
        assert!(partial.changed(Command::Outline(5)).is_err());
        assert!(toml::from_str::<Settings>("focus = \"invalid\"").is_err());
    }

    #[test]
    fn individual_bindings_roundtrip_without_changing_legacy_profile_or_neighbors() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_H, VK_LEFT, VK_RIGHT};
        let profile: Settings = toml::from_str(
            "focus = 'ctrl+alt'\nmovement = 'alt+shift'\nresize = 'ctrl+shift'\npadding = 12",
        )
        .unwrap();
        let profile = profile.validated().unwrap();
        assert_eq!(
            profile.chord(Shortcut::MoveLeft),
            Chord {
                mods: MOD_ALT | MOD_SHIFT,
                key: VK_LEFT
            }
        );
        let custom = Chord {
            mods: MOD_CONTROL,
            key: VK_H,
        };
        let next = profile
            .changed(Command::Shortcut(Shortcut::MoveLeft, custom))
            .unwrap();
        let text = toml::to_string_pretty(&next).unwrap();
        assert!(text.contains("shortcuts.move_left"));
        let loaded = toml::from_str::<Settings>(&text)
            .unwrap()
            .validated()
            .unwrap();
        assert_eq!(loaded, next);
        assert_eq!(loaded.chord(Shortcut::MoveLeft), custom);
        assert_eq!(
            loaded.chord(Shortcut::MoveRight),
            Chord {
                mods: MOD_ALT | MOD_SHIFT,
                key: VK_RIGHT
            }
        );
        assert_eq!(
            loaded.chord(Shortcut::StartResize),
            Chord {
                mods: MOD_CONTROL | MOD_SHIFT,
                key: VK_RETURN
            }
        );
        assert_eq!(loaded.padding, 12);
        assert_eq!(
            next.changed(Command::Shortcut(
                Shortcut::MoveLeft,
                profile.chord(Shortcut::MoveLeft)
            ))
            .unwrap(),
            profile
        );
    }

    #[test]
    fn binding_collisions_follow_modes_and_global_quit() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_F8, VK_H, VK_X};
        let settings = Settings::default();
        let assign =
            |action, mods, key| settings.changed(Command::Shortcut(action, Chord { mods, key }));
        assert!(
            settings
                .changed(Command::Shortcut(
                    Shortcut::GrowUp,
                    settings.chord(Shortcut::FocusLeft)
                ))
                .is_ok()
        );
        assert!(
            settings
                .changed(Command::Shortcut(
                    Shortcut::GrowUp,
                    settings.chord(Shortcut::GrowRight)
                ))
                .is_err()
        );
        for action in [Shortcut::GrowUp, Shortcut::MoveRight] {
            assert!(
                settings
                    .changed(Command::Shortcut(action, settings.chord(Shortcut::Quit)))
                    .is_err()
            );
        }
        assert!(assign(Shortcut::MoveLeft, MOD_WIN, VK_X).is_err());
        assert!(assign(Shortcut::MoveLeft, 0, VK_H).is_err());
        assert!(assign(Shortcut::MoveLeft, MOD_SHIFT, VK_H).is_err());
        assert!(assign(Shortcut::MoveLeft, 0, VK_F8).is_ok());
        assert!(assign(Shortcut::GrowUp, 0, VK_H).is_ok());
    }

    #[test]
    fn resize_override_and_remap_preserve_only_unmodified_modal_aliases() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_H, VK_UP};
        let settings = Settings::default()
            .with_resize_override(Modifier::CtrlAlt)
            .unwrap();
        let held = Chord {
            mods: MOD_CONTROL | MOD_ALT,
            key: VK_UP,
        };
        assert!(settings.resize_alias(Shortcut::GrowUp, held));
        assert_eq!(
            settings.chord(Shortcut::StartResize).mods,
            MOD_CONTROL | MOD_ALT
        );
        let settings = settings
            .changed(Command::Shortcut(
                Shortcut::GrowUp,
                Chord { mods: 0, key: VK_H },
            ))
            .unwrap();
        assert!(!settings.resize_alias(Shortcut::GrowUp, held));
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
