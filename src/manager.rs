use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_STATECHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART,
};

use crate::hud::{FocusOutline, ResizeHud};
use crate::layout::{Direction, Id, Layout, Rect, ResizeStops};
use crate::native::desktops::{DesktopId, DesktopMembership, Desktops};
use crate::native::{self, Cloak, Original, TileState};
use crate::settings::Settings;

mod mouse;
use mouse::MouseDrag;

#[cfg(test)]
mod desktop_test;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Resize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowControl {
    Close,
    ToggleMaximize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutTarget {
    Tile(Id),
    Expanded(Id),
}

impl ShortcutTarget {
    pub(crate) fn id(self) -> Id {
        match self {
            Self::Tile(id) | Self::Expanded(id) => id,
        }
    }
}

struct ResizeSession {
    target: Id,
    // Quick shortcuts share divider stops while the keyboard stays in normal mode.
    mode: Mode,
    stops: Option<ResizeStops>,
}

struct TrackedWindow {
    desktop: DesktopId,
    original: Original,
    requested: Option<Rect>,
    repairs: u8,
    // A former tile can remain floating until the monitor has room again.
    overflow: bool,
    before_maximize: Option<Box<MaximizedLayout>>,
}

// Peers fill the work area while this window is maximized. Restore the exact
// old splits only if the user has kept the resulting peer arrangement.
struct MaximizedLayout {
    previous: Layout,
    peers: Vec<(Id, Rect)>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct WorkArea {
    bounds: Rect,
    dpi: u32,
}

impl WorkArea {
    fn read() -> Option<Self> {
        Some(Self {
            bounds: native::work_area()?,
            dpi: native::system_dpi(),
        })
    }

    fn tiles(self, settings: Settings) -> Rect {
        self.bounds
            .inset_for_tiles(native::scaled_spacing(settings.padding, self.dpi))
    }
}

pub struct Manager {
    settings: Settings,
    desktops: Desktops,
    current: Option<DesktopId>,
    workspace: Workspace,
    parked: HashMap<DesktopId, Workspace>,
    // A Shell-selected foreground during the switch must not change which
    // destination tile the transferred window was meant to split.
    transfer_anchor: Option<(Id, DesktopId, Id)>,
    unconfirmed_focus: Cell<Option<(DesktopId, Id)>>,
    windows: HashMap<Id, TrackedWindow>,
    expanded: HashSet<Id>,
    pending_app_cloaks: HashSet<Id>,
    needs_desktop_retry: bool,
    work_area: Option<WorkArea>,
    area: Option<Rect>,
    dragging: Option<MouseDrag>,
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
        let work_area = WorkArea::read();
        Self {
            settings,
            desktops: Desktops::new(),
            current: None,
            workspace: Workspace::new(native::spacing_px(settings.gap)),
            parked: HashMap::new(),
            transfer_anchor: None,
            unconfirmed_focus: Cell::default(),
            windows: HashMap::new(),
            expanded: HashSet::new(),
            pending_app_cloaks: HashSet::new(),
            needs_desktop_retry: false,
            work_area,
            area: work_area.map(|work| work.tiles(settings)),
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
        self.pending_app_cloaks.clear();
        self.finish_mode();
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

    pub(crate) fn layout_desktop(&self) -> Option<DesktopId> {
        self.current
    }

    fn drag_target(&self) -> Option<Id> {
        self.dragging.as_ref().map(|drag| drag.target)
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
                self.finish_mode();
            }
            self.workspace.remove(id);
            if self.drag_target() == Some(id) {
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
        if self
            .transfer_anchor
            .is_some_and(|(target, _, anchor)| target == id || anchor == id)
        {
            self.transfer_anchor = None;
        }
        self.pending_app_cloaks.remove(&id);
        if self.drag_target() == Some(id) {
            self.dragging = None;
        }
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
        if self
            .transfer_anchor
            .is_some_and(|(target, home, anchor)| (target == id || anchor == id) && desktop != home)
        {
            self.transfer_anchor = None;
        }
        self.detach(id, old);
        let window = self.windows.get_mut(&id).unwrap();
        window.desktop = desktop;
        window.requested = None;
        window.before_maximize = None;
        self.remove_empty_desktop(old);
        self.current == Some(old)
    }

    fn transferable_on(&self, id: Id, desktop: DesktopId) -> bool {
        id != 0
            && native::window_enabled(id)
            && self.windows.get(&id).is_none_or(|w| w.original.matches(id))
            && matches!(
                native::tile_state(id),
                TileState::Eligible | TileState::Maximized
            )
            && self.desktops.membership(id) == DesktopMembership::Current(desktop)
    }

    pub(crate) fn transfer_target_is_valid(&self, id: Id, source: DesktopId) -> bool {
        native::foreground() == id && self.transferable_on(id, source) && native::foreground() == id
    }

    /// Called only after Explorer has confirmed the move. Keep the original
    /// HWND capture, and let normal admission place it when its desktop appears.
    pub(crate) fn transferred_window(&mut self, id: Id, destination: DesktopId) {
        if self
            .windows
            .get(&id)
            .is_some_and(|w| !w.original.matches(id))
        {
            self.forget(id);
            return;
        }
        let workspace = if self.current == Some(destination) {
            Some(&self.workspace)
        } else {
            self.parked.get(&destination)
        };
        self.transfer_anchor = workspace
            .and_then(|w| w.focus_history.last().copied())
            .filter(|&anchor| anchor != id && self.settings.enabled && !self.expanded.contains(&id))
            .map(|anchor| (id, destination, anchor));
        if self.migrate(id, destination) && self.settings.enabled {
            self.apply();
        }
    }

    pub fn sync(&mut self) {
        self.reconcile_desktop();
    }

    pub fn work_area_changed(&self) -> bool {
        self.settings.enabled && WorkArea::read().is_some_and(|work| self.work_area != Some(work))
    }

    fn update_work_area(&mut self, work: WorkArea) {
        if self.work_area == Some(work) {
            return;
        }
        self.finish_mode();
        let dpi_changed = self
            .work_area
            .is_some_and(|previous| previous.dpi != work.dpi);
        let area = work.tiles(self.settings);
        let gap = native::scaled_spacing(self.settings.gap, work.dpi);
        let windows = &mut self.windows;
        for window in windows.values_mut() {
            window.before_maximize = None;
        }
        for workspace in std::iter::once(&mut self.workspace).chain(self.parked.values_mut()) {
            let released = workspace.layout.refit_work_area(area, gap, |id| {
                if !dpi_changed {
                    return None;
                }
                let original = &mut windows.get_mut(&id)?.original;
                if !original.matches(id) {
                    return None;
                }
                original.refresh_frame();
                Some(original.minimum_frame_size())
            });
            for id in released {
                workspace.remove(id);
                if let Some(window) = windows.get_mut(&id) {
                    window.requested = None;
                    window.overflow = true;
                }
            }
        }
        self.work_area = Some(work);
        self.area = Some(area);
    }

    fn observe(&mut self, id: Id) -> TileState {
        let state = native::tile_state(id);
        match state {
            TileState::Maximized | TileState::Fullscreen => {
                self.expanded.insert(id);
            }
            TileState::Eligible => {
                self.expanded.remove(&id);
                self.pending_app_cloaks.remove(&id);
            }
            TileState::Excluded => {}
        }
        state
    }

    pub fn reconcile_desktop(&mut self) {
        self.reconcile_in(WorkArea::read());
    }

    fn reconcile_in(&mut self, work: Option<WorkArea>) {
        if !self.settings.enabled {
            return;
        }
        let Some(work) = work else { return };
        self.update_work_area(work);
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
                    DesktopMembership::Unknown => {
                        // A tray app can keep a hidden HWND without a Shell GUID
                        // until its next SHOW. Waiting longer will not resolve it.
                        self.needs_desktop_retry |= !native::hidden_or_minimized(id);
                    }
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
        self.settle_hidden(!visible.is_empty());
        let resume = self
            .current
            .is_some_and(|key| self.windows.values().any(|w| w.desktop == key));
        self.remember_tile(foreground);
        for (id, desktop) in visible {
            if self.current != Some(desktop)
                || self.workspace.layout.contains(id)
                || self.drag_target() == Some(id)
            {
                continue;
            }
            self.finish_mode();
            let admitted = if resume {
                self.admit_new(id)
            } else {
                self.admit(id, None)
            };
            if !admitted
                && let Some(window) = self.windows.get(&id)
                && window.overflow
            {
                window.original.fit_floating(work.bounds);
            }
        }
        if self.workspace.layout.contains(foreground) {
            self.workspace.last_foreground = Some(foreground);
        }
        self.remember_tile(foreground);
        self.place_tiles(true);
    }

    fn settle_hidden(&mut self, desktop_confirmed: bool) {
        let Some(area) = self.area else { return };
        let previous_cloaks = std::mem::take(&mut self.pending_app_cloaks);
        for (id, _) in self.workspace.layout.placements(area) {
            let on_current = match self.desktops.membership(id) {
                DesktopMembership::Current(key) if self.current == Some(key) => true,
                // Tray windows can lose their desktop GUID. A visible peer in
                // this workspace still identifies the active layout.
                DesktopMembership::Unknown if desktop_confirmed => false,
                _ => continue,
            };
            let mut maximized = false;
            let release = match native::cloak_state(id) {
                Cloak::None if on_current => {
                    let state = native::tile_state(id);
                    maximized = state == TileState::Maximized;
                    state != TileState::Eligible
                }
                Cloak::None => native::hidden_or_minimized(id),
                Cloak::Application if desktop_confirmed => {
                    if native::hidden_or_minimized(id) || previous_cloaks.contains(&id) {
                        true
                    } else {
                        // WS_VISIBLE can survive an app cloak. Give a returning
                        // desktop one settle interval to reveal the window.
                        self.pending_app_cloaks.insert(id);
                        self.needs_desktop_retry = true;
                        false
                    }
                }
                _ => false,
            };
            if release {
                self.suspend(id, maximized);
            }
        }
    }

    fn suspend(&mut self, id: Id, maximized: bool) {
        self.finish_mode();
        if self.drag_target() == Some(id) {
            self.dragging = None;
        }
        let previous = self
            .area
            .filter(|_| maximized)
            .map(|area| (area, self.workspace.layout.clone()));
        self.workspace.remove(id);
        if let Some(window) = self.windows.get_mut(&id) {
            window.requested = None;
            window.before_maximize = previous.map(|(area, layout)| {
                Box::new(MaximizedLayout {
                    previous: layout,
                    peers: self.workspace.layout.placements(area),
                })
            });
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
            let work = WorkArea::read()
                .ok_or_else(|| "Windows не сообщила размер рабочей области.".to_owned())?;
            let area = work.tiles(updated);
            // Validate the whole layout before changing the live model or any HWND.
            let gap = native::scaled_spacing(updated.gap, work.dpi);
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
            Some((work, area, active, others?))
        } else {
            None
        };
        persist()?;
        let system_changed = proposed
            .as_ref()
            .is_some_and(|(work, ..)| Some(*work) != self.work_area);
        if let Some((_, area, active, others)) = proposed {
            for window in self.windows.values_mut() {
                window.before_maximize = None;
            }
            self.area = Some(area);
            self.workspace.layout = active;
            for (desktop, layout) in others {
                self.parked.get_mut(&desktop).unwrap().layout = layout;
            }
        }
        self.settings = updated;
        if geometry || !updated.enabled {
            self.finish_mode();
        }
        if !updated.enabled {
            self.dragging = None;
            self.transfer_anchor = None;
            self.unconfirmed_focus.set(None);
            self.outline.hide();
        } else if resuming || system_changed {
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
        if let Some((_, _, anchor)) = self
            .transfer_anchor
            .take_if(|(target, home, _)| *target == id && Some(*home) == self.current)
            && self.can_act_on(anchor)
        {
            return self.admit(id, Some(anchor));
        }
        if let Some(window) = self.windows.get_mut(&id)
            && let Some(mut saved) = window.before_maximize.take()
            && let Some(area) = self.area
            && saved.peers == self.workspace.layout.placements(area)
        {
            window.original.refresh_frame();
            let minimum = window.original.minimum_frame_size();
            let gap = native::spacing_px(self.settings.gap);
            if saved
                .previous
                .refit_work_area(area, gap, |other| (other == id).then_some(minimum))
                .is_empty()
            {
                self.workspace.layout = saved.previous;
                return true;
            }
        }
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
                let tracked = entry.get_mut();
                tracked.original.refresh_frame();
                let admitted = insert(tracked.original.minimum_frame_size());
                if admitted {
                    tracked.overflow = false;
                }
                return admitted;
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
            overflow: false,
            before_maximize: None,
        });
        true
    }

    pub fn event(&mut self, event: u32, id: Id) {
        if !self.settings.enabled {
            return;
        }
        // Start and other shell surfaces can have no virtual desktop ID. A
        // foreground change still ends resize even if the tile regains focus
        // before this batch of events reaches refresh_focus.
        if event == EVENT_SYSTEM_FOREGROUND && self.session.as_ref().is_some_and(|s| s.target != id)
        {
            self.finish_mode();
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

        let finished_drag = self
            .dragging
            .take_if(|drag| event == EVENT_SYSTEM_MOVESIZEEND && drag.target == id);
        if finished_drag.is_some() {
            // END can coincide with minimizing or switching desktops. Settle
            // neighbors even if the target can no longer accept a placement.
            self.needs_desktop_retry = true;
        }
        let state = self.observe(id);
        let eligible = state == TileState::Eligible;
        if !eligible || event == EVENT_OBJECT_CLOAKED {
            self.hide_drop_for(id);
        }
        if event == EVENT_SYSTEM_FOREGROUND && self.drag_target() != Some(id) {
            self.cancel_mouse_move();
        }
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
                    if state == TileState::Fullscreen || native::minimized_or_maximized(id) {
                        self.suspend(id, state == TileState::Maximized);
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
        if event == EVENT_SYSTEM_MOVESIZESTART {
            self.start_mouse_drag(id);
            return;
        }
        if self.drag_target() == Some(id) {
            if event == EVENT_OBJECT_LOCATIONCHANGE {
                self.preview_mouse_drag(id);
            }
            return;
        }
        let was_tiled = self.workspace.layout.contains(id);
        if was_tiled && matches!(event, EVENT_OBJECT_SHOW | EVENT_SYSTEM_MINIMIZEEND) {
            // If every window hid, the first one back can still be at its old
            // coordinates. Its return now identifies the other hidden slots.
            self.needs_desktop_retry = true;
        }
        if !was_tiled {
            self.finish_mode();
            // A newly shown window can be the first visible peer after all
            // others hid to the tray. Reconcile their stale slots before retrying.
            if !self.admit_new(id) {
                self.needs_desktop_retry = true;
            }
            self.apply();
        } else if event == EVENT_SYSTEM_MOVESIZEEND
            && !switched
            && let Some(drag) = finished_drag
        {
            self.finish_mouse_drag(drag);
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
        self.refresh_drop_preview(true);
        let preview = self
            .dragging
            .as_ref()
            .and_then(|drag| drag.resized_layout(&self.workspace.layout, area));
        let layout = preview.as_ref().unwrap_or(&self.workspace.layout);
        let drag_target = self.drag_target();
        for (id, rect) in layout.placements(area) {
            let Some(window) = self.windows.get_mut(&id) else {
                continue;
            };
            // Compare with the last enqueued target since placement is asynchronous.
            if drag_target == Some(id)
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

    fn shortcut_target(&self, id: Id) -> Option<ShortcutTarget> {
        if !self.settings.enabled || !native::window_enabled(id) {
            return None;
        }
        if self.can_act_on(id) {
            return Some(ShortcutTarget::Tile(id));
        }
        // A window can already be maximized when Rustile starts, even on a
        // desktop with no normal tiles. Check its actual Shell membership.
        (native::maximized(id)
            && native::tile_state(id) == TileState::Maximized
            && matches!(self.desktops.membership(id), DesktopMembership::Current(_))
            && self.windows.get(&id).is_none_or(|w| w.original.matches(id)))
        .then_some(ShortcutTarget::Expanded(id))
    }

    pub(crate) fn accepts_foreground_shortcut(&mut self, target: Id) -> bool {
        if !self.settings.enabled || target == 0 || target != native::foreground() {
            return false;
        }
        // A key can arrive before the foreground/desktop notifications. If its
        // HWND belongs to a parked layout, confirm the switch with the Shell.
        if self
            .windows
            .get(&target)
            .is_some_and(|w| Some(w.desktop) != self.current)
            && matches!(self.desktops.membership(target), DesktopMembership::Current(home) if Some(home) != self.current)
        {
            self.reconcile_desktop();
        }
        if self.session.as_ref().is_some_and(|s| s.target != target) {
            self.finish_mode();
        }
        self.shortcut_target(target)
            .is_some_and(|window| window.id() == native::foreground())
    }

    pub(crate) fn control_window(
        &mut self,
        target: Id,
        control: WindowControl,
        foreground: impl Fn() -> Id,
    ) {
        // A paused layout may have never seen this window. Validate the HWND
        // from the physical press against Windows, independently of tile state.
        if target == 0 || target != foreground() {
            return;
        }
        let Some(window) = native::WindowMarker::for_window_control(target) else {
            return;
        };
        if !matches!(
            self.desktops.membership(target),
            DesktopMembership::Current(_)
        ) {
            return;
        }
        let maximized = native::maximized(target);
        if control == WindowControl::ToggleMaximize && !maximized && !native::can_maximize(target) {
            return;
        }
        self.finish_mode();
        if window.matches() && foreground() == target {
            match control {
                WindowControl::Close => native::close_window(target),
                WindowControl::ToggleMaximize => native::set_maximized(target, !maximized),
            }
        }
    }

    pub fn begin_resize(&mut self, target: Id) -> bool {
        let Some(area) = self.area else { return false };
        if self.mode() == Mode::Resize || self.dragging.is_some() || !self.can_act_on(target) {
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
            mode: Mode::Resize,
            stops: None,
        });
        self.hud.show(target, rect, None, &self.settings);
        true
    }

    pub fn mode(&self) -> Mode {
        self.session.as_ref().map_or(Mode::Normal, |s| s.mode)
    }

    pub fn refresh_resize_hint(&mut self, window: Id) {
        self.hud.refresh_appearance(window);
    }

    // Sample foreground once so the cue and the keyboard target agree, including
    // when a modal dialog receives focus while its owner is still tiled.
    pub fn refresh_focus(&mut self) -> Option<ShortcutTarget> {
        if !self.settings.enabled {
            return None;
        }
        let id = native::foreground();
        let target = self.shortcut_target(id);
        let eligible = matches!(target, Some(ShortcutTarget::Tile(_)));
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.target != id || !eligible)
        {
            self.finish_mode();
        }
        if eligible {
            self.workspace.last_foreground = Some(id);
            self.remember_tile(id);
            self.outline
                .show(id, self.settings.outline, self.settings.gap == 0);
        } else {
            self.outline.hide();
        }
        target
    }

    fn focusable_on(&self, id: Id, desktop: DesktopId) -> bool {
        let Some(owner) = native::focus_app_owner(id) else {
            return false;
        };
        if self
            .windows
            .get(&owner)
            .is_some_and(|window| !window.original.matches(owner))
        {
            return false;
        }
        let membership = match self.desktops.membership(id) {
            // Owned modal dialogs can inherit the disabled owner's desktop
            // without a desktop ID of their own. Activate the dialog itself.
            DesktopMembership::Unknown if id != owner => self.desktops.membership(owner),
            known => known,
        };
        membership == DesktopMembership::Current(desktop)
    }

    pub(crate) fn desktop_foreground_on(
        &self,
        id: Id,
        desktop: DesktopId,
    ) -> native::DesktopForeground {
        use native::DesktopForeground::*;
        let state = native::desktop_foreground(id);
        if state != Preserve {
            return state;
        }
        match self.desktops.membership(id) {
            DesktopMembership::Current(home) | DesktopMembership::Other(home)
                if native::desktops::occupancy::is_pinned(home) =>
            {
                Preserve
            }
            // Explorer may leave the outgoing HWND in the foreground after it
            // already reports that view on a different desktop.
            DesktopMembership::Other(home) if home != desktop => Vacant,
            DesktopMembership::Other(_) => Transient,
            DesktopMembership::Current(home) if home != desktop => Transient,
            DesktopMembership::Unknown
                if self.windows.get(&id).is_some_and(|w| w.desktop != desktop) =>
            {
                Transient
            }
            _ => Preserve,
        }
    }

    /// A queued activation needs confirmation from the actual foreground HWND.
    pub(crate) fn restore_desktop_focus(
        &self,
        desktop: DesktopId,
        uninterrupted: impl Fn() -> bool,
    ) -> bool {
        if !uninterrupted() {
            self.unconfirmed_focus.set(None);
            return true;
        }
        self.restore_desktop_focus_with(desktop, native::foreground, |id, previous| {
            native::focus_after_desktop_change(id, previous, || {
                self.focusable_on(id, desktop)
                    && self.desktop_foreground_on(previous, desktop)
                        == native::DesktopForeground::Vacant
                    && uninterrupted()
            })
        })
    }

    pub(crate) fn restore_transferred_focus(
        &self,
        identity: &native::TransferIdentity,
        desktop: DesktopId,
        uninterrupted: impl Fn() -> bool,
    ) -> bool {
        if !uninterrupted() || !identity.matches() {
            return true;
        }
        let id = identity.id();
        self.restore_transferred_focus_with(id, desktop, native::foreground, |previous| {
            native::focus_after_desktop_change(id, previous, || {
                self.transferable_on(id, desktop)
                    && self.transfer_foreground_can_yield(previous, desktop)
                    && identity.matches()
                    && uninterrupted()
            })
        })
    }

    fn transfer_foreground_can_yield(&self, previous: Id, desktop: DesktopId) -> bool {
        match self.desktop_foreground_on(previous, desktop) {
            native::DesktopForeground::Vacant => true,
            native::DesktopForeground::Transient => false,
            native::DesktopForeground::Preserve => {
                self.transferable_on(previous, desktop) && native::app_input_idle(previous)
            }
        }
    }

    fn restore_transferred_focus_with(
        &self,
        id: Id,
        desktop: DesktopId,
        foreground: impl Fn() -> Id,
        activate: impl FnOnce(Id) -> bool,
    ) -> bool {
        if !self.transferable_on(id, desktop) {
            return false;
        }
        let previous = foreground();
        if previous == id {
            return true;
        }
        if !self.transfer_foreground_can_yield(previous, desktop) {
            // A dialog or menu on the destination may retain legitimate input.
            return self.desktop_foreground_on(previous, desktop)
                == native::DesktopForeground::Preserve;
        }
        foreground() == previous && activate(previous) && foreground() == id
    }

    fn restore_desktop_focus_with(
        &self,
        desktop: DesktopId,
        foreground: impl Fn() -> Id,
        mut activate: impl FnMut(Id, Id) -> bool,
    ) -> bool {
        if !self.settings.enabled {
            return true;
        }
        let previous = foreground();
        match self.desktop_foreground_on(previous, desktop) {
            native::DesktopForeground::Transient => return false,
            native::DesktopForeground::Preserve => {
                self.unconfirmed_focus.set(None);
                return true;
            }
            native::DesktopForeground::Vacant => {}
        }
        let workspace = if self.current == Some(desktop) {
            Some(&self.workspace)
        } else {
            self.parked.get(&desktop)
        };
        let mut candidates = Vec::new();
        if let Some(workspace) = workspace {
            candidates.extend(workspace.focus_history.iter().rev().copied());
            if let Some(area) = self.area {
                candidates.extend(
                    workspace
                        .layout
                        .placements(area)
                        .into_iter()
                        .map(|(id, _)| id),
                );
            }
        }
        // A desktop with only maximized windows has no tiles to search.
        candidates.extend(native::enumerate().into_iter().rev());
        let mut seen = HashSet::new();
        candidates.retain(|&id| seen.insert(id));
        if let Some((home, last)) = self.unconfirmed_focus.take()
            && home == desktop
            && let Some(index) = candidates.iter().position(|&id| id == last)
        {
            candidates.rotate_left(index + 1);
        }
        for id in candidates {
            if !self.focusable_on(id, desktop) {
                continue;
            }
            if foreground() != previous {
                return false;
            }
            let submitted = activate(id, previous);
            let actual = foreground();
            if actual == id {
                return true;
            }
            if actual != previous {
                return false;
            }
            if submitted {
                // Let Windows deliver an accepted request before trying another
                // HWND. A later retry can advance if focus never arrived.
                self.unconfirmed_focus.set(Some((desktop, id)));
                return false;
            }
        }
        false
    }

    pub fn resize(&mut self, active: Id, dir: Direction, reverse: bool) {
        if self.mode() == Mode::Resize {
            self.resize_step(active, dir, reverse);
        }
    }

    pub fn quick_resize(&mut self, active: Id, dir: Direction, reverse: bool) {
        if self.mode() != Mode::Normal || self.dragging.is_some() {
            return;
        }
        if self.session.as_ref().is_none_or(|s| s.target != active) {
            self.session = Some(ResizeSession {
                target: active,
                mode: Mode::Normal,
                stops: None,
            });
        }
        self.resize_step(active, dir, reverse);
    }

    fn resize_step(&mut self, active: Id, dir: Direction, reverse: bool) {
        let eligible = self.can_act_on(active);
        let (Some(session), Some(area)) = (&mut self.session, self.area) else {
            return;
        };
        let target = session.target;
        if active != target || !eligible {
            self.finish_mode();
            return;
        }
        let show_hint = session.mode == Mode::Resize;
        if let Some(edge) =
            self.workspace
                .layout
                .resize(target, dir, reverse, area, &mut session.stops)
        {
            self.apply();
            if show_hint
                && let Some((_, rect)) = self
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

    pub fn finish_mode(&mut self) {
        if self.session.take().is_some_and(|s| s.mode == Mode::Resize) {
            self.hud.hide();
        }
    }

    pub fn move_window(&mut self, target: Id, direction: Direction) {
        if let Some(area) = self.area
            && self.mode() == Mode::Normal
            && self.dragging.is_none()
            && self.can_act_on(target)
            && self.workspace.layout.move_window(target, direction, area)
        {
            self.finish_mode();
            self.apply();
        }
    }

    pub fn focus(&mut self, from: Id, direction: Direction) {
        let Some(area) = self.area else { return };
        if self.can_act_on(from)
            && let Some(id) = self.workspace.layout.neighbor(from, direction, area)
            && self.can_act_on(id)
            && native::foreground() == from
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
        self.transfer_anchor = None;
        self.expanded.clear();
        self.pending_app_cloaks.clear();
        self.current = None;
        self.needs_desktop_retry = false;
        self.dragging = None;
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
