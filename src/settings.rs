mod bindings;

use crate::profile_io;
use bindings::Overrides;
pub(crate) use bindings::{BindingOwners, Chord, Shortcut};

const OWNER_CHANGED: &str = "Назначение уже изменилось. Запишите сочетание ещё раз.";

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
    SetStartup(crate::startup::Startup),
    Outline(u8),
    Gap(u8),
    Padding(u8),
    Shortcut(Shortcut, Chord),
    ReassignShortcut(Shortcut, Chord, BindingOwners),
    ResetShortcut(Shortcut),
    ResetShortcuts,
    Exit,
}

impl Command {
    pub fn changes_shortcuts(self) -> bool {
        matches!(
            self,
            Self::Shortcut(..)
                | Self::ReassignShortcut(..)
                | Self::ResetShortcut(_)
                | Self::ResetShortcuts
        )
    }
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
            Command::Shortcut(action, chord) => self.assign(action, chord)?,
            Command::ReassignShortcut(action, chord, owner) => {
                if self.conflict(action, chord) != Some(owner) {
                    return Err(OWNER_CHANGED.into());
                }
                self.transfer_confirmed(action, chord, owner)?;
            }
            Command::ResetShortcut(action) => self.assign(action, action.default_chord())?,
            Command::ResetShortcuts => {
                let defaults = Self::default();
                self.focus = defaults.focus;
                self.movement = defaults.movement;
                self.resize = defaults.resize;
                self.shortcuts = defaults.shortcuts;
            }
            Command::SetStartup(_) | Command::Exit => {}
        }
        self.validated()
    }

    pub fn changed_for_save(mut self, command: Command, live: Self) -> Result<Self, String> {
        // A launch-only opener can own a chord (or supply an alias) which is
        // free in the stored profile. Persist exactly the transfer shown in the editor.
        if let Command::ReassignShortcut(action, chord, owners) = command {
            if live.conflict(action, chord) != Some(owners)
                || self.conflict(action, chord).is_some_and(|stored| {
                    stored.actions().any(|s| !owners.actions().any(|a| a == s))
                })
            {
                return Err(OWNER_CHANGED.into());
            }
            self.transfer_confirmed(action, chord, owners)?;
            return self.validated();
        }
        self.changed(command)
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
        let text = match profile_io::with_user(|| fs::read_to_string(path)) {
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
        profile_io::with_user(|| PendingSave::new(path, content.as_bytes()))
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
        profile_io::with_user(|| {
            if unsafe {
                MoveFileExW(
                    src.as_ptr(),
                    dst.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })
        .map_err(|e| format!("Не удалось заменить {}: {e}", self.path.display()))
    }
}

impl Drop for PendingSave {
    fn drop(&mut self) {
        let _ = profile_io::with_user(|| fs::remove_file(&self.tmp));
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
                    partial.binding(Shortcut::FocusLeft).unwrap()
                ))
                .is_err()
        );
        assert!(partial.changed(Command::Outline(5)).is_err());
        assert!(toml::from_str::<Settings>("focus = \"invalid\"").is_err());
    }

    #[test]
    fn legacy_assignments_take_priority_over_new_quick_resize_defaults() {
        let text = "padding = 14\nshortcuts.focus_left = { mods = 9, key = 37 }";
        let profile = toml::from_str::<Settings>(text)
            .unwrap()
            .validated()
            .unwrap();
        let quick_left = Shortcut::QuickLeft.default_chord();
        assert_eq!(profile.padding, 14);
        assert_eq!(profile.binding(Shortcut::FocusLeft), Some(quick_left));
        assert_eq!(profile.binding(Shortcut::QuickLeft), None);
        assert_eq!(
            profile.binding(Shortcut::QuickRight),
            Some(Shortcut::QuickRight.default_chord())
        );
        assert_eq!(
            profile.conflict(Shortcut::QuickLeft, quick_left),
            Some(Shortcut::FocusLeft.into())
        );
        let remapped = profile
            .changed(Command::ReassignShortcut(
                Shortcut::QuickLeft,
                quick_left,
                Shortcut::FocusLeft.into(),
            ))
            .unwrap();
        assert_eq!(remapped.binding(Shortcut::FocusLeft), None);
        assert_eq!(remapped.binding(Shortcut::QuickLeft), Some(quick_left));
        for settings in [profile, remapped] {
            let saved = toml::to_string_pretty(&settings).unwrap();
            assert_eq!(toml::from_str::<Settings>(&saved).unwrap(), settings);
        }
    }

    #[test]
    fn existing_bindings_keep_win_q_and_win_f_until_reassigned_or_reset() {
        use Shortcut::{CloseWindow, FocusLeft, MoveRight, ToggleMaximize};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_F, VK_Q};

        let saved = "shortcuts.focus_left = { mods = 8, key = 81 }\n\
                     shortcuts.move_right = { mods = 8, key = 70 }";
        let profile = toml::from_str::<Settings>(saved)
            .unwrap()
            .validated()
            .unwrap();
        for (action, owner, key) in [
            (CloseWindow, FocusLeft, VK_Q),
            (ToggleMaximize, MoveRight, VK_F),
        ] {
            let chord = Chord { mods: MOD_WIN, key };
            assert_eq!(action.default_chord(), chord);
            assert_eq!(profile.binding(action), None);
            assert_eq!(profile.binding(owner), Some(chord));
            assert_eq!(profile.conflict(action, chord), Some(owner.into()));
            let moved = profile
                .changed(Command::ReassignShortcut(action, chord, owner.into()))
                .unwrap();
            let roundtrip: Settings = toml::from_str(&toml::to_string(&moved).unwrap()).unwrap();
            assert_eq!(roundtrip.binding(action), Some(chord));
            assert_eq!(roundtrip.binding(owner), None);
        }
        assert_eq!(
            profile.changed(Command::ResetShortcuts).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn mosaic_shortcut_preserves_legacy_owners_in_each_mode_and_transfers_both_on_confirmation() {
        use Shortcut::{FocusUp, GrowUp, ToggleMosaic};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_Y;

        let chord = Chord {
            mods: MOD_WIN,
            key: VK_Y,
        };
        assert_eq!(Settings::default().binding(ToggleMosaic), Some(chord));
        let normal = "shortcuts.focus_up = { mods = 8, key = 89 }";
        let resize = "shortcuts.grow_up = { mods = 8, key = 89 }";
        let both = format!("{normal}\n{resize}");
        for (profile, owners) in [
            (normal, BindingOwners::One(FocusUp)),
            (resize, BindingOwners::One(GrowUp)),
            (both.as_str(), BindingOwners::Both(FocusUp, GrowUp)),
        ] {
            let settings = toml::from_str::<Settings>(profile)
                .unwrap()
                .validated()
                .unwrap();
            assert_eq!(settings.binding(ToggleMosaic), None);
            assert_eq!(settings.conflict(ToggleMosaic, chord), Some(owners));
            if matches!(owners, BindingOwners::Both(_, _)) {
                assert!(
                    settings
                        .changed(Command::ReassignShortcut(
                            ToggleMosaic,
                            chord,
                            FocusUp.into()
                        ))
                        .is_err()
                );
            }
            let moved = settings
                .changed(Command::ReassignShortcut(ToggleMosaic, chord, owners))
                .unwrap();
            assert_eq!(moved.binding(ToggleMosaic), Some(chord));
            for owner in owners.actions() {
                assert_eq!(moved.binding(owner), None);
                assert_eq!(moved.conflict(owner, chord), Some(ToggleMosaic.into()));
            }
            let saved = toml::to_string(&moved).unwrap();
            assert_eq!(
                toml::from_str::<Settings>(&saved)
                    .unwrap()
                    .validated()
                    .unwrap(),
                moved
            );
        }
    }

    #[test]
    fn desktop_shortcuts_preserve_older_assignments_and_transfer_between_global_actions() {
        use Shortcut::{FocusUp, GrowUp, NextDesktop, PreviousDesktop};

        let settings: Settings = toml::from_str(
            "shortcuts.focus_up = { mods = 10, key = 37 }\nshortcuts.grow_up = { mods = 10, key = 37 }",
        )
        .unwrap();
        let settings = settings.validated().unwrap();
        let chord = PreviousDesktop.default_chord();
        assert_eq!(settings.binding(PreviousDesktop), None);
        let owners = BindingOwners::Both(FocusUp, GrowUp);
        assert_eq!(settings.conflict(PreviousDesktop, chord), Some(owners));
        let reassigned = settings
            .changed(Command::ReassignShortcut(PreviousDesktop, chord, owners))
            .unwrap();
        assert_eq!(
            reassigned.conflict(NextDesktop, chord),
            Some(PreviousDesktop.into())
        );
        let transferred = reassigned
            .changed(Command::ReassignShortcut(
                NextDesktop,
                chord,
                PreviousDesktop.into(),
            ))
            .unwrap();
        assert_eq!(transferred.binding(PreviousDesktop), None);
        assert_eq!(transferred.binding(NextDesktop), Some(chord));
        let restored: Settings = toml::from_str(&toml::to_string(&transferred).unwrap()).unwrap();
        assert_eq!(restored.validated().unwrap(), transferred);
    }

    #[test]
    fn assigning_a_global_chord_confirms_the_normal_binding_and_modal_alias() {
        use Shortcut::{FocusLeft, GrowLeft, ToggleMosaic};

        let settings = Settings::default();
        let chord = FocusLeft.default_chord();
        let owners = BindingOwners::Both(FocusLeft, GrowLeft);
        assert_eq!(settings.conflict(ToggleMosaic, chord), Some(owners));
        assert!(
            settings
                .changed(Command::Shortcut(ToggleMosaic, chord))
                .is_err()
        );
        let moved = settings
            .changed(Command::ReassignShortcut(ToggleMosaic, chord, owners))
            .unwrap();
        assert_eq!(moved.binding(FocusLeft), None);
        assert_eq!(moved.binding(GrowLeft), None);
        assert!(!moved.resize_alias(GrowLeft, chord));
        assert_eq!(moved.binding(ToggleMosaic), Some(chord));
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
            profile.binding(Shortcut::MoveLeft).unwrap(),
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
        assert_eq!(loaded.binding(Shortcut::MoveLeft).unwrap(), custom);
        assert_eq!(
            loaded.binding(Shortcut::MoveRight).unwrap(),
            Chord {
                mods: MOD_ALT | MOD_SHIFT,
                key: VK_RIGHT
            }
        );
        assert_eq!(
            loaded.binding(Shortcut::StartResize).unwrap(),
            Chord {
                mods: MOD_CONTROL | MOD_SHIFT,
                key: VK_RETURN
            }
        );
        assert_eq!(loaded.padding, 12);
        assert_eq!(
            next.changed(Command::Shortcut(
                Shortcut::MoveLeft,
                profile.binding(Shortcut::MoveLeft).unwrap()
            ))
            .unwrap(),
            profile
        );
    }

    #[test]
    fn binding_collisions_are_checked_within_each_mode() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_F8, VK_H, VK_Q, VK_X};
        let settings = Settings::default()
            .changed(Command::Shortcut(
                Shortcut::FocusLeft,
                Chord {
                    mods: MOD_CONTROL,
                    key: VK_H,
                },
            ))
            .unwrap();
        let assign =
            |action, mods, key| settings.changed(Command::Shortcut(action, Chord { mods, key }));
        assert!(
            settings
                .changed(Command::Shortcut(
                    Shortcut::GrowUp,
                    settings.binding(Shortcut::FocusLeft).unwrap()
                ))
                .is_ok()
        );
        assert!(
            settings
                .changed(Command::Shortcut(
                    Shortcut::GrowUp,
                    settings.binding(Shortcut::GrowRight).unwrap()
                ))
                .is_err()
        );
        assert!(assign(Shortcut::MoveRight, MOD_CONTROL | MOD_ALT | MOD_SHIFT, VK_Q).is_ok());
        assert!(assign(Shortcut::MoveLeft, MOD_WIN, VK_X).is_err());
        assert!(assign(Shortcut::MoveLeft, 0, VK_H).is_err());
        assert!(assign(Shortcut::MoveLeft, MOD_SHIFT, VK_H).is_err());
        assert!(assign(Shortcut::MoveLeft, 0, VK_F8).is_ok());
        assert!(assign(Shortcut::GrowUp, 0, VK_H).is_ok());
    }

    #[test]
    fn old_quit_setting_is_ignored_without_discarding_other_shortcuts() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_K;
        let text = "gap = 11\n[shortcuts]\nmove_down = { mods = 2, key = 75 }\n";
        for retired in ["{ mods = 3, key = 118 }", "'unused legacy value'"] {
            let loaded: Settings = toml::from_str(&format!("{text}quit = {retired}")).unwrap();
            let loaded = loaded.validated().unwrap();
            assert_eq!(loaded.gap, 11);
            assert_eq!(
                loaded.binding(Shortcut::MoveDown).unwrap(),
                Chord {
                    mods: MOD_CONTROL,
                    key: VK_K
                }
            );
            let persisted = toml::to_string_pretty(&loaded).unwrap();
            assert!(!persisted.contains("quit"));
            assert_eq!(toml::from_str::<Settings>(&persisted).unwrap(), loaded);
        }
        assert!(
            toml::from_str::<Settings>(&format!("{text}misspelled = {{ mods = 1, key = 88 }}"))
                .is_err()
        );
    }

    #[test]
    fn one_factory_shortcut_can_replace_a_legacy_profile_setting_and_all_can_be_reset_together() {
        let profile: Settings = toml::from_str(
            "focus = 'ctrl+alt'\nmovement = 'alt+shift'\nresize = 'ctrl+shift'\n\
             enabled = false\noutline = 3\ngap = 13\npadding = 15\n\
             [shortcuts]\nmove_left = { mods = 2, key = 72 }",
        )
        .unwrap();
        let profile = profile.validated().unwrap();
        let factory = Settings::default();
        let one = profile
            .changed(Command::ResetShortcut(Shortcut::FocusLeft))
            .unwrap();
        for action in Shortcut::ALL {
            let expected = if action == Shortcut::FocusLeft {
                factory.binding(action).unwrap()
            } else {
                profile.binding(action).unwrap()
            };
            assert_eq!(one.binding(action).unwrap(), expected);
        }
        let saved = toml::to_string_pretty(&one).unwrap();
        let reloaded: Settings = toml::from_str(&saved).unwrap();
        assert_eq!(reloaded, one);
        let all = reloaded.changed(Command::ResetShortcuts).unwrap();
        assert_eq!(
            all,
            Settings {
                enabled: false,
                outline: 3,
                gap: 13,
                padding: 15,
                ..factory
            }
        );
        assert!(!toml::to_string_pretty(&all).unwrap().contains("shortcuts"));
    }

    #[test]
    fn conflicting_single_reset_reports_the_other_action_and_reset_all_is_atomic() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_K;
        let factory = Settings::default();
        let custom = factory
            .changed(Command::Shortcut(
                Shortcut::FocusLeft,
                Chord {
                    mods: MOD_ALT,
                    key: VK_K,
                },
            ))
            .unwrap()
            .changed(Command::Shortcut(
                Shortcut::MoveDown,
                factory.binding(Shortcut::FocusLeft).unwrap(),
            ))
            .unwrap();
        let error = custom
            .changed(Command::ResetShortcut(Shortcut::FocusLeft))
            .unwrap_err();
        assert!(error.contains(Shortcut::MoveDown.label()));
        assert_eq!(custom.changed(Command::ResetShortcuts).unwrap(), factory);
    }

    #[test]
    fn confirmed_transfer_round_trips_and_keeps_the_previous_owner_unassigned_until_reset() {
        let original = Settings::default();
        let owner = Shortcut::FocusLeft;
        let action = Shortcut::MoveDown;
        let chord = owner.default_chord();
        assert_eq!(original.conflict(action, chord), Some(owner.into()));
        let moved = original
            .changed(Command::ReassignShortcut(action, chord, owner.into()))
            .unwrap();
        let text = toml::to_string_pretty(&moved).unwrap();
        assert!(text.contains("disabled"));
        let loaded = toml::from_str::<Settings>(&text)
            .unwrap()
            .validated()
            .unwrap();
        assert_eq!(loaded.binding(owner), None);
        assert_eq!(loaded.binding(action), Some(chord));
        for other in Shortcut::ALL {
            if other != action && other != owner {
                assert_eq!(loaded.binding(other), original.binding(other));
            }
        }
        // A stale confirmation must not transfer the binding again.
        assert!(
            loaded
                .changed(Command::ReassignShortcut(
                    Shortcut::MoveUp,
                    chord,
                    owner.into()
                ))
                .is_err()
        );
        assert!(loaded.changed(Command::ResetShortcut(owner)).is_err());
        let free = loaded.changed(Command::ResetShortcut(action)).unwrap();
        assert_eq!(free.binding(owner), None);
        assert_eq!(
            free.changed(Command::ResetShortcut(owner)).unwrap(),
            original
        );
        assert_eq!(loaded.changed(Command::ResetShortcuts).unwrap(), original);
    }

    #[test]
    fn confirmation_handles_resize_aliases_and_legacy_direct_bindings_take_precedence() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_LEFT;
        let original = Settings::default();
        let chord = Chord {
            mods: MOD_WIN,
            key: VK_LEFT,
        };
        assert_eq!(
            original.conflict(Shortcut::ShrinkUp, chord),
            Some(Shortcut::GrowLeft.into())
        );
        assert!(
            original
                .changed(Command::Shortcut(Shortcut::ShrinkUp, chord))
                .is_err()
        );
        let transferred = original
            .changed(Command::ReassignShortcut(
                Shortcut::ShrinkUp,
                chord,
                Shortcut::GrowLeft.into(),
            ))
            .unwrap();
        assert_eq!(transferred.binding(Shortcut::GrowLeft), None);
        assert!(!transferred.resize_alias(Shortcut::GrowLeft, chord));
        assert_eq!(transferred.binding(Shortcut::FocusLeft), Some(chord));
        // Older profiles could explicitly shadow a convenience alias.
        let legacy: Settings =
            toml::from_str("shortcuts.shrink_up = { mods = 8, key = 37 }").unwrap();
        let legacy = legacy.validated().unwrap();
        assert_eq!(
            legacy.conflict(Shortcut::ShrinkDown, chord),
            Some(Shortcut::ShrinkUp.into())
        );
        let next = legacy
            .changed(Command::ReassignShortcut(
                Shortcut::ShrinkDown,
                chord,
                Shortcut::ShrinkUp.into(),
            ))
            .unwrap();
        assert_eq!(next.binding(Shortcut::ShrinkUp), None);
        assert_eq!(
            next.binding(Shortcut::GrowLeft),
            original.binding(Shortcut::GrowLeft)
        );
        assert_eq!(
            next.conflict(Shortcut::ShrinkRight, chord),
            Some(Shortcut::ShrinkDown.into())
        );
    }

    #[test]
    fn transfer_of_a_launch_only_resize_binding_updates_both_stored_actions() {
        let saved = Settings::default();
        let live = saved.with_resize_override(Modifier::CtrlAlt).unwrap();
        let chord = live.binding(Shortcut::StartResize).unwrap();
        let command =
            Command::ReassignShortcut(Shortcut::FocusUp, chord, Shortcut::StartResize.into());
        assert!(saved.changed(command).is_err());
        let persisted = saved.changed_for_save(command, live).unwrap();
        assert_eq!(persisted, live.changed(command).unwrap());
        assert_eq!(persisted.binding(Shortcut::StartResize), None);
        assert_eq!(persisted.binding(Shortcut::FocusUp), Some(chord));
        // A different stored owner was never confirmed by the user.
        let other = saved
            .changed(Command::Shortcut(Shortcut::MoveUp, chord))
            .unwrap();
        assert!(other.changed_for_save(command, live).is_err());
        assert!(live.changed_for_save(command, other).is_err());
    }

    #[test]
    fn keyboard_validation_and_labels_cover_oem_layout_and_media_keys() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        for key in [
            VK_OEM_PLUS,
            VK_OEM_3,
            VK_OEM_102,
            VK_ABNT_C1,
            VK_MEDIA_PLAY_PAUSE,
            VK_VOLUME_UP,
            VK_SNAPSHOT,
            VK_SEPARATOR,
            VK_SCROLL,
        ] {
            let chord = Chord {
                mods: MOD_CONTROL,
                key,
            };
            assert!(Chord::supports_key(key));
            assert!(chord.validate(Shortcut::MoveUp).is_ok());
            assert_eq!(chord.label(), format!("Ctrl+{}", Chord::key_label(key)));
        }
        for key in [
            VK_LBUTTON,
            VK_LSHIFT,
            VK_RWIN,
            VK_GAMEPAD_A,
            VK_PACKET,
            VK_PROCESSKEY,
            0xff,
            0x100,
        ] {
            assert!(!Chord::supports_key(key));
        }
        assert_eq!(
            Chord {
                mods: MOD_WIN,
                key: VK_ADD
            }
            .label(),
            "Win+Num +"
        );
        assert!(
            Chord {
                mods: 0,
                key: VK_MEDIA_PLAY_PAUSE
            }
            .validate(Shortcut::StartResize)
            .is_ok()
        );
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
            settings.binding(Shortcut::StartResize).unwrap().mods,
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
