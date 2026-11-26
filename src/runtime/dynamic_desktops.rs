use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::DesktopDestination;
use crate::layout::Id;
use crate::native::desktops::{DesktopId, occupancy, shell::DesktopShell};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Inventory {
    order: Vec<DesktopId>,
    current: DesktopId,
    occupied: HashSet<DesktopId>,
}

impl Inventory {
    fn next_change(&self) -> Option<Change> {
        let empty = |id: &DesktopId| !self.occupied.contains(id);
        let current_occupied = self.occupied.contains(&self.current);
        let candidates = if current_occupied {
            self.order.as_slice()
        } else {
            // Removing or moving an earlier empty desktop would also change
            // the selected empty desktop's number. Only its successors can move.
            let (_, successors) = from_current(&self.order, self.current)?.split_first()?;
            if successors.iter().all(empty) {
                return successors.last().map(|&desktop| Change::Remove {
                    desktop,
                    fallback: self.current,
                });
            }
            successors
        };
        let Some(&spare) = candidates.iter().rfind(|id| empty(id)) else {
            return current_occupied.then_some(Change::Create);
        };
        if self.order.last() != Some(&spare) {
            return Some(Change::MoveToEnd(spare));
        }
        candidates
            .iter()
            .copied()
            .find(|&id| id != spare && empty(&id))
            .map(|desktop| Change::Remove {
                desktop,
                fallback: spare,
            })
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
    /// Returns the spare's ID after the change succeeds.
    fn apply(self, shell: &DesktopShell) -> Option<DesktopId> {
        match self {
            Self::Create => shell.create(),
            Self::MoveToEnd(id) => shell.move_to_end(id).then_some(id),
            Self::Remove { desktop, fallback } => {
                shell.remove(desktop, fallback).then_some(fallback)
            }
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
    pending: Option<(Inventory, Change)>,
    issued: Option<IssuedChange>,
    attempts: u8,
}

struct IssuedChange {
    proposal: (Inventory, Change),
    retry_at: Instant,
}

impl Policy {
    fn issue(&mut self, proposal: (Inventory, Change)) {
        // An HRESULT alone cannot tell us whether the order in Explorer has
        // actually changed. Back off until a fresh inventory confirms it.
        self.attempts = self.attempts.saturating_add(1);
        let seconds = match self.attempts {
            1 => 1,
            2 => 3,
            _ => 15,
        };
        self.issued = Some(IssuedChange {
            proposal,
            retry_at: Instant::now() + Duration::from_secs(seconds),
        });
        self.pending = None;
    }

    fn observe(&mut self, inventory: Inventory) -> Decision {
        let Some(change) = inventory.next_change() else {
            self.pending = None;
            self.issued = None;
            self.attempts = 0;
            return Decision::Idle;
        };
        let proposal = (inventory, change);
        if let Some(issued) = &self.issued
            && issued.proposal == proposal
        {
            let remaining = issued.retry_at.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                return Decision::RetryAfter(remaining.as_millis().max(1) as u32);
            }
            self.issued = None;
            self.pending = Some(proposal);
            return Decision::Verify;
        }
        if self.issued.is_some() || self.pending.as_ref().is_some_and(|old| old != &proposal) {
            self.attempts = 0;
        }
        self.issued = None;
        if self.pending.as_ref() != Some(&proposal) {
            self.pending = Some(proposal);
            return Decision::Verify;
        }
        self.issue(proposal);
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

fn previous_transfer_target(
    mut order: Vec<DesktopId>,
    from: DesktopId,
    create: impl FnOnce() -> Option<DesktopId>,
    place_first: impl FnOnce(DesktopId, &[DesktopId]) -> bool,
) -> Option<(DesktopId, Vec<DesktopId>)> {
    if order.first() != Some(&from) {
        let previous = neighbor(&order, from, false)?;
        return Some((previous, order));
    }
    let target = create()?;
    if order.contains(&target) {
        return None;
    }
    order.insert(0, target);
    place_first(target, &order).then_some((target, order))
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

fn stationary_index(
    order: &[DesktopId],
    current: DesktopId,
    destination: DesktopDestination,
    transferring_window: bool,
    occupied: impl FnOnce(&[DesktopId]) -> Option<bool>,
) -> Option<usize> {
    let index = order.iter().position(|&id| id == current)?;
    let empty_tail = || occupied(&order[index..]).map(|value| !value);
    let stationary = match destination {
        DesktopDestination::Relative { next: false } => index == 0 && !transferring_window,
        DesktopDestination::Relative { next: true } => empty_tail()?,
        DesktopDestination::Number(number) => {
            let target = number.checked_sub(1)?.min(order.len() - 1);
            target == index || (target > index && empty_tail()?)
        }
    };
    stationary.then_some(index + 1)
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

    pub(super) fn snapshot(&self) -> Option<(DesktopId, Vec<DesktopId>)> {
        let shell = self.shell.as_ref()?;
        let current = shell.current()?;
        let order = shell.list()?;
        (order.contains(&current) && shell.current()? == current).then_some((current, order))
    }

    pub(super) fn stationary_index(
        &self,
        destination: DesktopDestination,
        transferring_window: bool,
    ) -> Option<usize> {
        let (current, order) = self.snapshot()?;
        stationary_index(&order, current, destination, transferring_window, |tail| {
            occupancy::any_occupied(&order, tail)
        })
    }

    pub(super) fn activity(&mut self) -> bool {
        self.incomplete = 0;
        self.supported && (self.available() || Instant::now() >= self.reconnect_at)
    }

    fn retry_incomplete(&mut self) -> u32 {
        self.policy.pending = None;
        self.incomplete = self.incomplete.saturating_add(1);
        (RETRY_MS * u32::from(self.incomplete)).min(5000)
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
            let disconnected = shell.current().is_none();
            let delay = self.retry_incomplete();
            if disconnected {
                self.shell = None;
                self.reconnect_at = Instant::now() + Duration::from_secs(5);
                return Some(5000);
            }
            return Some(delay);
        };
        self.incomplete = 0;
        match self.policy.observe(inventory) {
            Decision::Idle => None,
            Decision::Verify => Some(CONFIRM_MS),
            Decision::RetryAfter(delay) => Some(delay),
            Decision::Apply(change) => {
                change.apply(shell);
                // Even after an RPC error, the Shell may have committed the change.
                // Observe the result before considering another mutation.
                Some(CONFIRM_MS)
            }
        }
    }

    fn resolve(
        &mut self,
        current: DesktopId,
        destination: DesktopDestination,
    ) -> Option<(DesktopId, Vec<DesktopId>)> {
        let shell = self.shell.as_ref()?;
        let order = shell.list()?;
        let target = match destination {
            DesktopDestination::Number(number) => {
                numbered_target(&order, current, number, |tail| {
                    occupancy::any_occupied(&order, tail)
                })?
            }
            DesktopDestination::Relative { next: false } => neighbor(&order, current, false)?,
            DesktopDestination::Relative { next: true } => {
                let tail = from_current(&order, current)?;
                if tail.len() == 1 {
                    return self.prepare_next(current, order);
                }
                forward_neighbor(tail, |ids| occupancy::any_occupied(&order, ids))?
            }
        };
        Some((target, order))
    }

    fn prepare_next(
        &mut self,
        current: DesktopId,
        order: Vec<DesktopId>,
    ) -> Option<(DesktopId, Vec<DesktopId>)> {
        let shell = self.shell.as_ref()?;
        let inventory = Inventory::for_order(shell, current, order)?;
        if !inventory.occupied.contains(&current) {
            return None;
        }
        let change = inventory.next_change()?;
        let proposal = (inventory, change);
        if self
            .policy
            .issued
            .as_ref()
            .is_some_and(|issued| issued.proposal == proposal)
        {
            return None;
        }
        self.policy.attempts = 0;
        self.policy.issue(proposal);
        let to = change.apply(shell)?;
        let order = shell.list()?;
        (neighbor(&order, current, true) == Some(to)).then_some((to, order))
    }

    pub(super) fn navigate(
        &mut self,
        destination: DesktopDestination,
    ) -> Option<(DesktopId, DesktopId)> {
        let current = self.current_desktop()?;
        let (target, order) = self.resolve(current, destination)?;
        let shell = self.shell.as_ref()?;
        (shell.current()? == current && shell.list()? == order && shell.switch(target))
            .then_some((current, target))
    }

    pub(super) fn transfer(
        &mut self,
        window: Id,
        destination: DesktopDestination,
        valid: impl Fn(DesktopId) -> bool,
        uninterrupted: impl Fn() -> bool,
    ) -> Option<WindowTransfer> {
        let from = self.current_desktop()?;
        let still_valid = || uninterrupted() && valid(from);
        if !still_valid() {
            return None;
        }
        let (to, order) = if destination == (DesktopDestination::Relative { next: false }) {
            let shell = self.shell.as_ref()?;
            previous_transfer_target(
                shell.list()?,
                from,
                || {
                    if shell.can_move_window(window, from) && still_valid() {
                        shell.create()
                    } else {
                        None
                    }
                },
                |created, expected| {
                    if !still_valid() {
                        return false;
                    }
                    // Explorer may commit a reorder even if its RPC reports an error.
                    // Transfer only after observing the exact original order behind us.
                    shell.move_before_first(created, from);
                    shell.list().as_deref() == Some(expected)
                },
            )?
        } else {
            self.resolve(from, destination)?
        };
        let shell = self.shell.as_ref()?;
        WindowTransfer::perform(
            from,
            to,
            || {
                shell.list().as_ref() == Some(&order)
                    && shell.move_window(window, from, to, still_valid)
            },
            || shell.current(),
            &uninterrupted,
            |target| shell.switch(target),
        )
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
        assert!(matches!(policy.observe(full), Decision::RetryAfter(_)));
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
    fn a_successor_gaining_a_window_cancels_tail_cleanup_and_preserves_the_current_desktop() {
        let mut policy = Policy::default();
        let order = [1, 2, 3, 4, 5, 6];
        let used = inventory(&order, 4, &[1, 2, 3, 4, 5]);
        assert_eq!(policy.observe(used), Decision::Idle);
        let tail = inventory(&order, 4, &[1, 2, 3]);
        assert_eq!(policy.observe(tail), Decision::Verify);
        let fifth_reopened = inventory(&order, 4, &[1, 2, 3, 5]);
        assert_eq!(policy.observe(fifth_reopened.clone()), Decision::Idle);
        assert_eq!(policy.observe(fifth_reopened), Decision::Idle);
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
    fn displaced_spare_moves_to_the_end_only_after_the_user_leaves_it() {
        for displaced in [[2, 3, 1], [3, 2, 1]] {
            let mut policy = Policy::default();
            let away = inventory(&displaced, 1, &[1, 3]);
            let on_spare = inventory(&displaced, 2, &[1, 3]);
            // Selecting the proposed move target before confirmation cancels it.
            assert_eq!(policy.observe(away.clone()), Decision::Verify);
            assert_eq!(policy.observe(on_spare.clone()), Decision::Idle);
            assert_eq!(policy.observe(on_spare), Decision::Idle);
            assert_eq!(policy.observe(away.clone()), Decision::Verify);
            assert_eq!(
                policy.observe(away),
                Decision::Apply(Change::MoveToEnd(DesktopId(2)))
            );
            assert_eq!(
                policy.observe(inventory(&[3, 1, 2], 1, &[1, 3])),
                Decision::Idle
            );
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
    fn closing_and_reopening_the_last_window_keeps_the_selected_desktop_in_place() {
        let mut policy = Policy::default();
        let order = [1, 2, 3, 4];
        let open = inventory(&order, 2, &[1, 2, 3]);
        let closed = inventory(&order, 2, &[1, 3]);
        assert_eq!(policy.observe(open.clone()), Decision::Idle);
        assert_eq!(policy.observe(closed.clone()), Decision::Idle);
        assert_eq!(policy.observe(closed.clone()), Decision::Idle);
        assert_eq!(policy.observe(open), Decision::Idle);
        assert_eq!(
            policy.observe(inventory(&order, 3, &[1, 2, 3])),
            Decision::Idle
        );

        assert_eq!(policy.observe(closed), Decision::Idle);
        let departed = inventory(&order, 3, &[1, 3]);
        assert_eq!(policy.observe(departed.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(departed),
            Decision::Apply(Change::Remove {
                desktop: DesktopId(2),
                fallback: DesktopId(4)
            })
        );
        assert_eq!(
            policy.observe(inventory(&[1, 3, 4], 3, &[1, 3])),
            Decision::Idle
        );
    }

    #[test]
    fn removing_an_earlier_empty_waits_while_it_would_renumber_the_selected_empty_desktop() {
        let mut policy = Policy::default();
        assert_eq!(policy.observe(inventory(&[1], 1, &[])), Decision::Idle);
        let selected = inventory(&[1, 2], 2, &[]);
        assert_eq!(policy.observe(selected.clone()), Decision::Idle);
        assert_eq!(policy.observe(selected), Decision::Idle);
        let departed = inventory(&[1, 2], 1, &[]);
        assert_eq!(policy.observe(departed.clone()), Decision::Verify);
        assert_eq!(
            policy.observe(departed),
            Decision::Apply(Change::Remove {
                desktop: DesktopId(2),
                fallback: DesktopId(1),
            })
        );
    }

    #[test]
    fn all_four_desktop_occupancies_respect_the_selected_slot_and_settle_after_departure() {
        fn settle(policy: &mut Policy, state: &mut Inventory) {
            let index = |s: &Inventory| s.order.iter().position(|&id| id == s.current).unwrap();
            let selected_slot = index(state);
            for _ in 0..=state.order.len() {
                match policy.observe(state.clone()) {
                    Decision::Idle => return,
                    Decision::Verify => {}
                    other => panic!("unexpected initial decision: {other:?}"),
                }
                let Decision::Apply(change) = policy.observe(state.clone()) else {
                    panic!("unchanged inventory should confirm the proposed change");
                };
                match change {
                    Change::Create => state.order.push(DesktopId(100)),
                    Change::MoveToEnd(id) => {
                        assert_ne!(id, state.current);
                        assert!(!state.occupied.contains(&id));
                        state.order.retain(|&desktop| desktop != id);
                        state.order.push(id);
                    }
                    Change::Remove { desktop, fallback } => {
                        assert_ne!(desktop, state.current);
                        assert!(!state.occupied.contains(&desktop));
                        assert!(state.order.contains(&fallback));
                        state.order.retain(|&id| id != desktop);
                    }
                }
                if !state.occupied.contains(&state.current) {
                    assert_eq!(index(state), selected_slot);
                }
            }
            panic!("spare policy did not settle");
        }

        let order = [41, 8, 74, 3].map(DesktopId);
        for mask in 0u8..16 {
            let working: Vec<_> = order
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, &desktop)| desktop)
                .collect();
            for current in order {
                let mut state = Inventory {
                    order: order.to_vec(),
                    current,
                    occupied: working.iter().copied().collect(),
                };
                let mut policy = Policy::default();
                settle(&mut policy, &mut state);
                if !state.occupied.contains(&current) {
                    state.current = working.first().copied().unwrap_or(state.order[0]);
                    settle(&mut policy, &mut state);
                }
                let (spare, settled) = state.order.split_last().unwrap();
                assert!(!state.occupied.contains(spare));
                assert_eq!(settled, working);
                assert_eq!(policy.observe(state), Decision::Idle);
            }
        }
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
    fn transferring_before_the_first_desktop_inserts_one_slot_and_preserves_the_spare() {
        let (first, second, spare, new) =
            (DesktopId(31), DesktopId(8), DesktopId(42), DesktopId(17));
        for order in [vec![first], vec![first, second, spare]] {
            let expected: Vec<_> = std::iter::once(new).chain(order.iter().copied()).collect();
            let target = previous_transfer_target(
                order,
                first,
                || Some(new),
                |created, proposed| created == new && proposed == expected,
            );
            assert_eq!(target, Some((new, expected)));
        }
        let order = vec![first, second, spare];
        let unexpected_create = || panic!("created a desktop without reaching the first one");
        let unexpected_move = |_, _: &[DesktopId]| panic!("reordered an existing desktop");
        assert_eq!(
            previous_transfer_target(order.clone(), second, unexpected_create, unexpected_move),
            Some((first, order.clone()))
        );
        assert_eq!(
            previous_transfer_target(order, new, unexpected_create, unexpected_move),
            None
        );
    }

    #[test]
    fn a_failed_create_or_unconfirmed_prepend_cannot_supply_a_transfer_destination() {
        let from = DesktopId(7);
        let new = DesktopId(9);
        for returned in [None, Some(from)] {
            assert!(
                previous_transfer_target(
                    vec![from],
                    from,
                    || returned,
                    |_, _| { panic!("only a newly created desktop may be repositioned") }
                )
                .is_none()
            );
        }
        assert!(previous_transfer_target(vec![from], from, || Some(new), |_, _| false).is_none());
    }

    #[test]
    fn cli_recognizes_only_proven_noop_desktop_moves_and_transfers() {
        use crate::cli::Destination;

        let order = [0x33, 0x55, 0x11].map(DesktopId);
        let by_position = |from, destination: Destination, transferring_window| {
            stationary_index(
                &order,
                from,
                destination.into(),
                transferring_window,
                |_| panic!("position is sufficient"),
            )
        };
        let stationary = |from, destination: Destination, occupancy| {
            stationary_index(&order, from, destination.into(), true, |tail| {
                assert_eq!(tail.first(), Some(&from));
                occupancy
            })
        };
        assert_eq!(
            by_position(order[2], Destination::Number(50), true),
            Some(3)
        );
        assert_eq!(by_position(order[0], Destination::Previous, false), Some(1));
        assert_eq!(by_position(order[0], Destination::Previous, true), None);
        assert_eq!(by_position(order[0], Destination::Number(1), true), Some(1));
        assert_eq!(by_position(order[0], Destination::Number(0), true), None);
        assert_eq!(by_position(DesktopId(999), Destination::Next, true), None);
        for destination in [Destination::Next, Destination::Number(99)] {
            assert_eq!(stationary(order[1], destination, Some(false)), Some(2));
            assert_eq!(stationary(order[1], destination, Some(true)), None);
            assert_eq!(stationary(order[1], destination, None), None);
        }
        // Next can create a spare on an occupied last desktop. An uncertain
        // inventory cannot be reported as a successful no-op, either.
        assert_eq!(stationary(order[2], Destination::Next, Some(true)), None);
        assert_eq!(stationary(order[2], Destination::Next, None), None);
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
    #[ignore = "read-only numbered transfer preflight; requires a movable foreground and two desktops"]
    fn numbered_foreground_transfer_preflight() {
        use crate::native::{
            desktops::{DesktopMembership, Desktops},
            foreground,
        };

        let mut workspaces = Workspaces::new();
        let current = workspaces.current_desktop().expect("interactive Explorer");
        let order = workspaces.shell.as_ref().unwrap().list().unwrap();
        assert!(order.len() > 1, "a second desktop is required");
        let number = if order[4.min(order.len() - 1)] == current {
            1
        } else {
            5
        };
        let destination = DesktopDestination::Number(number);
        let (target, snapshot) = workspaces.resolve(current, destination).unwrap();
        assert_eq!(snapshot, order);
        assert_eq!(target, order[(number - 1).min(order.len() - 1)]);
        let hwnd = foreground();
        let desktops = Desktops::new();
        assert_eq!(
            desktops.membership(hwnd),
            DesktopMembership::Current(current)
        );

        let shell = workspaces.shell.as_ref().unwrap();
        let mut reached = false;
        assert!(!shell.move_window(hwnd, current, target, || {
            reached = true;
            false
        }));
        assert!(reached, "foreground must reach the final veto");
        assert_eq!(workspaces.current_desktop(), Some(current));
        assert_eq!(
            desktops.membership(hwnd),
            DesktopMembership::Current(current)
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
    fn an_unobserved_shell_change_retries_until_the_inventory_confirms_it() {
        let complete = inventory(&[1, 2], 1, &[1]);
        for (stale, change) in [
            (inventory(&[1], 1, &[1]), Change::Create),
            (inventory(&[2, 1], 1, &[1]), Change::MoveToEnd(DesktopId(2))),
        ] {
            let mut policy = Policy::default();
            for seconds in [1, 3, 15, 15] {
                assert_eq!(policy.observe(stale.clone()), Decision::Verify);
                let before = Instant::now();
                assert_eq!(policy.observe(stale.clone()), Decision::Apply(change));
                assert!(matches!(
                    policy.observe(stale.clone()),
                    Decision::RetryAfter(_)
                ));
                let issued = policy.issued.as_mut().unwrap();
                assert!(issued.retry_at >= before + Duration::from_secs(seconds));
                issued.retry_at = Instant::now();
            }
            assert_eq!(policy.observe(stale), Decision::Verify);
            assert_eq!(policy.observe(complete.clone()), Decision::Idle);
        }
    }

    #[test]
    fn incomplete_inventories_retry_at_a_bounded_rate_and_break_confirmation() {
        let state = inventory(&[2, 1], 1, &[1]);
        let mut policy = Policy::default();
        assert_eq!(policy.observe(state.clone()), Decision::Verify);
        let mut workspaces = Workspaces {
            supported: true,
            shell: None,
            reconnect_at: Instant::now(),
            policy,
            incomplete: 0,
        };
        for n in 1..=32 {
            assert_eq!(workspaces.retry_incomplete(), (RETRY_MS * n).min(5000));
        }
        assert_eq!(workspaces.policy.observe(state.clone()), Decision::Verify);
        assert_eq!(
            workspaces.policy.observe(state),
            Decision::Apply(Change::MoveToEnd(DesktopId(2)))
        );
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
