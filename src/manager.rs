use std::collections::HashMap;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART,
};

use crate::hud::{FocusOutline, ResizeHud};
use crate::layout::{Direction, Id, Layout, Rect};
use crate::native::{self, Original};
use crate::settings::Settings;

#[cfg(test)]
mod desktop_test;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Resize,
}

struct ResizeSession {
    target: Id,
    before: Layout,
}

struct TrackedWindow {
    original: Original,
    requested: Option<Rect>,
}

pub struct Manager {
    settings: Settings,
    layout: Layout,
    windows: HashMap<Id, TrackedWindow>,
    last_foreground: Option<Id>,
    focus_history: Vec<Id>,
    area: Option<Rect>,
    dragging: Option<Id>,
    session: Option<ResizeSession>,
    hud: ResizeHud,
    outline: FocusOutline,
}

impl Manager {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_settings(Settings::default())
    }

    pub fn with_settings(settings: Settings) -> Self {
        Self {
            settings,
            layout: Layout::with_gap(native::spacing_px(settings.gap)),
            windows: HashMap::new(),
            last_foreground: None,
            focus_history: Vec::new(),
            area: tiling_area(settings),
            dragging: None,
            session: None,
            hud: ResizeHud::default(),
            outline: FocusOutline::default(),
        }
    }

    // Subscribe before the initial inventory so windows opened during startup
    // are reconciled from the event queue.
    pub fn sync(&mut self) {
        if !self.settings.enabled {
            return;
        }
        let foreground = native::foreground();
        let resume = !self.windows.is_empty();
        // A paused manager may have missed a destroyed, hidden or reused HWND.
        let tracked: Vec<_> = self.windows.keys().copied().collect();
        for id in tracked {
            if !self.windows[&id].original.matches(id) {
                self.forget(id);
            } else if !native::eligible(id) {
                self.layout.remove(id);
                self.focus_history.retain(|&prior| prior != id);
            } else {
                self.windows.get_mut(&id).unwrap().original.refresh_frame();
            }
        }
        for window in self.windows.values_mut() {
            window.requested = None;
        }
        self.remember_tile(foreground);
        for id in native::enumerate() {
            if self.layout.contains(id) {
                continue;
            }
            if resume {
                if native::eligible(id) {
                    self.admit_new(id);
                }
            } else {
                self.admit(id, None);
            }
        }
        self.last_foreground = Some(foreground);
        self.remember_tile(foreground);
        self.apply();
    }

    #[cfg(test)]
    pub fn apply_settings(&mut self, updated: Settings) -> Result<(), String> {
        self.apply_settings_with(updated, || Ok(()))
    }

    pub fn apply_settings_with(
        &mut self,
        updated: Settings,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let previous = self.settings;
        let resuming = updated.enabled && !previous.enabled;
        let geometry = updated.gap != previous.gap || updated.padding != previous.padding;
        let proposed = if geometry || resuming {
            let area = tiling_area(updated)
                .ok_or_else(|| "Windows не сообщила размер рабочей области.".to_owned())?;
            // Validate the whole layout before changing the live model or any HWND.
            let mut layout = self.layout.clone();
            if !previous.enabled {
                for (&id, window) in &self.windows {
                    if !window.original.matches(id) || !native::eligible(id) {
                        layout.remove(id);
                    }
                }
            }
            let next = layout
                .with_spacing(area, native::spacing_px(updated.gap))
                .ok_or_else(|| {
                    "С такими отступами окна не помещаются. Уменьшите отступ.".to_owned()
                })?;
            Some((area, next))
        } else {
            None
        };
        persist()?;
        if let Some((area, layout)) = proposed {
            self.area = Some(area);
            self.layout = layout;
        }
        self.settings = updated;
        if geometry || !updated.enabled {
            // Keep the visible preview when the user pauses the current arrangement.
            self.finish_mode(true);
        }
        if !updated.enabled {
            self.dragging = None;
            self.outline.hide();
        } else if resuming {
            self.sync();
        } else if geometry {
            self.apply();
        }
        self.refresh_focus();
        Ok(())
    }

    fn forget(&mut self, id: Id) {
        self.layout.remove(id);
        self.windows.remove(&id);
        self.focus_history.retain(|&previous| previous != id);
    }

    fn remember_tile(&mut self, id: Id) {
        if self.focus_history.last() != Some(&id) && self.layout.contains(id) {
            self.focus_history.retain(|&previous| previous != id);
            self.focus_history.push(id);
        }
    }

    fn admit_new(&mut self, id: Id) -> bool {
        if let Some(anchor) = self
            .focus_history
            .iter()
            .rev()
            .copied()
            .find(|&prior| self.can_act_on(prior))
        {
            return self.admit(id, Some(anchor));
        }
        let Some(area) = self.area else { return false };
        let tiles = self.layout.placements(area);
        if tiles.is_empty() {
            return self.admit(id, None);
        }
        let largest = tiles
            .into_iter()
            .filter(|&(prior, _)| self.can_act_on(prior))
            .max_by_key(|(_, rect)| i64::from(rect.w) * i64::from(rect.h));
        largest.is_some_and(|(anchor, _)| self.admit(id, Some(anchor)))
    }

    fn admit(&mut self, id: Id, anchor: Option<Id>) -> bool {
        let Some(area) = self.area else { return false };
        if self.layout.contains(id) || !native::eligible(id) {
            return false;
        }
        let mut insert = |minimum| match anchor {
            Some(anchor) => self.layout.insert_near(id, anchor, area, minimum),
            None => self.layout.insert_with_minimum(id, area, minimum),
        };
        let entry = match self.windows.entry(id) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let original = &mut entry.get_mut().original;
                original.refresh_frame();
                return insert(original.minimum_frame_size());
            }
            std::collections::hash_map::Entry::Vacant(entry) => entry,
        };
        let Some(original) = native::capture(id) else {
            return false;
        };
        if !insert(original.minimum_frame_size()) {
            return false;
        }
        entry.insert(TrackedWindow {
            original,
            requested: None,
        });
        true
    }

    pub fn event(&mut self, event: u32, id: Id) {
        if !self.settings.enabled {
            return;
        }
        // Events arrive in order, but GetForegroundWindow can already describe
        // a later launch when the message loop drains several events together.
        if event == EVENT_SYSTEM_FOREGROUND {
            self.last_foreground = Some(id);
        }
        if self.session.as_ref().is_some_and(|s| {
            native::foreground() != s.target || (event == EVENT_SYSTEM_FOREGROUND && id != s.target)
        }) {
            self.finish_mode(false);
        }
        if event == EVENT_SYSTEM_MOVESIZESTART {
            self.dragging = Some(id);
            return;
        }
        if event == EVENT_SYSTEM_MOVESIZEEND {
            self.dragging = None;
        } else if self.dragging == Some(id) && event != EVENT_OBJECT_DESTROY {
            if event == EVENT_SYSTEM_FOREGROUND && self.can_act_on(id) {
                self.remember_tile(id);
            }
            return;
        }

        let known = self.windows.get(&id);
        let was_tiled = self.layout.contains(id);
        if event == EVENT_OBJECT_DESTROY || known.is_some_and(|w| !w.original.matches(id)) {
            if known.is_some() {
                self.finish_mode(false);
                self.forget(id);
                if self.dragging == Some(id) {
                    self.dragging = None;
                }
                if was_tiled {
                    self.apply();
                }
            }
            if event == EVENT_OBJECT_DESTROY {
                if self.last_foreground == Some(id) {
                    self.last_foreground = None;
                }
                return;
            }
        }
        if native::eligible(id) {
            if !self.layout.contains(id) {
                self.finish_mode(false);
                if self.admit_new(id) {
                    self.apply();
                }
            } else if event == EVENT_SYSTEM_MOVESIZEEND
                || (matches!(event, EVENT_OBJECT_LOCATIONCHANGE | EVENT_SYSTEM_FOREGROUND)
                    && native::is_snapped(id))
            {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.requested = None;
                    window.original.refresh_frame();
                }
                self.apply();
            }
            if self.last_foreground == Some(id) {
                self.remember_tile(id);
            }
        } else if self.layout.contains(id) {
            self.finish_mode(false);
            self.layout.remove(id);
            self.focus_history.retain(|&previous| previous != id);
            if let Some(window) = self.windows.get_mut(&id) {
                window.requested = None;
            }
            self.apply();
        }
    }

    fn apply(&mut self) {
        let Some(area) = self.area else { return };
        for (id, rect) in self.layout.placements(area) {
            let Some(window) = self.windows.get_mut(&id) else {
                continue;
            };
            // Window positioning is asynchronous. Compare the last enqueued
            // target, so Esc also supersedes a preview still in the target's queue.
            if self.dragging != Some(id)
                && window.requested != Some(rect)
                && window.original.place(rect)
            {
                window.requested = Some(rect);
            }
        }
    }

    fn can_act_on(&self, target: Id) -> bool {
        self.settings.enabled
            && self
                .windows
                .get(&target)
                .is_some_and(|w| w.original.matches(target))
            && native::eligible(target)
    }

    pub fn begin_resize(&mut self, target: Id) -> bool {
        let Some(area) = self.area else { return false };
        if self.session.is_some() || !self.can_act_on(target) {
            return false;
        }
        let tiles = self.layout.placements(area);
        if tiles.len() < 2 {
            return false;
        }
        let Some((_, rect)) = tiles.into_iter().find(|(id, _)| *id == target) else {
            return false;
        };
        self.session = Some(ResizeSession {
            target,
            before: self.layout.clone(),
        });
        self.hud.show(target, rect, None);
        true
    }

    pub fn mode(&self) -> Mode {
        if self.session.is_some() {
            Mode::Resize
        } else {
            Mode::Normal
        }
    }

    // Sample foreground once so the cue and the keyboard target agree, including
    // when a modal dialog receives focus while its owner is still tiled.
    pub fn refresh_focus(&mut self) -> Option<Id> {
        if !self.settings.enabled {
            return None;
        }
        let id = native::foreground();
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.target != id)
        {
            self.finish_mode(false);
        }
        if self.layout.contains(id)
            && self
                .windows
                .get(&id)
                .is_some_and(|w| w.original.matches(id))
        {
            self.outline
                .show(id, self.settings.outline, self.settings.gap == 0);
            Some(id)
        } else {
            self.outline.hide();
            None
        }
    }

    pub fn resize(&mut self, active: Id, dir: Direction, shrink: bool) {
        let (Some(session), Some(area)) = (&self.session, self.area) else {
            return;
        };
        let target = session.target;
        if active != target || !self.can_act_on(target) {
            self.finish_mode(false);
            return;
        }
        if let Some(edge) = self.layout.resize(target, dir, shrink, area) {
            self.apply();
            if let Some((_, rect)) = self
                .layout
                .placements(area)
                .into_iter()
                .find(|(id, _)| *id == target)
            {
                self.hud.show(target, rect, Some(edge));
            }
        }
    }

    pub fn finish_mode(&mut self, commit: bool) {
        if let Some(session) = self.session.take() {
            self.hud.hide();
            if !commit {
                self.layout = session.before;
                self.apply();
            }
        }
    }

    pub fn move_window(&mut self, target: Id, direction: Direction) {
        if let Some(area) = self.area
            && self.session.is_none()
            && self.can_act_on(target)
            && self.layout.move_window(target, direction, area)
        {
            self.apply();
        }
    }

    pub fn focus(&mut self, direction: Direction) {
        if !self.settings.enabled {
            return;
        }
        let Some(area) = self.area else { return };
        if let Some(id) = self.layout.neighbor(native::foreground(), direction, area) {
            native::focus(id);
        }
    }

    pub fn restore_positions(&mut self) {
        self.session = None;
        self.outline.hide();
        self.hud.hide();
        for (id, window) in self.windows.drain() {
            native::restore(id, &window.original);
        }
        self.layout = Layout::with_gap(native::spacing_px(self.settings.gap));
        self.dragging = None;
        self.focus_history.clear();
        self.last_foreground = None;
    }

    pub fn shutdown(&mut self) {
        self.restore_positions();
    }
}

fn tiling_area(settings: Settings) -> Option<Rect> {
    native::work_area().map(|area| area.inset_for_tiles(native::spacing_px(settings.padding)))
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
