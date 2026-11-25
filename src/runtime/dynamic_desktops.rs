use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::native::desktops::{DesktopId, occupancy, shell::DesktopShell};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Inventory {
    order: Vec<DesktopId>,
    current: DesktopId,
    occupied: HashSet<DesktopId>,
}

impl Inventory {
    fn read(shell: &DesktopShell) -> Option<Self> {
        let current = shell.current()?;
        let order = shell.list()?;
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
    Remove {
        desktop: DesktopId,
        fallback: DesktopId,
    },
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
        if occupied || self.awaiting_departure != Some(current) {
            self.awaiting_departure = None;
        }
        if !occupied && self.last_current == Some((current, true)) {
            self.awaiting_departure = Some(current);
        }
        self.last_current = Some((current, occupied));
        self.spare = if !occupied && self.awaiting_departure.is_none() {
            // Task View can create and select a second empty desktop. Keep the
            // one the user selected and retire the old spare in the background.
            Some(current)
        } else {
            self.spare
                .filter(|id| inventory.order.contains(id) && empty(id))
                .or_else(|| inventory.order.iter().rev().find(|id| empty(id)).copied())
        };
        let change = match self.spare {
            None => Some(Change::Create),
            Some(spare) => inventory
                .order
                .iter()
                .copied()
                .find(|&id| id != inventory.current && id != spare && empty(&id))
                .map(|desktop| Change::Remove {
                    desktop,
                    fallback: spare,
                }),
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

pub(super) const SETTLE_MS: u32 = 350;

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
            return (self.incomplete <= 3).then(|| SETTLE_MS * u32::from(self.incomplete));
        };
        self.incomplete = 0;
        match self.policy.observe(inventory) {
            Decision::Idle => None,
            Decision::Verify => Some(SETTLE_MS),
            Decision::RetryAfter(delay) => Some(delay),
            Decision::Apply(change) => {
                let applied = match change {
                    Change::Create => {
                        self.policy.spare = shell.create();
                        self.policy.spare.is_some()
                    }
                    Change::Remove { desktop, fallback } => shell.remove(desktop, fallback),
                };
                if !applied {
                    self.policy.failed();
                }
                // Even after an RPC error, the Shell may have committed the change.
                // Observe the result before considering another mutation.
                Some(SETTLE_MS)
            }
        }
    }

    pub(super) fn navigate(&mut self, next: bool) -> bool {
        let Some(shell) = &self.shell else {
            return false;
        };
        let Some((current, order)) = shell.current().zip(shell.list()) else {
            return false;
        };
        if let Some(target) = neighbor(&order, current, next) {
            return shell.switch(target);
        }
        // A user can open the first window on the spare and immediately press Next,
        // before the normal two event-driven observations have created its successor.
        if next
            && order.last() == Some(&current)
            && let Some(inventory) = Inventory::read(shell)
            && inventory.order == order
            && inventory.current == current
            && order.iter().all(|id| inventory.occupied.contains(id))
        {
            let proposal = (inventory, Change::Create);
            if self.policy.issued.as_ref() != Some(&proposal) {
                self.policy.issued = Some(proposal);
                self.policy.pending = None;
                self.policy.retry_at = None;
                self.policy.failures = 0;
                self.policy.spare = shell.create();
                if let Some(spare) = self.policy.spare {
                    return shell.switch(spare);
                }
                self.policy.failed();
            }
        }
        false
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
    fn last_window_closed_waits_until_departure_and_keeps_the_existing_spare() {
        let mut policy = Policy::default();
        assert_eq!(
            policy.observe(inventory(&[1, 2, 3], 2, &[1, 2])),
            Decision::Idle
        );
        assert_eq!(
            policy.observe(inventory(&[1, 2, 3], 2, &[1])),
            Decision::Idle
        );
        for destination in [1, 3] {
            let left = inventory(&[1, 2, 3], destination, &[1]);
            assert_eq!(policy.observe(left.clone()), Decision::Verify);
            assert_eq!(
                policy.observe(left),
                Decision::Apply(Change::Remove {
                    desktop: DesktopId(2),
                    fallback: DesktopId(3),
                })
            );
        }
        assert_eq!(policy.observe(inventory(&[1, 3], 3, &[1])), Decision::Idle);
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
        // Desktop 2 became current. It can now replace 3 as the spare, but
        // the pending request to remove 2 must never be issued.
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
