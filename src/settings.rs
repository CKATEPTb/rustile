mod bindings;
pub(crate) mod exclusions;
mod scheme;

use crate::i18n::{Msg, render, tr};
use crate::profile_io;
#[cfg(test)]
pub(crate) use bindings::BindingOwners;
pub(crate) use bindings::{Assignment, BindingConflicts, Chord, Shortcut, WHEEL_DOWN, WHEEL_UP};
use bindings::{Overrides, StoredOverrides};
use exclusions::ExclusionRule;
use scheme::{Scheme, SchemeKey};

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
    Assign(Assignment),
    Transfer(Assignment, BindingConflicts),
    RemoveShortcut(Shortcut, Chord),
    ResetShortcuts,
    Exit,
}

impl Command {
    pub fn changes_shortcuts(self) -> bool {
        matches!(
            self,
            Self::Assign(..) | Self::Transfer(..) | Self::RemoveShortcut(..) | Self::ResetShortcuts
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "StoredSettings")]
pub(crate) struct Settings {
    version: u16,
    pub enabled: bool,
    pub outline: u8,
    pub gap: u8,
    pub padding: u8,
    #[serde(skip_serializing_if = "Scheme::is_default")]
    scheme: Scheme,
    #[serde(skip_serializing_if = "Overrides::is_empty")]
    shortcuts: Overrides,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<ExclusionRule>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            outline: 1,
            gap: 8,
            padding: 8,
            scheme: Scheme::default(),
            shortcuts: Overrides::default(),
            exclusions: Vec::new(),
        }
    }
}

// Pre-scheme profiles stored three independent modifier groups. Import those
// groups as individual exceptions so unrelated rules keep their original keys.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSettings {
    version: Option<u16>,
    enabled: Option<bool>,
    outline: Option<u8>,
    gap: Option<u8>,
    padding: Option<u8>,
    focus: Option<Modifier>,
    movement: Option<Modifier>,
    resize: Option<Modifier>,
    scheme: Option<Scheme>,
    #[serde(default)]
    shortcuts: StoredOverrides,
    #[serde(default)]
    exclusions: Vec<ExclusionRule>,
}

impl From<StoredSettings> for Settings {
    fn from(file: StoredSettings) -> Self {
        let defaults = Self::default();
        let mut settings = Self {
            version: file.version.unwrap_or(defaults.version),
            enabled: file.enabled.unwrap_or(defaults.enabled),
            outline: file.outline.unwrap_or(defaults.outline),
            gap: file.gap.unwrap_or(defaults.gap),
            padding: file.padding.unwrap_or(defaults.padding),
            scheme: file.scheme.unwrap_or_default(),
            shortcuts: Overrides::default(),
            exclusions: file.exclusions,
        };
        settings.import_bindings(
            file.shortcuts,
            file.scheme.is_none(),
            file.focus,
            file.movement,
            file.resize,
        );
        settings
    }
}

impl Settings {
    pub const MAX_OUTLINE: u8 = 4;
    pub const MAX_SPACING: u8 = 32;

    fn validated(&self) -> Result<Self, String> {
        if self.version != 1 {
            return Err(render(
                Msg::ConfigVersion,
                &[("version", &self.version.to_string())],
            ));
        }
        if self.gap > Self::MAX_SPACING
            || self.padding > Self::MAX_SPACING
            || self.outline > Self::MAX_OUTLINE
        {
            return Err(render(
                Msg::ConfigRange,
                &[
                    ("spacing", &Self::MAX_SPACING.to_string()),
                    ("outline", &Self::MAX_OUTLINE.to_string()),
                ],
            ));
        }
        self.scheme.validate()?;
        self.validate_shortcuts()?;
        Ok(self.clone())
    }

    pub fn changed(&self, command: Command) -> Result<Self, String> {
        let mut next = self.clone();
        match command {
            Command::Toggle => next.enabled = !next.enabled,
            Command::Outline(value) => next.outline = value,
            Command::Gap(value) => next.gap = value,
            Command::Padding(value) => next.padding = value,
            Command::Assign(assignment) => {
                next.apply_assignment(assignment, BindingConflicts::default())?
            }
            Command::Transfer(assignment, confirmed) => {
                next.apply_assignment(assignment, confirmed)?
            }
            Command::RemoveShortcut(action, chord) => next.remove_chord(action, chord)?,
            Command::ResetShortcuts => {
                let defaults = Self::default();
                next.scheme = defaults.scheme;
                next.shortcuts = defaults.shortcuts;
            }
            Command::SetStartup(_) | Command::Exit => {}
        }
        next.validated()
    }

    pub fn changed_for_save(&self, command: Command, live: &Self) -> Result<Self, String> {
        if self == live {
            return self.changed(command);
        }
        let mut next = self.clone();
        // Only explicitly edited actions can persist a launch-only resize opener.
        match command {
            Command::Transfer(edit, confirmed) => {
                next.copy_confirmed_bindings(live, edit, confirmed)?
            }
            Command::Assign(edit) => {
                next.copy_confirmed_bindings(live, edit, BindingConflicts::default())?
            }
            Command::RemoveShortcut(action, chord) => {
                live.changed(command)?;
                if let Some(stored) = next.stored_source(live, action, chord) {
                    next.remove_chord(action, stored)?;
                }
            }
            _ => return self.changed(command),
        }
        next.validated()
    }

    pub fn path() -> Result<PathBuf, String> {
        let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| tr(Msg::ConfigLocalAppData))?;
        Ok(PathBuf::from(local).join("Rustile").join("settings.toml"))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match profile_io::with_user(|| fs::read_to_string(path)) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(file_error(Msg::ConfigRead, path, &e)),
        };
        let parsed: Self =
            toml::from_str(&text).map_err(|e| file_error(Msg::ConfigParse, path, &e))?;
        parsed.validated()
    }

    // Prepare the file before changing live geometry; validation or I/O failure
    // must leave the previous settings file intact.
    pub fn prepare(&self, path: &Path) -> Result<PendingSave, String> {
        let content =
            toml::to_string_pretty(self).map_err(|e| file_error(Msg::ConfigWrite, path, &e))?;
        profile_io::with_user(|| PendingSave::new(path, content.as_bytes()))
            .map_err(|e| file_error(Msg::ConfigWrite, path, &e))
    }
}

fn file_error(msg: Msg, path: &Path, error: &dyn std::fmt::Display) -> String {
    render(
        msg,
        &[
            ("path", &path.to_string_lossy()),
            ("error", &error.to_string()),
        ],
    )
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
        .map_err(|e| file_error(Msg::ConfigReplace, &self.path, &e))
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
    fn display_language_changes_action_and_conflict_text_but_keeps_bindings_portable() {
        use crate::i18n::{Language, with_language};

        let action = Shortcut::MoveLeft;
        let chord = action.default_chord();
        let conflict = || {
            Settings::default()
                .changed(Command::Assign(Assignment::Add(Shortcut::FocusLeft, chord)))
                .unwrap_err()
        };
        with_language(Language::De, || {
            assert_eq!(action.label(), "Nach links verschieben");
            assert_eq!(chord.label(), "Win+Shift+Left");
            assert!(conflict().contains("„Nach links verschieben“"));
        });
        with_language(Language::Ja, || {
            assert_eq!(action.label(), "左に移動");
            assert_eq!(chord.label(), "Win+Shift+Left");
            assert!(conflict().contains("「左に移動」"));
            let changed = Settings::default()
                .changed(Command::RemoveShortcut(action, chord))
                .unwrap();
            assert!(
                toml::to_string_pretty(&changed)
                    .unwrap()
                    .contains("move_left")
            );
        });
    }

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
                .assign_primary(
                    Shortcut::MoveLeft,
                    partial.binding(Shortcut::FocusLeft).unwrap()
                )
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
            .transfer_primary(Shortcut::QuickLeft, quick_left, Shortcut::FocusLeft.into())
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
                .transfer_primary(action, chord, owner.into())
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
                        .transfer_primary(ToggleMosaic, chord, FocusUp.into())
                        .is_err()
                );
            }
            let moved = settings
                .transfer_primary(ToggleMosaic, chord, owners)
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
        assert!(!settings.bindings(PreviousDesktop).contains(&chord));
        let owners = BindingOwners::Both(FocusUp, GrowUp);
        assert_eq!(settings.conflict(PreviousDesktop, chord), Some(owners));
        let addition = Assignment::Add(PreviousDesktop, chord);
        let reassigned = settings
            .changed(Command::Transfer(
                addition,
                settings.conflicts(addition).unwrap(),
            ))
            .unwrap();
        assert_eq!(
            reassigned.conflict(NextDesktop, chord),
            Some(PreviousDesktop.into())
        );
        let transferred = reassigned
            .transfer_primary(NextDesktop, chord, PreviousDesktop.into())
            .unwrap();
        assert_eq!(
            transferred.bindings(PreviousDesktop),
            settings.bindings(PreviousDesktop)
        );
        assert_eq!(transferred.binding(NextDesktop), Some(chord));
        let restored: Settings = toml::from_str(&toml::to_string(&transferred).unwrap()).unwrap();
        assert_eq!(restored.validated().unwrap(), transferred);
    }

    #[test]
    fn desktop_move_preserves_existing_normal_and_resize_bindings() {
        use Shortcut::{FocusLeft, GrowLeft, MovePreviousDesktop, MovePreviousDesktopAlt};

        let profile = "shortcuts.focus_left = { mods = 14, key = 37 }\nshortcuts.grow_left = { mods = 14, key = 37 }";
        let settings = toml::from_str::<Settings>(profile)
            .unwrap()
            .validated()
            .unwrap();
        let chord = MovePreviousDesktop.default_chord();
        assert_eq!(
            settings.bindings(MovePreviousDesktop),
            [MovePreviousDesktopAlt.default_chord()]
        );
        assert_eq!(settings.binding(FocusLeft), Some(chord));
        assert_eq!(settings.binding(GrowLeft), Some(chord));
        let owners = BindingOwners::Both(FocusLeft, GrowLeft);
        assert_eq!(settings.conflict(MovePreviousDesktop, chord), Some(owners));
        let addition = Assignment::Add(MovePreviousDesktop, chord);
        let moved = settings
            .changed(Command::Transfer(
                addition,
                settings.conflicts(addition).unwrap(),
            ))
            .unwrap();
        assert_eq!(moved.binding(FocusLeft), None);
        assert_eq!(moved.binding(GrowLeft), None);
        assert_eq!(
            moved.bindings(MovePreviousDesktop),
            [MovePreviousDesktopAlt.default_chord(), chord]
        );
        let saved = toml::to_string(&moved).unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&saved)
                .unwrap()
                .validated()
                .unwrap(),
            moved
        );
    }

    #[test]
    fn desktop_number_preserves_a_saved_digit_and_wheel_assignments_roundtrip() {
        use Shortcut::{Desktop1, NextDesktop, NextDesktopWheel, PreviousDesktop};
        let saved = "shortcuts.focus_left = { mods = 8, key = 49 }";
        let settings = toml::from_str::<Settings>(saved)
            .unwrap()
            .validated()
            .unwrap();
        assert_eq!(settings.binding(Desktop1), None);
        assert_eq!(
            settings.binding(Shortcut::FocusLeft),
            Some(Desktop1.default_chord())
        );
        let down = NextDesktopWheel.default_chord();
        assert_eq!(settings.conflict(Desktop1, down), Some(NextDesktop.into()));
        let changed = settings
            .transfer_primary(Desktop1, down, NextDesktop.into())
            .unwrap();
        assert_eq!(changed.binding(Desktop1), Some(down));
        assert!(!changed.bindings(NextDesktop).contains(&down));
        assert_eq!(
            changed.bindings(PreviousDesktop).last().unwrap().label(),
            "Win+Wheel Up"
        );
        let restored: Settings = toml::from_str(&toml::to_string(&changed).unwrap()).unwrap();
        assert_eq!(restored.validated().unwrap(), changed);
        assert_eq!(
            restored.changed(Command::ResetShortcuts).unwrap(),
            Settings::default()
        );
        assert!(
            Chord {
                key: WHEEL_UP,
                mods: 0
            }
            .validate(Desktop1)
            .is_err()
        );
    }

    #[test]
    fn numbered_transfers_preserve_existing_owners_in_both_modes_and_can_be_reset() {
        use Shortcut::{FocusLeft, MoveDesktop5, MoveDesktop6, ShrinkUp};

        let chord = MoveDesktop5.default_chord();
        assert_eq!(chord.label(), "Win+Shift+5");
        for scheme in ["", "[scheme]\n"] {
            let text = format!(
                "{scheme}[shortcuts]\nfocus_left = [{{mods=8,key=37}},{{mods=12,key=53}}]\nshrink_up = {{mods=12,key=53}}"
            );
            let imported = toml::from_str::<Settings>(&text)
                .unwrap()
                .validated()
                .unwrap();
            assert!(imported.bindings(MoveDesktop5).is_empty());
            assert_eq!(
                imported.binding(MoveDesktop6),
                Some(MoveDesktop6.default_chord())
            );
            assert_eq!(
                imported.conflict(MoveDesktop5, chord),
                Some(BindingOwners::Both(FocusLeft, ShrinkUp))
            );
            let reset = Assignment::Reset(MoveDesktop5);
            assert!(imported.changed(Command::Assign(reset)).is_err());
            let moved = imported
                .changed(Command::Transfer(reset, imported.conflicts(reset).unwrap()))
                .unwrap();
            assert_eq!(moved.bindings(FocusLeft), [FocusLeft.default_chord()]);
            assert!(moved.bindings(ShrinkUp).is_empty());
            assert_eq!(moved.bindings(MoveDesktop5), [chord]);
            let reloaded: Settings = toml::from_str(&toml::to_string(&moved).unwrap()).unwrap();
            assert_eq!(reloaded.validated().unwrap(), moved);
        }
    }

    #[test]
    fn numbered_transfers_use_saved_base_and_grab_and_preserve_digit_direction_aliases() {
        use Shortcut::{MoveDesktop5, MoveDesktop10, ShrinkLeft};

        let saved = "scheme.base = 18\nscheme.resize = 91\nscheme.left = 53\nshortcuts.desktop5 = 'disabled'\nshortcuts.move_left = 'disabled'";
        let imported = toml::from_str::<Settings>(saved)
            .unwrap()
            .validated()
            .unwrap();
        assert_eq!(
            imported.binding(MoveDesktop10).unwrap().label(),
            "Alt+Shift+0"
        );
        let chord = Chord {
            mods: MOD_ALT | MOD_SHIFT,
            key: b'5' as u16,
        };
        assert_eq!(
            imported.conflict(MoveDesktop5, chord),
            Some(ShrinkLeft.into())
        );
        assert!(imported.bindings(MoveDesktop5).is_empty());
        let changed = imported
            .transfer_primary(MoveDesktop5, chord, ShrinkLeft.into())
            .unwrap();
        let reloaded: Settings =
            toml::from_str(&toml::to_string_pretty(&changed).unwrap()).unwrap();
        assert_eq!(reloaded.validated().unwrap(), changed);
        assert_eq!(reloaded.bindings(MoveDesktop5), [chord]);
        assert_eq!(reloaded.bindings(ShrinkLeft), imported.bindings(ShrinkLeft));
        assert_eq!(
            changed.changed(Command::ResetShortcuts).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn assigning_a_global_chord_confirms_the_normal_binding_and_modal_alias() {
        use Shortcut::{FocusLeft, GrowLeft, ToggleMosaic};

        let settings = Settings::default();
        let chord = FocusLeft.default_chord();
        let owners = BindingOwners::Both(FocusLeft, GrowLeft);
        assert_eq!(settings.conflict(ToggleMosaic, chord), Some(owners));
        assert!(settings.assign_primary(ToggleMosaic, chord).is_err());
        let moved = settings
            .transfer_primary(ToggleMosaic, chord, owners)
            .unwrap();
        assert_eq!(moved.binding(FocusLeft), None);
        assert_eq!(moved.bindings(GrowLeft), GrowLeft.factory_bindings());
        assert_eq!(moved.conflict(GrowLeft, chord), Some(ToggleMosaic.into()));
        assert_eq!(moved.binding(ToggleMosaic), Some(chord));
    }

    #[test]
    fn individual_bindings_roundtrip_without_changing_legacy_profile_or_neighbors() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_H, VK_LEFT, VK_RETURN, VK_RIGHT};
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
        let next = profile.assign_primary(Shortcut::MoveLeft, custom).unwrap();
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
            next.assign_primary(
                Shortcut::MoveLeft,
                profile.binding(Shortcut::MoveLeft).unwrap()
            )
            .unwrap(),
            profile
        );
    }

    #[test]
    fn binding_collisions_are_checked_within_each_mode() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_F8, VK_H, VK_Q, VK_X};
        let settings = Settings::default()
            .assign_primary(
                Shortcut::FocusLeft,
                Chord {
                    mods: MOD_CONTROL,
                    key: VK_H,
                },
            )
            .unwrap();
        let assign = |action, mods, key| settings.assign_primary(action, Chord { mods, key });
        assert!(
            settings
                .assign_primary(
                    Shortcut::GrowUp,
                    settings.binding(Shortcut::FocusLeft).unwrap()
                )
                .is_ok()
        );
        assert!(
            settings
                .assign_primary(
                    Shortcut::GrowUp,
                    settings.binding(Shortcut::GrowRight).unwrap()
                )
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
    fn multiple_shortcuts_roundtrip_and_edit_delete_only_the_selected_chord() {
        use Shortcut::{MoveLeft, MoveRight};
        let base = MoveLeft.default_chord();
        let j = Chord {
            key: b'J' as u16,
            ..base
        };
        let l = Chord {
            key: b'L' as u16,
            ..base
        };
        let other = MoveRight.default_chord();
        let first = Settings::default()
            .changed(Command::Assign(Assignment::Add(MoveLeft, j)))
            .unwrap();
        assert!(
            first
                .changed(Command::Assign(Assignment::Add(MoveLeft, j)))
                .is_err()
        );
        assert!(
            first
                .changed(Command::Assign(Assignment::Edit(MoveLeft, base, j)))
                .is_err()
        );
        let edited = first
            .changed(Command::Assign(Assignment::Edit(MoveLeft, j, l)))
            .unwrap();
        assert_eq!(edited.bindings(MoveLeft), [base, l]);
        assert!(
            edited
                .changed(Command::RemoveShortcut(MoveLeft, j))
                .is_err()
        );
        let take = Assignment::Add(MoveRight, l);
        let confirmed = edited.conflicts(take).unwrap();
        assert_eq!(
            confirmed.iter().collect::<Vec<_>>(),
            [(l, BindingOwners::One(MoveLeft))]
        );
        let moved = edited.changed(Command::Transfer(take, confirmed)).unwrap();
        assert_eq!(moved.bindings(MoveLeft), [base]);
        assert_eq!(moved.bindings(MoveRight), [other, l]);
        assert!(moved.changed(Command::Transfer(take, confirmed)).is_err());
        let saved = toml::to_string(&moved).unwrap();
        let loaded: Settings = toml::from_str(&saved).unwrap();
        assert_eq!(loaded.validated().unwrap(), moved);
        let removed = moved
            .changed(Command::RemoveShortcut(MoveRight, other))
            .unwrap();
        assert_eq!(removed.bindings(MoveRight), [l]);
        assert_eq!(
            removed
                .changed(Command::Assign(Assignment::Reset(MoveRight)))
                .unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn legacy_desktop_slots_merge_in_order_and_new_arrays_are_authoritative() {
        use Shortcut::{PreviousDesktop, PreviousDesktopAlt};
        let original = PreviousDesktop.factory_bindings();
        let custom = Chord {
            mods: MOD_CONTROL,
            key: b'J' as u16,
        };
        let legacy: Settings = toml::from_str("shortcuts.previous_desktop = 'disabled'\nshortcuts.previous_desktop_alt = {mods=2, key=74}").unwrap();
        assert_eq!(legacy.bindings(PreviousDesktop), [custom, original[2]]);
        assert!(legacy.bindings(PreviousDesktopAlt).is_empty());
        let one = legacy
            .changed(Command::RemoveShortcut(PreviousDesktop, original[2]))
            .unwrap();
        let saved = toml::to_string(&one).unwrap();
        assert!(
            !saved.contains("previous_desktop_alt") && !saved.contains("previous_desktop_wheel")
        );
        let loaded: Settings = toml::from_str(&saved).unwrap();
        assert_eq!(loaded.bindings(PreviousDesktop), [custom]);
        let empty = loaded
            .changed(Command::RemoveShortcut(PreviousDesktop, custom))
            .unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&toml::to_string(&empty).unwrap())
                .unwrap()
                .bindings(PreviousDesktop),
            []
        );
        assert_eq!(
            empty
                .changed(Command::Assign(Assignment::Reset(PreviousDesktop)))
                .unwrap()
                .bindings(PreviousDesktop),
            original
        );
    }

    #[test]
    fn larger_binding_sets_preserve_their_order_and_all_opener_modifiers() {
        use Shortcut::{GrowLeft, StartResize};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_F1, VK_F24, VK_LEFT};
        let mut saved = Settings::default();
        let mut expected = StartResize.factory_bindings();
        for key in VK_F1..=VK_F24 {
            let chord = Chord {
                mods: if key % 2 == 0 { MOD_CONTROL } else { MOD_ALT },
                key,
            };
            expected.push(chord);
            saved = saved
                .changed(Command::Assign(Assignment::Add(StartResize, chord)))
                .unwrap();
        }
        let old = expected.remove(9);
        saved = saved
            .changed(Command::RemoveShortcut(StartResize, old))
            .unwrap();
        let profile = toml::from_str::<Settings>(&toml::to_string(&saved).unwrap()).unwrap();
        assert_eq!(profile.bindings(StartResize), expected);
        for mods in [MOD_WIN, MOD_CONTROL, MOD_ALT] {
            assert!(profile.resize_alias(GrowLeft, Chord { mods, key: VK_LEFT }));
        }
    }

    #[test]
    fn a_reset_transfers_all_factory_conflicts_in_one_confirmed_change() {
        use Shortcut::{MoveLeft, MoveRight, PreviousDesktop};
        let defaults = PreviousDesktop.factory_bindings();
        let mut profile = Settings::default();
        for (owner, &chord) in [MoveLeft, MoveRight].into_iter().zip(&defaults) {
            let add = Assignment::Add(owner, chord);
            profile = profile
                .changed(Command::Transfer(add, profile.conflicts(add).unwrap()))
                .unwrap();
        }
        assert_eq!(profile.bindings(PreviousDesktop), [defaults[2]]);
        let reset = Assignment::Reset(PreviousDesktop);
        let conflicts = profile.conflicts(reset).unwrap();
        assert_eq!(conflicts.iter().count(), 2);
        assert!(profile.changed(Command::Assign(reset)).is_err());
        let freed = profile
            .changed(Command::RemoveShortcut(MoveRight, defaults[1]))
            .unwrap();
        assert!(freed.changed(Command::Transfer(reset, conflicts)).is_err());
        // Both arrows also have inherited modal aliases. Their unmodified keys survive.
        assert_eq!(
            profile
                .changed(Command::Transfer(reset, conflicts))
                .unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn fixed_resize_exits_ignore_retired_settings_and_preserve_other_bindings() {
        use Shortcut::{Cancel, Confirm, GrowLeft, MoveLeft};
        let profile: Settings = toml::from_str("gap=17\n[shortcuts]\nconfirm={unknown=true}\ncancel='disabled'\ngrow_left=[{key=13,mods=0},{key=27,mods=0},{key=74,mods=0}]\nmove_left={key=74,mods=12}").unwrap();
        let profile = profile.validated().unwrap();
        assert_eq!(profile.bindings(GrowLeft), [Chord { mods: 0, key: 74 }]);
        assert_eq!(profile.bindings(MoveLeft), [Chord { mods: 12, key: 74 }]);
        assert_eq!(profile.gap, 17);
        for action in [Confirm, Cancel] {
            assert_eq!(profile.bindings(action), action.factory_bindings());
            assert!(!action.is_editable());
            assert!(
                profile
                    .changed(Command::RemoveShortcut(action, action.default_chord()))
                    .is_err()
            );
            assert!(
                profile
                    .conflicts(Assignment::Add(GrowLeft, action.default_chord()))
                    .is_err()
            );
        }
        let saved = toml::to_string(&profile).unwrap();
        assert!(!saved.contains("confirm") && !saved.contains("cancel"));
    }

    #[test]
    fn changing_a_secondary_opener_preserves_the_launch_only_primary() {
        use Shortcut::StartResize;
        let default = StartResize.default_chord();
        let a = Chord {
            mods: MOD_ALT,
            key: b'J' as u16,
        };
        let b = Chord {
            mods: MOD_ALT,
            key: b'K' as u16,
        };
        for initial in [vec![default], vec![]] {
            let mut saved = Settings::default();
            saved.set_bindings(StartResize, initial.clone());
            let mut live = saved.with_resize_override(Modifier::CtrlAlt).unwrap();
            for (command, extra) in [
                (Command::Assign(Assignment::Add(StartResize, a)), Some(a)),
                (
                    Command::Assign(Assignment::Edit(StartResize, a, b)),
                    Some(b),
                ),
                (Command::RemoveShortcut(StartResize, b), None),
            ] {
                saved = saved.changed_for_save(command, &live).unwrap();
                live = live.changed(command).unwrap();
                assert_eq!(
                    saved.bindings(StartResize),
                    initial.iter().copied().chain(extra).collect::<Vec<_>>()
                );
                assert_eq!(live.bindings(StartResize)[0].mods, MOD_CONTROL | MOD_ALT);
            }
            let replace = Command::Assign(Assignment::Edit(
                StartResize,
                live.bindings(StartResize)[0],
                b,
            ));
            assert_eq!(
                saved
                    .changed_for_save(replace, &live)
                    .unwrap()
                    .bindings(StartResize),
                [b]
            );
        }
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
            .changed(Command::Assign(Assignment::Reset(Shortcut::FocusLeft)))
            .unwrap();
        for action in Shortcut::ALL.into_iter().filter(|a| a.is_editable()) {
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
            .assign_primary(
                Shortcut::FocusLeft,
                Chord {
                    mods: MOD_ALT,
                    key: VK_K,
                },
            )
            .unwrap()
            .assign_primary(
                Shortcut::MoveDown,
                factory.binding(Shortcut::FocusLeft).unwrap(),
            )
            .unwrap();
        let error = custom
            .changed(Command::Assign(Assignment::Reset(Shortcut::FocusLeft)))
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
            .transfer_primary(action, chord, owner.into())
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
                .transfer_primary(Shortcut::MoveUp, chord, owner.into())
                .is_err()
        );
        assert!(
            loaded
                .changed(Command::Assign(Assignment::Reset(owner)))
                .is_err()
        );
        let free = loaded
            .changed(Command::Assign(Assignment::Reset(action)))
            .unwrap();
        assert_eq!(free.binding(owner), None);
        assert_eq!(
            free.changed(Command::Assign(Assignment::Reset(owner)))
                .unwrap(),
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
        assert!(original.assign_primary(Shortcut::ShrinkUp, chord).is_err());
        let transferred = original
            .transfer_primary(Shortcut::ShrinkUp, chord, Shortcut::GrowLeft.into())
            .unwrap();
        assert_eq!(
            transferred.binding(Shortcut::GrowLeft),
            Some(Shortcut::GrowLeft.default_chord())
        );
        assert_eq!(
            transferred.conflict(Shortcut::GrowLeft, chord),
            Some(Shortcut::ShrinkUp.into())
        );
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
            .transfer_primary(Shortcut::ShrinkDown, chord, Shortcut::ShrinkUp.into())
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
        let assignment =
            Assignment::Edit(Shortcut::FocusUp, Shortcut::FocusUp.default_chord(), chord);
        let command = Command::Transfer(assignment, live.conflicts(assignment).unwrap());
        assert!(saved.changed(command).is_err());
        let persisted = saved.changed_for_save(command, &live).unwrap();
        assert_eq!(persisted, live.changed(command).unwrap());
        assert_eq!(persisted.binding(Shortcut::StartResize), None);
        assert_eq!(persisted.binding(Shortcut::FocusUp), Some(chord));
        // A different stored owner was never confirmed by the user.
        let other = saved.assign_primary(Shortcut::MoveUp, chord).unwrap();
        assert!(other.changed_for_save(command, &live).is_err());
        assert!(live.changed_for_save(command, &other).is_err());
    }

    #[test]
    fn shortcut_keycaps_use_stable_english_legends() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        for key in (VK_A..=VK_Z).chain(VK_0..=VK_9) {
            assert_eq!(Chord::key_label(key), char::from(key as u8).to_string());
        }
        for key in VK_F1..=VK_F24 {
            assert_eq!(Chord::key_label(key), format!("F{}", key - VK_F1 + 1));
        }
        for (key, expected) in [
            (VK_LEFT, "Left"),
            (VK_DOWN, "Down"),
            (VK_UP, "Up"),
            (VK_RIGHT, "Right"),
            (VK_SPACE, "Space"),
            (VK_OEM_1, ";"),
            (VK_OEM_102, "ISO \\"),
            (VK_ADD, "Num +"),
            (VK_MEDIA_PLAY_PAUSE, "Play/Pause"),
            (WHEEL_UP, "Wheel Up"),
            (WHEEL_DOWN, "Wheel Down"),
        ] {
            assert_eq!(Chord::key_labels(0, Some(key)), [expected]);
        }
        for key in (0..=WHEEL_DOWN).filter(|key| Chord::supports_key(*key)) {
            let label = Chord::key_label(key);
            assert!(!label.is_empty() && label.is_ascii(), "{key:#x}: {label}");
            assert!(!label.starts_with("VK "), "missing legend for {key:#x}");
        }
        assert_eq!(Chord::key_labels(MOD_WIN, Some(VK_OEM_PLUS)), ["Win", "+"]);
        let mods = MOD_CONTROL | MOD_WIN | MOD_ALT | MOD_SHIFT;
        assert_eq!(
            Chord::key_labels(mods, None),
            ["Ctrl", "Win", "Alt", "Shift"]
        );
        assert!(Chord::key_labels(0, None).is_empty());
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
            WHEEL_DOWN + 1,
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
            .assign_primary(Shortcut::GrowUp, Chord { mods: 0, key: VK_H })
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
