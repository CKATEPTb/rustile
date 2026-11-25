use std::collections::{HashMap, HashSet};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_STATECHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART,
};

use crate::hud::{FocusOutline, ResizeHud};
use crate::layout::{Direction, Id, Layout, Rect};
use crate::native::desktops::{DesktopId, DesktopMembership, Desktops};
use crate::native::{self, Original, TileState};
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
    desktop: DesktopId,
    original: Original,
    requested: Option<Rect>,
    repairs: u8,
}

impl TrackedWindow {
    // A successful async request can still be superseded by the app restoring
    // its own normal frame. Bound retries for apps that reject a tile size.
    fn frame_drifted(&mut self, rect: Rect) -> bool {
        if self.original.is_at(rect) {
            self.repairs = 0;
            return false;
        }
        self.repairs < 2
    }
}

struct Workspace {
    layout: Layout,
    last_foreground: Option<Id>,
    focus_history: Vec<Id>,
}

impl Workspace {
    fn new(gap: i32) -> Self {
        Self {
            layout: Layout::with_gap(gap),
            last_foreground: None,
            focus_history: Vec::new(),
        }
    }

    fn remove(&mut self, id: Id) {
        self.layout.remove(id);
        self.focus_history.retain(|&other| other != id);
        if self.last_foreground == Some(id) {
            self.last_foreground = None;
        }
    }
}

pub struct Manager {
    settings: Settings,
    desktops: Desktops,
    current: Option<DesktopId>,
    workspace: Workspace,
    parked: HashMap<DesktopId, Workspace>,
    windows: HashMap<Id, TrackedWindow>,
    expanded: HashSet<Id>,
    needs_desktop_retry: bool,
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
            desktops: Desktops::new(),
            current: None,
            workspace: Workspace::new(native::spacing_px(settings.gap)),
            parked: HashMap::new(),
            windows: HashMap::new(),
            expanded: HashSet::new(),
            needs_desktop_retry: false,
            area: tiling_area(settings),
            dragging: None,
            session: None,
            hud: ResizeHud::default(),
            outline: FocusOutline::default(),
        }
    }

    // Only a window that Windows positively identifies on the current desktop
    // can select a layout. Keep the entire previous BSP, including pending moves.
    fn select(&mut self, desktop: DesktopId) {
        if self.current == Some(desktop) {
            return;
        }
        self.rollback_resize();
        self.dragging = None;
        self.outline.hide();
        let next = self
            .parked
            .remove(&desktop)
            .unwrap_or_else(|| Workspace::new(native::spacing_px(self.settings.gap)));
        let previous = std::mem::replace(&mut self.workspace, next);
        if let Some(old) = self.current.replace(desktop)
            && self.windows.values().any(|w| w.desktop == old)
        {
            self.parked.insert(old, previous);
        }
    }

    pub fn tracks_window(&self, id: Id) -> bool {
        self.windows.contains_key(&id)
    }

    pub fn take_desktop_retry(&mut self) -> bool {
        std::mem::take(&mut self.needs_desktop_retry)
    }

    fn on_current(&self, id: Id) -> bool {
        matches!(self.desktops.membership(id), DesktopMembership::Current(key) if Some(key) == self.current)
    }

    fn detach(&mut self, id: Id, desktop: DesktopId) {
        if self.current == Some(desktop) {
            if self.workspace.layout.contains(id) {
                self.rollback_resize();
            }
            self.workspace.remove(id);
            if self.dragging == Some(id) {
                self.dragging = None;
            }
        } else if let Some(previous) = self.parked.get_mut(&desktop) {
            previous.remove(id);
        }
    }

    fn remove_empty_desktop(&mut self, desktop: DesktopId) {
        if !self
            .windows
            .values()
            .any(|window| window.desktop == desktop)
        {
            self.parked.remove(&desktop);
        }
    }

    fn forget(&mut self, id: Id) {
        self.expanded.remove(&id);
        if let Some(window) = self.windows.remove(&id) {
            self.detach(id, window.desktop);
            self.remove_empty_desktop(window.desktop);
        }
    }

    // Move the original capture with the HWND. Recapturing here would make a
    // tiled position its new "original" when the user moves it between desktops.
    fn migrate(&mut self, id: Id, desktop: DesktopId) -> bool {
        let Some(old) = self.windows.get(&id).map(|w| w.desktop) else {
            return false;
        };
        if old == desktop {
            return false;
        }
        self.detach(id, old);
        let window = self.windows.get_mut(&id).unwrap();
        window.desktop = desktop;
        window.requested = None;
        self.remove_empty_desktop(old);
        self.current == Some(old)
    }

    pub fn sync(&mut self) {
        self.reconcile_desktop();
    }

    fn observe(&mut self, id: Id) -> TileState {
        let state = native::tile_state(id);
        match state {
            TileState::Expanded => {
                self.expanded.insert(id);
            }
            TileState::Eligible => {
                self.expanded.remove(&id);
            }
            TileState::Excluded => {}
        }
        state
    }

    pub fn reconcile_desktop(&mut self) {
        if !self.settings.enabled {
            return;
        }
        let foreground = native::foreground();
        // A paused or inactive workspace can miss DESTROY, or an HWND can be
        // reused. Identity markers survive cloaking and distinguish the cases.
        let tracked: Vec<_> = self.windows.keys().copied().collect();
        for id in tracked {
            if !self.windows[&id].original.matches(id) {
                self.forget(id);
            } else {
                match self.desktops.membership(id) {
                    DesktopMembership::Other(key) | DesktopMembership::Current(key) => {
                        self.migrate(id, key);
                    }
                    DesktopMembership::Unknown => self.needs_desktop_retry = true,
                }
            }
        }
        let inventory = native::enumerate();
        self.expanded.retain(|id| inventory.contains(id));
        let visible: Vec<_> = inventory
            .into_iter()
            .filter_map(|id| {
                let state = self.observe(id);
                if state != TileState::Eligible {
                    // Some browsers restore the normal bounds before restoring
                    // their resizable frame. Recheck that transition even if
                    // the window was already fullscreen when Rustile started.
                    if state == TileState::Excluded
                        && self.expanded.contains(&id)
                        && !native::cloaked(id)
                        && matches!(self.desktops.membership(id), DesktopMembership::Current(_))
                    {
                        self.needs_desktop_retry = true;
                    }
                    return None;
                }
                match self.desktops.membership(id) {
                    DesktopMembership::Current(key) => Some((id, key)),
                    DesktopMembership::Other(_) => None,
                    DesktopMembership::Unknown => {
                        self.needs_desktop_retry = true;
                        None
                    }
                }
            })
            .collect();
        if let Some(&(_, desktop)) = visible
            .iter()
            .find(|&&(id, _)| id == foreground)
            .or(visible.first())
        {
            self.select(desktop);
        }
        self.settle_hidden();
        let resume = self
            .current
            .is_some_and(|key| self.windows.values().any(|w| w.desktop == key));
        self.remember_tile(foreground);
        for (id, desktop) in visible {
            if self.current != Some(desktop) || self.workspace.layout.contains(id) {
                continue;
            }
            self.rollback_resize();
            if resume {
                self.admit_new(id);
            } else {
                self.admit(id, None);
            }
        }
        if self.workspace.layout.contains(foreground) {
            self.workspace.last_foreground = Some(foreground);
        }
        self.remember_tile(foreground);
        self.place_tiles(true);
    }

    // A HIDE can precede both the Shell cloak and the membership change.
    // Leave the tile intact until the transition settles and Windows confirms
    // the window is still on this desktop and is actually hidden/ineligible.
    fn settle_hidden(&mut self) {
        let Some(area) = self.area else { return };
        for (id, _) in self.workspace.layout.placements(area) {
            if self.on_current(id) && !native::cloaked(id) && !native::eligible(id) {
                self.suspend(id);
            }
        }
    }

    fn suspend(&mut self, id: Id) {
        self.rollback_resize();
        if self.dragging == Some(id) {
            self.dragging = None;
        }
        self.workspace.remove(id);
        if let Some(window) = self.windows.get_mut(&id) {
            window.requested = None;
        }
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
        let proposed = if geometry {
            let area = tiling_area(updated)
                .ok_or_else(|| "Windows не сообщила размер рабочей области.".to_owned())?;
            // Validate the whole layout before changing the live model or any HWND.
            let gap = native::spacing_px(updated.gap);
            let refit = |layout: &Layout| {
                layout.clone().with_spacing(area, gap).ok_or_else(|| {
                    "С такими отступами окна не помещаются. Уменьшите отступ.".to_owned()
                })
            };
            let active = refit(&self.workspace.layout)?;
            let others: Result<Vec<_>, String> = self
                .parked
                .iter()
                .map(|(&desktop, workspace)| {
                    refit(&workspace.layout).map(|layout| (desktop, layout))
                })
                .collect();
            Some((area, active, others?))
        } else {
            None
        };
        persist()?;
        if let Some((area, active, others)) = proposed {
            self.area = Some(area);
            self.workspace.layout = active;
            for (desktop, layout) in others {
                self.parked.get_mut(&desktop).unwrap().layout = layout;
            }
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

    fn remember_tile(&mut self, id: Id) {
        if self.workspace.focus_history.last() != Some(&id) && self.workspace.layout.contains(id) {
            self.workspace
                .focus_history
                .retain(|&previous| previous != id);
            self.workspace.focus_history.push(id);
        }
    }

    fn admit_new(&mut self, id: Id) -> bool {
        if let Some(anchor) = self
            .workspace
            .focus_history
            .iter()
            .rev()
            .copied()
            .find(|&prior| self.can_act_on(prior))
        {
            return self.admit(id, Some(anchor));
        }
        let Some(area) = self.area else { return false };
        let tiles = self.workspace.layout.placements(area);
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
        let DesktopMembership::Current(desktop) = self.desktops.membership(id) else {
            return false;
        };
        if self.current.is_none() {
            self.select(desktop);
        }
        if self.current != Some(desktop)
            || self.workspace.layout.contains(id)
            || !native::eligible(id)
        {
            return false;
        }
        self.migrate(id, desktop);
        let mut insert = |minimum| match anchor {
            Some(anchor) => self.workspace.layout.insert_near(id, anchor, area, minimum),
            None => self.workspace.layout.insert_with_minimum(id, area, minimum),
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
            desktop,
            original,
            requested: None,
            repairs: 0,
        });
        true
    }

    pub fn event(&mut self, event: u32, id: Id) {
        if !self.settings.enabled {
            return;
        }
        let known = self.windows.get(&id);
        if event == EVENT_OBJECT_DESTROY || known.is_some_and(|w| !w.original.matches(id)) {
            let active_tile = self.workspace.layout.contains(id);
            self.forget(id);
            if active_tile {
                self.apply();
            }
            if event == EVENT_OBJECT_DESTROY {
                return;
            }
        }

        if event == EVENT_SYSTEM_MOVESIZEEND && self.dragging == Some(id) {
            self.dragging = None;
        }
        let state = self.observe(id);
        let eligible = state == TileState::Eligible;
        if !eligible
            && self.expanded.contains(&id)
            && matches!(
                event,
                EVENT_OBJECT_LOCATIONCHANGE
                    | EVENT_OBJECT_STATECHANGE
                    | EVENT_OBJECT_SHOW
                    | EVENT_SYSTEM_MINIMIZEEND
            )
        {
            self.needs_desktop_retry = true;
        }
        if !eligible && !self.tracks_window(id) && event != EVENT_SYSTEM_FOREGROUND {
            return;
        }
        let desktop = match self.desktops.membership(id) {
            DesktopMembership::Other(key) => {
                // A cloak for an existing desktop is visibility noise. Only an
                // actual change of the HWND's GUID releases its old tile.
                if self.migrate(id, key) {
                    self.apply();
                }
                return;
            }
            DesktopMembership::Unknown => {
                self.needs_desktop_retry |= eligible || self.tracks_window(id);
                return;
            }
            DesktopMembership::Current(key) => key,
        };
        let left_active = self.migrate(id, desktop);
        if !eligible || event == EVENT_OBJECT_CLOAKED {
            if self.current == Some(desktop) {
                if event == EVENT_SYSTEM_FOREGROUND {
                    self.workspace.last_foreground = Some(id);
                }
                // Fullscreen, maximized and minimized windows release their
                // tile. An ordinary HIDE may still be a shell desktop switch.
                if self.workspace.layout.contains(id)
                    && !eligible
                    && event != EVENT_OBJECT_CLOAKED
                    && !native::cloaked(id)
                {
                    if state == TileState::Expanded || native::minimized_or_maximized(id) {
                        self.suspend(id);
                        self.apply();
                    } else {
                        self.needs_desktop_retry = true;
                    }
                }
            } else if left_active {
                self.apply();
            }
            return;
        }

        let switched = self.current != Some(desktop);
        if switched && self.current.is_some() && !self.parked.contains_key(&desktop) {
            // The first UNCLOAK on a new desktop may only expose one of many
            // windows. Build its initial grid from the settled inventory in
            // one pass, so the first window does not briefly fill the monitor.
            self.needs_desktop_retry = true;
            return;
        }
        self.select(desktop);
        if switched {
            // Other HWNDs might have shown up before this desktop's first
            // resolvable foreground/UNCLOAK event, or may still be animating.
            self.needs_desktop_retry = true;
        }
        if event == EVENT_SYSTEM_FOREGROUND {
            self.workspace.last_foreground = Some(id);
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
        if self.dragging == Some(id) {
            return;
        }
        if !self.workspace.layout.contains(id) {
            self.rollback_resize();
            self.admit_new(id);
            self.apply();
        } else if event == EVENT_SYSTEM_MOVESIZEEND
            || (matches!(event, EVENT_OBJECT_LOCATIONCHANGE | EVENT_SYSTEM_FOREGROUND)
                && native::is_snapped(id))
        {
            if let Some(window) = self.windows.get_mut(&id) {
                window.requested = None;
                window.original.refresh_frame();
            }
            self.apply();
        } else if switched || event == EVENT_OBJECT_UNCLOAKED {
            if let Some(window) = self.windows.get_mut(&id)
                && window
                    .requested
                    .is_some_and(|rect| !window.original.is_at(rect))
            {
                window.requested = None;
            }
            self.apply();
        } else if matches!(
            event,
            EVENT_OBJECT_LOCATIONCHANGE
                | EVENT_OBJECT_STATECHANGE
                | EVENT_OBJECT_SHOW
                | EVENT_SYSTEM_FOREGROUND
                | EVENT_SYSTEM_MINIMIZEEND
        ) && let Some(window) = self.windows.get_mut(&id)
            && window
                .requested
                .is_none_or(|rect| window.frame_drifted(rect))
        {
            // Check again after WinEvents settle. An older LOCATIONCHANGE can
            // arrive while a newer placement is still in the target's queue.
            self.needs_desktop_retry = true;
        }
        if self.workspace.last_foreground == Some(id) {
            self.remember_tile(id);
        }
    }

    fn apply(&mut self) {
        self.place_tiles(false);
    }

    fn place_tiles(&mut self, verify: bool) {
        let Some(area) = self.area else { return };
        for (id, rect) in self.workspace.layout.placements(area) {
            let Some(window) = self.windows.get_mut(&id) else {
                continue;
            };
            // Window positioning is asynchronous. Compare the last enqueued
            // target, so Esc also supersedes a preview still in the target's queue.
            if self.dragging == Some(id)
                || (!verify && window.requested == Some(rect))
                || self.desktops.membership(id) != DesktopMembership::Current(window.desktop)
            {
                continue;
            }
            if window.requested != Some(rect) {
                if window.original.place(rect) {
                    window.requested = Some(rect);
                    window.repairs = 0;
                }
            } else if window.frame_drifted(rect) && native::eligible(id) {
                window.original.refresh_frame();
                if !window.original.is_at(rect) {
                    window.repairs += 1;
                    window.original.place(rect);
                    self.needs_desktop_retry |= window.repairs < 2;
                }
            }
        }
    }

    fn can_act_on(&self, target: Id) -> bool {
        self.settings.enabled
            && self.workspace.layout.contains(target)
            && self.on_current(target)
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
        let tiles = self.workspace.layout.placements(area);
        if tiles.len() < 2 {
            return false;
        }
        let Some((_, rect)) = tiles.into_iter().find(|(id, _)| *id == target) else {
            return false;
        };
        self.session = Some(ResizeSession {
            target,
            before: self.workspace.layout.clone(),
        });
        self.hud.show(target, rect, None, &self.settings);
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
        if self.can_act_on(id) {
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
        if let Some(edge) = self.workspace.layout.resize(target, dir, shrink, area) {
            self.apply();
            if let Some((_, rect)) = self
                .workspace
                .layout
                .placements(area)
                .into_iter()
                .find(|(id, _)| *id == target)
            {
                self.hud.show(target, rect, Some(edge), &self.settings);
            }
        }
    }

    fn rollback_resize(&mut self) {
        if let Some(session) = self.session.take() {
            self.hud.hide();
            self.workspace.layout = session.before;
        }
    }

    pub fn finish_mode(&mut self, commit: bool) {
        if self.session.is_none() {
            return;
        }
        if commit {
            self.session = None;
            self.hud.hide();
        } else {
            self.rollback_resize();
            self.apply();
        }
    }

    pub fn move_window(&mut self, target: Id, direction: Direction) {
        if let Some(area) = self.area
            && self.session.is_none()
            && self.can_act_on(target)
            && self.workspace.layout.move_window(target, direction, area)
        {
            self.apply();
        }
    }

    pub fn focus(&mut self, direction: Direction) {
        if !self.settings.enabled {
            return;
        }
        let Some(area) = self.area else { return };
        let from = native::foreground();
        if self.can_act_on(from)
            && let Some(id) = self.workspace.layout.neighbor(from, direction, area)
            && self.can_act_on(id)
        {
            native::focus(id);
        }
    }

    pub fn shutdown(&mut self) {
        self.session = None;
        self.outline.hide();
        self.hud.hide();
        for (id, window) in self.windows.drain() {
            if matches!(self.desktops.membership(id), DesktopMembership::Current(_)) {
                native::restore(id, &window.original);
            }
        }
        self.workspace = Workspace::new(native::spacing_px(self.settings.gap));
        self.parked.clear();
        self.expanded.clear();
        self.current = None;
        self.needs_desktop_retry = false;
        self.dragging = None;
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
