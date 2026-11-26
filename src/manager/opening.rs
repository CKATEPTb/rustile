use super::{
    DesktopId, DesktopMembership, EVENT_OBJECT_LOCATIONCHANGE, HashMap, Id, Layout, Manager,
    Original, Rect, native,
};
use std::time::{Duration, Instant};

const PREVIEW_LIFETIME: Duration = Duration::from_secs(2);
const SHOW_RETRY_MS: u32 = 16;

struct Prepared {
    desktop: DesktopId,
    original: Original,
    outer: Rect,
    expires: Instant,
}

impl Prepared {
    fn cancel(&self) {
        self.original.restore_opening(self.outer);
    }
}

#[derive(Default)]
pub(super) struct Openings {
    prepared: HashMap<Id, Prepared>,
    retry: HashMap<Id, u8>,
}

pub(super) fn insert_tile(
    layout: &mut Layout,
    id: Id,
    anchor: Option<Id>,
    area: Rect,
    minimum: (i32, i32),
) -> bool {
    match anchor {
        Some(anchor) => layout.insert_near(id, anchor, area, minimum),
        None => layout.insert_with_minimum(id, area, minimum),
    }
}

impl Openings {
    pub(super) fn forget(&mut self, id: Id) {
        self.prepared.remove(&id);
        self.retry.remove(&id);
    }

    pub(super) fn cancel(&mut self, id: Id) {
        if let Some(prepared) = self.prepared.remove(&id) {
            prepared.cancel();
        }
        self.retry.remove(&id);
    }

    pub(super) fn cancel_if_visible(&mut self, id: Id) {
        if self.prepared.contains_key(&id) && !native::hidden_or_minimized(id) {
            self.cancel(id);
        }
    }

    pub(super) fn clear(&mut self) {
        for (_, prepared) in self.prepared.drain() {
            prepared.cancel();
        }
        self.retry.clear();
    }

    pub(super) fn leave_other_desktops(&mut self, current: DesktopId) {
        self.prepared.retain(|_, prepared| {
            if prepared.desktop == current {
                return true;
            }
            prepared.cancel();
            false
        });
    }

    pub(super) fn retry_show(&mut self, id: Id) {
        self.retry.entry(id).or_insert(4);
    }

    pub(super) fn take_for_admission(
        &mut self,
        id: Id,
        desktop: DesktopId,
    ) -> Option<(Original, Rect)> {
        self.retry.remove(&id);
        let mut prepared = self.prepared.remove(&id)?;
        if prepared.desktop == desktop && prepared.original.confirm_opening(prepared.outer) {
            Some((prepared.original, prepared.outer))
        } else {
            prepared.cancel();
            None
        }
    }
}

impl Manager {
    pub(super) fn prepare_opening(&mut self, id: Id, stage: native::OpeningStage) {
        if self.windows.contains_key(&id)
            || self.openings.prepared.contains_key(&id)
            || self.dragging.is_some()
        {
            return;
        }
        let Some(area) = self.area else { return };
        let Some(original) = native::capture_opening(id, stage) else {
            return;
        };
        let Some(desktop) = self.desktops.opening_desktop(id) else {
            return;
        };
        if self.current.is_some_and(|active| active != desktop) {
            return;
        }
        let anchor = self.insertion_anchor(area);
        let mut prospective = self.workspace.layout.clone();
        if anchor.is_none() && !prospective.is_empty() {
            return;
        }
        if !insert_tile(
            &mut prospective,
            id,
            anchor,
            area,
            original.minimum_frame_size(),
        ) {
            return;
        }
        let Some((_, tile)) = prospective
            .placements(area)
            .into_iter()
            .find(|&(window, _)| window == id)
        else {
            return;
        };
        if let Some(outer) = original.place_opening(tile, stage) {
            // Neighbors keep their full tiles until visible admission on a
            // confirmed desktop, even if Explorer registers the HWND late.
            self.openings.prepared.insert(
                id,
                Prepared {
                    desktop,
                    original,
                    outer,
                    expires: Instant::now() + PREVIEW_LIFETIME,
                },
            );
        }
    }

    pub(crate) fn next_opening_check(&self) -> Option<u32> {
        let expiry = self
            .openings
            .prepared
            .values()
            .map(|pending| pending.expires)
            .min()
            .map(|deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .max(1) as u32
            });
        expiry
            .into_iter()
            .chain((!self.openings.retry.is_empty()).then_some(SHOW_RETRY_MS))
            .min()
    }

    pub(crate) fn check_openings(&mut self) {
        // Retry only the new HWNDs whose SHOW preceded Shell registration.
        // The normal desktop reconciliation still handles longer transitions.
        for (id, remaining) in std::mem::take(&mut self.openings.retry) {
            if native::eligible(id) && !self.workspace.layout.contains(id) {
                if remaining > 1 && self.desktops.membership(id) == DesktopMembership::Unknown {
                    self.openings.retry.insert(id, remaining - 1);
                } else {
                    self.event(EVENT_OBJECT_LOCATIONCHANGE, id);
                }
            }
        }
        let now = Instant::now();
        self.openings.prepared.retain(|_, prepared| {
            if now < prepared.expires {
                return true;
            }
            prepared.cancel();
            false
        });
    }
}
