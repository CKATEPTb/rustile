use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::layout::Id;
use crate::native::desktops::{DesktopId, occupancy, shell::DesktopShell};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Inventory {
    order: Vec<DesktopId>,
    current: DesktopId,
    occupied: HashSet<DesktopId>,
}

impl Inventory {
    fn current_is_empty_tail(&self) -> bool {
        from_current(&self.order, self.current)
            .is_some_and(|tail| tail.iter().all(|id| !self.occupied.contains(id)))
    }

    fn empty_spare(&self, preferred: Option<DesktopId>) -> Option<DesktopId> {
        let empty = |id: &DesktopId| !self.occupied.contains(id);
        preferred
            .filter(|id| self.order.contains(id) && empty(id))
            .or_else(|| self.order.iter().rev().find(|id| empty(id)).copied())
    }

    fn read(shell: &DesktopShell) -> Option<Self> {
        let current = shell.current()?;
        let order = shell.list()?;
        Self::for_order(shell, current, order)
    }

    fn for_order(shell: &DesktopShell, current: DesktopId, order: Vec<DesktopId>) -> Option<Self> {
        if !order.contains(&current) {
            return None;
        }
        let occupied = occupancy::occupied(&order)?;
        // Task View can move, remove or activate a desktop during EnumWindows.
        (shell.current()? == current && shell.list()? == order).then_some(Self {
            order,
            current,
            occupied,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Create,
    MoveToEnd(DesktopId),
    Remove {
        desktop: DesktopId,
        fallback: DesktopId,
    },
}

impl Change {
    fn position_spare(order: &[DesktopId], spare: Option<DesktopId>) -> Option<Self> {
        match spare {
            None => Some(Self::Create),
            Some(id) if order.last() != Some(&id) => Some(Self::MoveToEnd(id)),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Idle,
    Verify,
    RetryAfter(u32),
    Apply(Change),
}

#[derive(Default)]
struct Policy {
    spare: Option<DesktopId>,
    last_current: Option<(DesktopId, bool)>,
    awaiting_departure: Option<DesktopId>,
    pending: Option<(Inventory, Change)>,
    issued: Option<(Inventory, Change)>,
    failures: u8,
    retry_at: Option<Instant>,
}

impl Policy {
    fn apply(&mut self, shell: &DesktopShell, change: Change) -> bool {
        let applied = match change {
            Change::Create => {
                self.spare = shell.create();
                self.spare.is_some()
            }
            Change::MoveToEnd(spare) => {
                self.spare = Some(spare);
                shell.move_to_end(spare)
            }
            Change::Remove { desktop, fallback } => shell.remove(desktop, fallback),
        };
        if !applied {
            self.failed();
        }
        applied
    }

    fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let seconds = match self.failures {
            1 => 1,
            2 => 3,
            _ => 15,
        };
        self.retry_at = Some(Instant::now() + Duration::from_secs(seconds));
    }

    fn observe(&mut self, inventory: Inventory) -> Decision {
        let empty = |id: &DesktopId| !inventory.occupied.contains(id);
        let current = inventory.current;
        let occupied = !empty(&current);
        let empty_tail = inventory.current_is_empty_tail();
        if occupied || self.awaiting_departure != Some(current) {
            self.awaiting_departure = None;
        }
        if !occupied && self.last_current == Some((current, true)) {
            self.awaiting_departure = Some(current);
        }
        self.last_current = Some((current, occupied));
        self.spare = if empty_tail || (!occupied && self.awaiting_departure.is_none()) {
            Some(current)
        } else if self.awaiting_departure == Some(current) && self.spare == Some(current) {
            // A window may open farther right while tail removal is pending.
            // Keep the vacated current slot in place if another spare exists.
            inventory
                .order
                .iter()
                .rev()
                .copied()
                .find(|&id| id != current && empty(&id))
                .or(Some(current))
        } else {
            inventory.empty_spare(self.spare)
        };
        let change = if empty_tail
            && let Some(&last) = inventory.order.last().filter(|&&id| id != current)
        {
            // The current desktop already occupies the correct slot. Trim
            // the unused tail without moving it or waiting for departure.
            Some(Change::Remove {
                desktop: last,
                fallback: current,
            })
        } else {
            Change::position_spare(&inventory.order, self.spare).or_else(|| {
                let spare = self.spare?;
                inventory
                    .order
                    .iter()
                    .copied()
                    .find(|&id| id != current && id != spare && empty(&id))
                    .map(|desktop| Change::Remove {
                        desktop,
                        fallback: spare,
                    })
            })
        };
        let Some(change) = change else {
            self.pending = None;
            self.issued = None;
            self.failures = 0;
            self.retry_at = None;
            return Decision::Idle;
        };
        let proposal = (inventory, change);
        if self.issued.as_ref() == Some(&proposal) {
            let Some(deadline) = self.retry_at else {
                return Decision::Idle;
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                // After repeated failures, only a new external event wakes the loop.
                return if self.failures < 3 {
                    Decision::RetryAfter(remaining.as_millis().max(1) as u32)
                } else {
                    Decision::Idle
                };
            }
            self.issued = None;
            self.retry_at = None;
            self.pending = Some(proposal);
            return Decision::Verify;
        }
        if self.issued.is_some() || self.pending.as_ref().is_some_and(|old| old != &proposal) {
            self.failures = 0;
            self.retry_at = None;
        }
        self.issued = None;
        if self.pending.as_ref() != Some(&proposal) {
            self.pending = Some(proposal);
            return Decision::Verify;
        }
        self.issued = self.pending.take();
        Decision::Apply(change)
    }
}

fn neighbor(order: &[DesktopId], current: DesktopId, next: bool) -> Option<DesktopId> {
    let index = order.iter().position(|id| *id == current)?;
    order
        .get(if next {
            index.checked_add(1)?
        } else {
            index.checked_sub(1)?
        })
        .copied()
}

fn from_current(order: &[DesktopId], current: DesktopId) -> Option<&[DesktopId]> {
    let index = order.iter().position(|&id| id == current)?;
    Some(&order[index..])
}

fn forward_neighbor(
    tail: &[DesktopId],
    occupied: impl FnOnce(&[DesktopId]) -> Option<bool>,
) -> Option<DesktopId> {
    let next = *tail.get(1)?;
    // Explorer can list the redundant spare briefly after the last close.
    // A live window here or further right is evidence that Next is valid.
    (occupied(tail)?).then_some(next)
}

fn numbered_target(
    order: &[DesktopId],
    current: DesktopId,
    number: usize,
    occupied: impl FnOnce(&[DesktopId]) -> Option<bool>,
) -> Option<DesktopId> {
    let from = order.iter().position(|&id| id == current)?;
    let to = number.checked_sub(1)?.min(order.len() - 1);
    // The selected empty desktop becomes the last one as soon as its
    // last window closes, even while Explorer still lists the previous spare.
    if to == from || (to > from && !occupied(&order[from..])?) {
        return None;
    }
    Some(order[to])
}

pub(super) const EVENT_BATCH_MS: u32 = 40;
const CONFIRM_MS: u32 = 60;
const RETRY_MS: u32 = 350;

pub(super) struct WindowTransfer {
    pub(super) from: DesktopId,
    pub(super) to: DesktopId,
    pub(super) followed: bool,
}

impl WindowTransfer {
    fn perform(
        from: DesktopId,
        to: DesktopId,
        move_window: impl FnOnce() -> bool,
        current: impl FnOnce() -> Option<DesktopId>,
        uninterrupted: impl Fn() -> bool,
        switch: impl FnOnce(DesktopId) -> bool,
    ) -> Option<Self> {
        if from == to || !uninterrupted() || !move_window() {
            return None;
        }
        // A committed move is final even if the user interrupts the follow,
        // Explorer changes desktops itself, or the switch RPC fails.
        let followed = match current() {
            Some(selected) if uninterrupted() => selected == to || (selected == from && switch(to)),
            _ => false,
        };
        Some(Self { from, to, followed })
    }
}

pub(super) struct Workspaces {
    pub(super) supported: bool,
    shell: Option<DesktopShell>,
    reconnect_at: Instant,
    policy: Policy,
    incomplete: u8,
}

impl Workspaces {
    pub(super) fn new() -> Self {
        let shell = DesktopShell::connect();
        Self {
            supported: shell.is_some() || DesktopShell::supported(),
            shell,
            reconnect_at: Instant::now() + Duration::from_secs(5),
            policy: Policy::default(),
            incomplete: 0,
        }
    }

    pub(super) fn available(&self) -> bool {
        self.shell.is_some()
    }

    pub(super) fn current_desktop(&self) -> Option<DesktopId> {
        self.shell.as_ref()?.current()
    }

    pub(super) fn activity(&mut self) -> bool {
        self.incomplete = 0;
        self.supported && (self.available() || Instant::now() >= self.reconnect_at)
    }

    // Returns a delay only while Explorer is settling or a change needs confirmation.
    pub(super) fn reconcile(&mut self) -> Option<u32> {
        if !self.supported {
            return None;
        }
        if self.shell.is_none() && Instant::now() >= self.reconnect_at {
            self.shell = DesktopShell::connect();
            self.reconnect_at = Instant::now() + Duration::from_secs(5);
        }
        let Some(shell) = &self.shell else {
            // Autostart and Explorer restarts can precede Shell COM availability.
            // No window enumeration occurs until Explorer has actually connected.
            return Some(5000);
        };
        let Some(inventory) = Inventory::read(shell) else {
            self.policy.pending = None;
            if shell.current().is_none() {
                self.shell = None;
                self.reconnect_at = Instant::now() + Duration::from_secs(5);
                return Some(5000);
            }
            self.incomplete = self.incomplete.saturating_add(1);
            return (self.incomplete <= 3).then(|| RETRY_MS * u32::from(self.incomplete));
        };
        self.incomplete = 0;
        match self.policy.observe(inventory) {
            Decision::Idle => None,
            Decision::Verify => Some(CONFIRM_MS),
            Decision::RetryAfter(delay) => Some(delay),
            Decision::Apply(change) => {
                self.policy.apply(shell, change);
                // Even after an RPC error, the Shell may have committed the change.
                // Observe the result before considering another mutation.
                Some(CONFIRM_MS)
            }
        }
    }

    fn relative_target(&mut self, current: DesktopId, next: bool) -> Option<DesktopId> {
        let shell = self.shell.as_ref()?;
        let order = shell.list()?;
        if !next {
            return neighbor(&order, current, false);
        }
        let tail = from_current(&order, current)?;
        if tail.len() > 1 {
            let target = forward_neighbor(tail, |ids| occupancy::any_occupied(&order, ids))?;
            if shell.current()? != current || shell.list()? != order {
                return None;
            }
            return Some(target);
        }
        let inventory = Inventory::for_order(shell, current, order)?;
        if !inventory.occupied.contains(&current) {
            return None;
        }
        let change =
            Change::position_spare(&inventory.order, inventory.empty_spare(self.policy.spare))?;
        let proposal = (inventory, change);
        if self.policy.issued.as_ref() == Some(&proposal) {
            return None;
        }
        self.policy.issued = Some(proposal);
        self.policy.pending = None;
        self.policy.retry_at = None;
        self.policy.failures = 0;
        if self.policy.apply(shell, change) {
            return self.policy.spare;
        }
        None
    }

    pub(super) fn navigate(&mut self, next: bool) -> Option<(DesktopId, DesktopId)> {
        let current = self.current_desktop()?;
        let target = self.relative_target(current, next)?;
        let shell = self.shell.as_ref()?;
        (shell.current()? == current && shell.switch(target)).then_some((current, target))
    }

    pub(super) fn transfer(
        &mut self,
        window: Id,
        next: bool,
        valid: impl Fn(DesktopId) -> bool,
        uninterrupted: impl Fn() -> bool,
    ) -> Option<WindowTransfer> {
        let from = self.current_desktop()?;
        if !uninterrupted() || !valid(from) {
            return None;
        }
        let to = self.relative_target(from, next)?;
        let shell = self.shell.as_ref()?;
        if neighbor(&shell.list()?, from, next) != Some(to) {
            return None;
        }
        WindowTransfer::perform(
            from,
            to,
            || shell.move_window(window, from, to, || valid(from) && uninterrupted()),
            || shell.current(),
            &uninterrupted,
            |target| shell.switch(target),
        )
    }

    pub(super) fn navigate_to(&self, number: usize) -> Option<(DesktopId, DesktopId)> {
        let shell = self.shell.as_ref()?;
        let current = shell.current()?;
        let order = shell.list()?;
        let target = numbered_target(&order, current, number, |tail| {
            occupancy::any_occupied(&order, tail)
        })?;
        if shell.current()? != current || shell.list()? != order {
            return None;
        }
        shell.switch(target).then_some((current, target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory(order: &[u128], current: u128, occupied: &[u128]) -> Inventory {
        Inventory {
            order: order.iter().copied().map(DesktopId).collect(),
            current: DesktopId(current),
            occupied: occupied.iter().copied().map(DesktopId).collect(),
        }
    }

    #[test]
    fn first_window_creates_one_spare_without_activating_it() {
        let mut policy = Policy::default();
        assert_eq!(policy.observe(inventory(&[1], 1, &[])), Decision::Idle);
        let full = inventory(&[1], 1, &[1]);
        assert_eq!(policy.observe(full.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(full.clone()),
            Decision::Apply(Change::Create)
        );
        assert_eq!(policy.observe(full), Decision::Idle);
        assert_eq!(policy.observe(inventory(&[1, 2], 1, &[1])), Decision::Idle);
        let full = inventory(&[1, 2], 2, &[1, 2]);
        assert_eq!(policy.observe(full.clone()), Decision::Verify);
        assert_eq!(policy.observe(full), Decision::Apply(Change::Create));
    }

    #[test]
    fn closing_the_last_window_on_four_retires_five_and_keeps_four_selected() {
        let mut policy = Policy::default();
        let order = [1, 2, 3, 4, 5];
        let reopened = inventory(&order, 4, &[1, 2, 3, 4]);
        let closed = inventory(&order, 4, &[1, 2, 3]);
        assert_eq!(policy.observe(reopened.clone()), Decision::Idle);
        assert_eq!(policy.observe(closed.clone()), Decision::Verify);
        assert_eq!(policy.observe(reopened), Decision::Idle);
        assert_eq!(policy.observe(closed.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(closed),
            Decision::Apply(Change::Remove {
                desktop: DesktopId(5),
                fallback: DesktopId(4),
            })
        );
        assert_eq!(
            policy.observe(inventory(&[1, 2, 3, 4], 4, &[1, 2, 3])),
            Decision::Idle
        );
    }

    #[test]
    fn startup_and_multiple_empty_successors_trim_from_the_end() {
        let mut policy = Policy::default();
        for last in (5..=7).rev() {
            let order: Vec<_> = (1..=last).collect();
            let state = inventory(&order, 4, &[1, 2, 3]);
            assert_eq!(policy.observe(state.clone()), Decision::Verify);
            assert_eq!(
                policy.observe(state),
                Decision::Apply(Change::Remove {
                    desktop: DesktopId(last),
                    fallback: DesktopId(4),
                })
            );
        }
        assert_eq!(
            policy.observe(inventory(&[1, 2, 3, 4], 4, &[1, 2, 3])),
            Decision::Idle
        );
    }

    #[test]
    fn a_successor_gaining_a_window_cancels_tail_cleanup_and_preserves_the_current_slot() {
        let mut policy = Policy::default();
        let order = [1, 2, 3, 4, 5, 6];
        let used = inventory(&order, 4, &[1, 2, 3, 4, 5]);
        assert_eq!(policy.observe(used), Decision::Idle);
        let tail = inventory(&order, 4, &[1, 2, 3]);
        assert_eq!(policy.observe(tail), Decision::Verify);
        let fifth_reopened = inventory(&order, 4, &[1, 2, 3, 5]);
        assert_eq!(policy.observe(fifth_reopened), Decision::Idle);
        assert_eq!(policy.spare, Some(DesktopId(6)));
    }

    #[test]
    fn a_window_appearing_or_user_returning_during_confirmation_cancels_removal() {
        let mut policy = Policy::default();
        let empty = inventory(&[1, 2, 3], 1, &[1]);
        assert_eq!(policy.observe(empty.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(inventory(&[1, 2, 3], 1, &[1, 2])),
            Decision::Idle
        );
        assert_eq!(policy.observe(empty.clone()), Decision::Verify);
        // Returning to desktop 2 keeps its identity as the new trailing spare.
        let returned = inventory(&[1, 2, 3], 2, &[1]);
        assert_eq!(policy.observe(returned.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(returned),
            Decision::Apply(Change::Remove {
                desktop: DesktopId(3),
                fallback: DesktopId(2),
            })
        );
    }

    #[test]
    fn manually_reordered_spare_returns_to_the_end_even_when_selected() {
        for current in [1, 2, 3] {
            for displaced in [[2, 3, 1], [3, 2, 1]] {
                let mut policy = Policy::default();
                let original = inventory(&[3, 1, 2], current, &[1, 3]);
                assert_eq!(policy.observe(original.clone()), Decision::Idle);
                let moved = inventory(&displaced, current, &[1, 3]);
                assert_eq!(policy.observe(moved.clone()), Decision::Verify);
                assert_eq!(
                    policy.observe(moved),
                    Decision::Apply(Change::MoveToEnd(DesktopId(2)))
                );
                assert_eq!(policy.observe(original), Decision::Idle);
            }
        }
    }

    #[test]
    fn window_opened_during_move_confirmation_keeps_its_desktop_in_place() {
        let mut policy = Policy::default();
        assert_eq!(
            policy.observe(inventory(&[1, 2], 2, &[2])),
            Decision::Verify
        );
        let full = inventory(&[1, 2], 2, &[1, 2]);
        assert_eq!(policy.observe(full.clone()), Decision::Verify);
        assert_eq!(policy.observe(full), Decision::Apply(Change::Create));
    }

    #[test]
    fn the_spare_stays_last_while_the_vacated_current_waits_for_departure() {
        let mut policy = Policy::default();
        assert_eq!(
            policy.observe(inventory(&[1, 2, 3, 4], 2, &[1, 2, 3])),
            Decision::Idle
        );
        let closed_and_reordered = inventory(&[4, 1, 2, 3], 2, &[1, 3]);
        assert_eq!(
            policy.observe(closed_and_reordered.clone()),
            Decision::Verify
        );
        assert_eq!(
            policy.observe(closed_and_reordered),
            Decision::Apply(Change::MoveToEnd(DesktopId(4)))
        );
        assert_eq!(
            policy.observe(inventory(&[1, 2, 3, 4], 2, &[1, 3])),
            Decision::Idle
        );
        let left = inventory(&[1, 2, 3, 4], 3, &[1, 3]);
        assert_eq!(policy.observe(left.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(left),
            Decision::Apply(Change::Remove {
                desktop: DesktopId(2),
                fallback: DesktopId(4)
            })
        );
    }

    #[test]
    fn opening_a_desktop_in_task_view_while_already_empty_keeps_only_the_selected_one() {
        let mut policy = Policy::default();
        assert_eq!(policy.observe(inventory(&[1], 1, &[])), Decision::Idle);
        let selected = inventory(&[1, 2], 2, &[]);
        assert_eq!(policy.observe(selected.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(selected),
            Decision::Apply(Change::Remove {
                desktop: DesktopId(1),
                fallback: DesktopId(2),
            })
        );
        assert_eq!(policy.observe(inventory(&[2], 2, &[])), Decision::Idle);
    }

    #[test]
    fn externally_reordered_desktops_navigate_by_shell_order_without_wrapping() {
        let order = [DesktopId(7), DesktopId(9), DesktopId(3)];
        assert_eq!(neighbor(&order, DesktopId(7), true), Some(DesktopId(9)));
        assert_eq!(neighbor(&order, DesktopId(3), false), Some(DesktopId(9)));
        assert_eq!(neighbor(&order, DesktopId(3), true), None);
        assert_eq!(neighbor(&order, DesktopId(7), false), None);
        assert_eq!(neighbor(&order, DesktopId(404), true), None);
    }

    #[test]
    fn only_a_committed_window_move_can_switch_desktops() {
        let from = DesktopId(42);
        let to = DesktopId(17);
        let no_query = || panic!("no follow after a rejected move");
        let no_switch = |_| panic!("no switch after a rejected move");
        let no_move = || panic!("invalid or interrupted request was moved");
        assert!(
            WindowTransfer::perform(from, from, no_move, no_query, || true, no_switch).is_none()
        );
        assert!(
            WindowTransfer::perform(from, to, no_move, no_query, || false, no_switch).is_none()
        );
        assert!(
            WindowTransfer::perform(from, to, || false, no_query, || true, no_switch).is_none()
        );
    }

    #[test]
    fn follow_uses_the_moved_window_destination_and_preserves_partial_success() {
        let from = DesktopId(42);
        let to = DesktopId(17);
        let other = DesktopId(99);
        for (current, switch_accepted, expected_switch, expected_follow) in [
            (Some(from), true, true, true),
            (Some(from), false, true, false),
            (Some(to), false, false, true),
            (Some(other), true, false, false),
            (None, true, false, false),
        ] {
            let mut moves = 0;
            let mut switches = Vec::new();
            let result = WindowTransfer::perform(
                from,
                to,
                || {
                    moves += 1;
                    true
                },
                || current,
                || true,
                |target| {
                    switches.push(target);
                    switch_accepted
                },
            )
            .unwrap();
            assert_eq!(moves, 1);
            assert_eq!(switches, expected_switch.then_some(to).as_slice());
            assert_eq!(
                (result.from, result.to, result.followed),
                (from, to, expected_follow)
            );
        }
    }

    #[test]
    fn user_input_during_the_shell_move_cancels_follow_only() {
        use std::cell::Cell;
        let from = DesktopId(42);
        let to = DesktopId(17);
        for current in [from, to] {
            let interrupted = Cell::new(false);
            let moved = WindowTransfer::perform(
                from,
                to,
                || {
                    interrupted.set(true);
                    true
                },
                || Some(current),
                || !interrupted.get(),
                |_| panic!("interrupted move attempted a switch"),
            )
            .unwrap();
            assert!(!moved.followed);
        }
    }

    #[test]
    fn desktop_numbers_use_task_view_order_and_clamp_to_the_last_existing_slot() {
        let order = [9, 15, 2].map(DesktopId);
        let here = order[1];
        let no_scan = |_: &[DesktopId]| panic!("only forward movement needs an occupancy check");
        assert_eq!(numbered_target(&order, here, 1, no_scan), Some(order[0]));
        assert_eq!(numbered_target(&order, here, 2, no_scan), None);
        for number in [3, 5, 10] {
            assert_eq!(
                numbered_target(&order, here, number, |_| Some(true)),
                Some(order[2])
            );
        }
        assert_eq!(numbered_target(&order, order[2], 10, no_scan), None);
        assert_eq!(numbered_target(&[], here, 1, no_scan), None);
        assert_eq!(numbered_target(&order, DesktopId(99), 1, no_scan), None);
        assert_eq!(numbered_target(&order, here, 0, no_scan), None);

        let many: Vec<_> = (20..32).rev().map(DesktopId).collect();
        assert_eq!(
            numbered_target(&many, many[0], 10, |_| Some(true)),
            Some(many[9])
        );
    }

    #[test]
    fn numbered_forward_jump_respects_the_empty_tail_during_cleanup() {
        let order = [1, 2, 3, 4, 5, 6].map(DesktopId);
        let here = order[3];
        let target = |occupancy| {
            numbered_target(&order, here, 10, |tail| {
                assert_eq!(tail, &order[3..]);
                occupancy
            })
        };
        assert_eq!(target(Some(false)), None);
        assert_eq!(target(None), None);
        // A window here or farther right keeps the final empty desktop reachable.
        assert_eq!(target(Some(true)), Some(order[5]));
    }

    #[test]
    fn next_on_four_checks_live_windows_before_the_shell_removes_five() {
        let state = inventory(&[1, 2, 3, 4, 5], 4, &[1, 2, 3]);
        let tail = from_current(&state.order, state.current).unwrap();
        let forward = |occupied: &HashSet<DesktopId>| {
            forward_neighbor(tail, |ids| Some(ids.iter().any(|id| occupied.contains(id))))
        };
        assert_eq!(forward(&state.occupied), None);
        assert_eq!(forward_neighbor(tail, |_| None), None);
        assert_eq!(
            neighbor(&state.order, state.current, false),
            Some(DesktopId(3))
        );
        // A minimized window still occupies its desktop. Reopening an app on
        // either side of this boundary must also make Next work immediately.
        for reopened in [DesktopId(4), DesktopId(5)] {
            let mut occupied = state.occupied.clone();
            occupied.insert(reopened);
            assert_eq!(forward(&occupied), Some(DesktopId(5)));
        }
    }

    #[test]
    fn ambiguous_shell_failures_retry_only_after_checking_the_result() {
        let full = inventory(&[1], 1, &[1]);
        let mut policy = Policy::default();
        assert_eq!(policy.observe(full.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(full.clone()),
            Decision::Apply(Change::Create)
        );
        policy.failed();
        assert!(matches!(
            policy.observe(full.clone()),
            Decision::RetryAfter(_)
        ));
        policy.retry_at = Some(Instant::now());
        assert_eq!(policy.observe(full.clone()), Decision::Verify);
        assert_eq!(policy.observe(inventory(&[1, 2], 1, &[1])), Decision::Idle);
    }

    #[test]
    #[ignore = "read-only inspection of the interactive Explorer session"]
    fn live_spare_inventory() {
        let shell = DesktopShell::connect().expect("supported interactive desktop");
        let state = Inventory::read(&shell).expect("complete, consistent desktop inventory");
        let occupancy: Vec<_> = state
            .order
            .iter()
            .map(|id| (state.occupied.contains(id), *id == state.current))
            .collect();
        println!("Task View order (occupied, current): {occupancy:?}");
    }
}
