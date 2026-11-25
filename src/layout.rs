mod bsp;
mod edge;
mod grid;

#[cfg(test)]
mod focused_insert_tests;
#[cfg(test)]
mod move_sequence_tests;

use bsp::{Bsp, Cut};
use grid::{Grid, Shape};

pub type Id = isize;
const GAP: i32 = 8;
const MIN_WIDTH: i32 = 160;
const MIN_HEIGHT: i32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn enclosing(self, other: Self) -> Self {
        let span = |axis: Axis| {
            let (a, b) = axis.bounds(self);
            let (c, d) = axis.bounds(other);
            (b.max(d) - a.min(c)) as i32
        };
        Self {
            x: self.x.min(other.x),
            y: self.y.min(other.y),
            w: span(Axis::X),
            h: span(Axis::Y),
        }
    }

    fn contains(self, rect: Self) -> bool {
        [Axis::X, Axis::Y].into_iter().all(|axis| {
            let (lo, hi) = axis.bounds(self);
            let (a, b) = axis.bounds(rect);
            lo <= a && b <= hi
        })
    }

    pub fn inset_for_tiles(self, gutter: i32) -> Self {
        let width = self.w.max(0);
        let height = self.h.max(0);
        let dx = gutter.clamp(0, (width - MIN_WIDTH).max(0) / 2);
        let dy = gutter.clamp(0, (height - MIN_HEIGHT).max(0) / 2);
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
            w: width - dx * 2,
            h: height - dy * 2,
        }
    }

    // Two tiles can share a single rectangular region exactly when their
    // touching edges have the same span and only the normal gutter between.
    fn joined(self, other: Self, axis: Axis) -> Option<Self> {
        let across = axis.perpendicular();
        if across.bounds(self) != across.bounds(other) {
            return None;
        }
        let (a, b) = axis.bounds(self);
        let (c, d) = axis.bounds(other);
        if a.max(c) - b.min(d) != i64::from(GAP) {
            return None;
        }
        let length = i32::try_from(b.max(d) - a.min(c)).ok()?;
        Some(match axis {
            Axis::X => Self {
                x: self.x.min(other.x),
                w: length,
                ..self
            },
            Axis::Y => Self {
                y: self.y.min(other.y),
                h: length,
                ..self
            },
        })
    }

    // At a T, the narrow tile enters at its aligned outer edge. Return the
    // direction of the remaining half of the host, where its filler must lie.
    fn entry_side(self, host: Self, movement: Direction) -> Option<Direction> {
        let across = movement.axis().perpendicular();
        if movement.distance(self, host) != i64::from(GAP)
            || across.length(self) >= across.length(host)
        {
            return None;
        }
        let (a, b) = across.bounds(self);
        let (c, d) = across.bounds(host);
        if a == c {
            Some(across.direction(false))
        } else if b == d {
            Some(across.direction(true))
        } else {
            None
        }
    }

    // For an L-shaped pair the shared outer edge fixes the new divider.
    // The narrower cell extends through both bands; the wider one's arm
    // remains confined to its original band.
    fn corner_parts(self, other: Self, direction: Direction) -> Option<(Rect, Rect)> {
        if direction.distance(self, other) != i64::from(GAP) {
            return None;
        }
        let axis = direction.axis();
        let across = axis.perpendicular();
        let (narrow, wide) = if across.length(self) < across.length(other) {
            (self, other)
        } else {
            (other, self)
        };
        let (a, b) = across.bounds(narrow);
        let (c, d) = across.bounds(wide);
        let strip_start = if a == c {
            b + i64::from(GAP)
        } else if b == d {
            c
        } else {
            return None;
        };
        let strip_length = across.length(wide) - across.length(narrow) - GAP;
        if strip_length <= 0 {
            return None;
        }
        let strip_start = i32::try_from(strip_start).ok()?;
        let full_start = axis.bounds(narrow).0.min(axis.bounds(wide).0) as i32;
        let full_length = axis
            .length(narrow)
            .checked_add(GAP)?
            .checked_add(axis.length(wide))?;
        Some(match axis {
            Axis::Y => (
                Rect {
                    y: full_start,
                    h: full_length,
                    ..narrow
                },
                Rect {
                    x: strip_start,
                    w: strip_length,
                    ..wide
                },
            ),
            Axis::X => (
                Rect {
                    x: full_start,
                    w: full_length,
                    ..narrow
                },
                Rect {
                    y: strip_start,
                    h: strip_length,
                    ..wide
                },
            ),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn towards_start(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
    fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::X,
            Self::Up | Self::Down => Axis::Y,
        }
    }
    fn distance(self, from: Rect, to: Rect) -> i64 {
        let (a, b) = self.axis().bounds(from);
        let (c, d) = self.axis().bounds(to);
        if self.towards_start() { a - d } else { c - b }
    }

    // Compare doubled centers so that odd sizes and negative monitor origins
    // need no rounding. A move must also vacate some of the original tile.
    fn advances(self, from: Rect, to: Rect) -> bool {
        let (a, b) = self.axis().bounds(from);
        let (c, d) = self.axis().bounds(to);
        let forward = if self.towards_start() {
            c + d < a + b
        } else {
            c + d > a + b
        };
        forward && !to.contains(from)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

impl Axis {
    fn perpendicular(self) -> Self {
        match self {
            Self::X => Self::Y,
            Self::Y => Self::X,
        }
    }
    fn length(self, rect: Rect) -> i32 {
        match self {
            Self::X => rect.w,
            Self::Y => rect.h,
        }
    }
    fn minimum(self) -> i32 {
        match self {
            Self::X => MIN_WIDTH,
            Self::Y => MIN_HEIGHT,
        }
    }
    fn min_dimension(self, minimum: (i32, i32)) -> i32 {
        match self {
            Self::X => minimum.0,
            Self::Y => minimum.1,
        }
    }
    fn divider_limits(self, region: Rect, first: Window, second: Window) -> Option<(i32, i32)> {
        let across = self.perpendicular();
        let min = self.min_dimension(first.min);
        let max = self.length(region) - GAP - self.min_dimension(second.min);
        (min <= max
            && across.length(region)
                >= across
                    .min_dimension(first.min)
                    .max(across.min_dimension(second.min)))
        .then_some((min, max))
    }
    fn split_at(self, rect: Rect, first: i32) -> (Rect, Rect) {
        let gap = GAP.min(self.length(rect));
        let second = self.length(rect) - gap - first;
        match self {
            Self::X => (
                Rect { w: first, ..rect },
                Rect {
                    x: rect.x.saturating_add(first + gap),
                    w: second,
                    ..rect
                },
            ),
            Self::Y => (
                Rect { h: first, ..rect },
                Rect {
                    y: rect.y.saturating_add(first + gap),
                    h: second,
                    ..rect
                },
            ),
        }
    }
    fn bounds(self, rect: Rect) -> (i64, i64) {
        let start = i64::from(match self {
            Self::X => rect.x,
            Self::Y => rect.y,
        });
        (start, start + i64::from(self.length(rect)))
    }
    fn direction(self, towards_start: bool) -> Direction {
        match (self, towards_start) {
            (Self::X, true) => Direction::Left,
            (Self::X, false) => Direction::Right,
            (Self::Y, true) => Direction::Up,
            (Self::Y, false) => Direction::Down,
        }
    }
}

#[derive(Clone, Debug)]
struct SharedPair {
    source: usize,
    target: usize,
    direction: Direction,
    partition: Partition,
}

#[derive(Clone, Copy, Debug)]
enum Partition {
    Divider {
        axis: Axis,
        source_first: bool,
        part: i32,
        whole: i32,
    },
    Corner,
    Enter {
        filler: usize,
    },
}

impl SharedPair {
    // Check both the displayed layout and its independent second presses.
    // In particular, a T junction can become a pinwheel with no full gutter.
    fn can_freeze(pairs: &[Self], area: Rect, cells: &[Rect], windows: &[Window]) -> bool {
        let mut displayed = cells.to_vec();
        for pair in pairs {
            if !pair.apply(&mut displayed, windows) {
                return false;
            }
        }
        if Bsp::from_rects(area, &displayed).is_none() {
            return false;
        }
        if pairs.len() < 2 {
            return true;
        }
        let mut regions: Vec<_> = cells.iter().map(|&r| Some((r, false))).collect();
        for pair in pairs {
            let mut bounds = cells[pair.source];
            for slot in pair.slots() {
                bounds = bounds.enclosing(cells[slot]);
                regions[slot] = None;
            }
            regions[pair.source] = Some((bounds, true));
        }
        Bsp::separates_pairs(&mut regions.into_iter().flatten().collect::<Vec<_>>())
    }

    fn new(
        source: usize,
        target: usize,
        direction: Direction,
        cells: &[Rect],
        windows: &[Window],
    ) -> Option<Self> {
        let a = cells[source];
        let b = cells[target];
        let Some(joined) = a.joined(b, direction.axis()) else {
            let (through, arm) = a.corner_parts(b, direction)?;
            // The arm lies wholly in one original slot. Only the through
            // tile can be an intermediate step between the two centers.
            return (direction.advances(a, through)
                && direction.advances(through, b)
                && windows[source].fits(through)
                && windows[target].fits(arm))
            .then_some(Self {
                source,
                target,
                direction,
                partition: Partition::Corner,
            });
        };
        // Turn the seam so each window enters its neighbor's former area.
        // Sliding the existing seam would leave the active window in place.
        let axis = direction.axis().perpendicular();
        let usable = axis.length(joined) - GAP;
        let (min, max) = axis.divider_limits(joined, windows[source], windows[target])?;
        let source_first = axis == Axis::X || direction.towards_start();
        let source_length = (usable / 2 + usable % 2).clamp(min, max);
        let part = if source_first {
            source_length
        } else {
            usable - source_length
        };
        Some(Self {
            source,
            target,
            direction,
            partition: Partition::Divider {
                axis,
                source_first,
                part,
                whole: usable,
            },
        })
    }

    fn rectangles(&self, cells: &[Rect], windows: &[Window]) -> Option<(Rect, Rect)> {
        let (axis, source_first, part, whole) = match self.partition {
            Partition::Divider {
                axis,
                source_first,
                part,
                whole,
            } => (axis, source_first, part, whole),
            Partition::Corner => {
                let (through, arm) =
                    cells[self.source].corner_parts(cells[self.target], self.direction)?;
                return (windows[self.source].fits(through) && windows[self.target].fits(arm))
                    .then_some((through, arm));
            }
            Partition::Enter { .. } => {
                let (source, host) = (cells[self.source], cells[self.target]);
                let side = source.entry_side(host, self.direction)?;
                let across = side.axis();
                let (min, max) =
                    across.divider_limits(host, windows[self.source], windows[self.target])?;
                let width = across.length(source).clamp(min, max);
                if side.towards_start() {
                    let (host_part, entering) =
                        across.split_at(host, across.length(host) - GAP - width);
                    return Some((entering, host_part));
                }
                return Some(across.split_at(host, width));
            }
        };
        let region = cells[self.source].joined(cells[self.target], self.direction.axis())?;
        let total = axis.length(region) - GAP;
        let (first, second) = if source_first {
            (self.source, self.target)
        } else {
            (self.target, self.source)
        };
        let (min, max) = axis.divider_limits(region, windows[first], windows[second])?;
        let first =
            ((i64::from(part) * i64::from(total) + i64::from(whole) / 2) / i64::from(whole)) as i32;
        let (leading, trailing) = axis.split_at(region, first.clamp(min, max));
        Some(if source_first {
            (leading, trailing)
        } else {
            (trailing, leading)
        })
    }

    fn filler(&self) -> Option<usize> {
        match self.partition {
            Partition::Enter { filler } => Some(filler),
            _ => None,
        }
    }

    fn slots(&self) -> impl Iterator<Item = usize> {
        [self.source, self.target].into_iter().chain(self.filler())
    }

    fn remap(&mut self, slot: impl Fn(usize) -> usize) {
        self.source = slot(self.source);
        self.target = slot(self.target);
        if let Partition::Enter { filler } = &mut self.partition {
            *filler = slot(*filler);
        }
    }

    fn apply(&self, cells: &mut [Rect], windows: &[Window]) -> bool {
        let Some((source, target)) = self.rectangles(cells, windows) else {
            return false;
        };
        if let Some(filler) = self.filler() {
            let old = cells[self.source];
            let Some(side) = old.entry_side(cells[self.target], self.direction) else {
                return false;
            };
            let Some(expanded) = old.joined(cells[filler], side.axis()) else {
                return false;
            };
            if side.distance(old, cells[filler]) != i64::from(GAP)
                || !windows[filler].fits(expanded)
            {
                return false;
            }
            cells[filler] = expanded;
        }
        cells[self.source] = source;
        cells[self.target] = target;
        true
    }

    fn other(&self, slot: usize) -> Option<usize> {
        if self.source == slot {
            Some(self.target)
        } else if self.target == slot {
            Some(self.source)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Window {
    id: Id,
    min: (i32, i32),
}

impl Window {
    fn fits(self, rect: Rect) -> bool {
        rect.w >= self.min.0 && rect.h >= self.min.1
    }
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    grid: Grid,
    bsp: Bsp,
    occupants: Vec<Window>,
    shared: Vec<SharedPair>,
    last_area: Option<Rect>,
}

impl Layout {
    pub fn cell_rects(&self, area: Rect) -> Vec<Rect> {
        self.bsp.rects(self.grid.rects(area))
    }

    pub fn placements(&self, area: Rect) -> Vec<(Id, Rect)> {
        let mut visible = self.cell_rects(area);
        for pair in &self.shared {
            // Pending moves have disjoint participants.
            pair.apply(&mut visible, &self.occupants);
        }
        self.occupants.iter().map(|w| w.id).zip(visible).collect()
    }

    fn slot(&self, id: Id) -> Option<usize> {
        self.occupants.iter().position(|w| w.id == id)
    }
    pub fn contains(&self, id: Id) -> bool {
        self.slot(id).is_some()
    }

    fn swap_fits(&self, source: usize, target: usize, cells: &[Rect]) -> bool {
        self.occupants[source].fits(cells[target]) && self.occupants[target].fits(cells[source])
    }

    // An unrelated operation can invalidate a preview. Complete the swap
    // where the two slots already fit, otherwise return to the original cells.
    fn dismiss_pair(&mut self, index: usize, cells: &[Rect]) {
        let pair = self.shared.swap_remove(index);
        if self.swap_fits(pair.source, pair.target, cells) {
            self.occupants.swap(pair.source, pair.target);
        }
    }

    fn minimums(&self, area: Rect) -> Vec<(i32, i32)> {
        let mut minima: Vec<_> = self.occupants.iter().map(|w| w.min).collect();
        if !self.shared.is_empty() {
            let cells = self.cell_rects(area);
            for pair in &self.shared {
                // Preserve an already feasible takeover across unrelated edits.
                // A preview may also fit before its destination has grown.
                if self.swap_fits(pair.source, pair.target, &cells) {
                    let (a, b) = (minima[pair.source], minima[pair.target]);
                    let both = (a.0.max(b.0), a.1.max(b.1));
                    minima[pair.source] = both;
                    minima[pair.target] = both;
                }
            }
        }
        minima
    }

    fn settle_invalid_pairs(&mut self, area: Rect) {
        if self.shared.is_empty() {
            return;
        }
        let cells = self.cell_rects(area);
        if !SharedPair::can_freeze(&self.shared, area, &cells, &self.occupants) {
            for pair in std::mem::take(&mut self.shared) {
                self.shared.push(pair);
                if !SharedPair::can_freeze(&self.shared, area, &cells, &self.occupants) {
                    self.dismiss_pair(self.shared.len() - 1, &cells);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn insert(&mut self, id: Id, area: Rect) -> bool {
        self.insert_with_minimum(id, area, (MIN_WIDTH, MIN_HEIGHT))
    }

    fn grid_for(&self, windows: &[Window], area: Rect) -> Option<Grid> {
        let minima: Vec<_> = windows.iter().map(|w| w.min).collect();
        Shape::candidates(windows.len(), area)
            .into_iter()
            .find_map(|shape| {
                let mut grid = self.grid.clone();
                grid.set_shape(shape);
                grid.fit(area, &minima).then_some(grid)
            })
    }

    pub fn insert_with_minimum(&mut self, id: Id, area: Rect, minimum: (i32, i32)) -> bool {
        if self.contains(id) {
            return false;
        }
        let mut windows = self.occupants.clone();
        for pair in &self.shared {
            windows.swap(pair.source, pair.target)
        }
        windows.push(Window {
            id,
            min: (minimum.0.max(MIN_WIDTH), minimum.1.max(MIN_HEIGHT)),
        });
        let Some(grid) = self.grid_for(&windows, area) else {
            return false;
        };
        self.occupants = windows;
        self.grid = grid;
        self.bsp.0.clear();
        self.shared.clear();
        self.last_area = Some(area);
        true
    }

    fn freeze_pair(&mut self, index: usize, area: Rect) -> bool {
        let pair = &self.shared[index];
        let mut cells = self.cell_rects(area);
        if !pair.apply(&mut cells, &self.occupants) {
            self.dismiss_pair(index, &cells);
            return false;
        }
        let mut remaining = self.shared.clone();
        remaining.swap_remove(index);
        if !self.rebuild_cells(area, &cells, remaining) {
            // A non-slicing arrangement can have no straight cut across the
            // region. Finish only the active pair before splitting its slot.
            let original = self.cell_rects(area);
            self.dismiss_pair(index, &original);
            return false;
        }
        true
    }

    fn rebuild_cells(&mut self, area: Rect, cells: &[Rect], shared: Vec<SharedPair>) -> bool {
        let Some((bsp, order)) = Bsp::from_rects(area, cells) else {
            return false;
        };
        self.shared = shared;
        let mut slots = vec![0; order.len()];
        self.occupants = order
            .iter()
            .enumerate()
            .map(|(new, &old)| {
                slots[old] = new;
                self.occupants[old]
            })
            .collect();
        for pair in &mut self.shared {
            pair.remap(|old| slots[old]);
        }
        self.bsp = bsp;
        self.grid.set_shape(Shape::candidates(1, area)[0]);
        true
    }

    pub fn insert_near(&mut self, id: Id, anchor: Id, area: Rect, minimum: (i32, i32)) -> bool {
        let Some(anchor_slot) = self.slot(anchor) else {
            return false;
        };
        if self.contains(id) {
            return false;
        }
        let mut base = self.clone();
        if let Some(i) = base
            .shared
            .iter()
            .position(|p| p.slots().any(|slot| slot == anchor_slot))
        {
            base.freeze_pair(i, area);
        }
        let at = base.slot(anchor).expect("anchor is still tiled");
        let cells = base.cell_rects(area);
        let roots = cells.len() - base.bsp.0.len();
        let rect = cells[at];
        let axis = if let Some(i) = base.bsp.parent_cut(at, roots) {
            base.bsp.0[i].axis.perpendicular()
        } else if roots == 2 {
            let roots = base.grid.rects(area);
            if roots[0].x == roots[1].x {
                Axis::X
            } else {
                Axis::Y
            }
        } else if rect.w >= rect.h {
            Axis::X
        } else {
            Axis::Y
        };
        let new = Window {
            id,
            min: (minimum.0.max(MIN_WIDTH), minimum.1.max(MIN_HEIGHT)),
        };
        let old = base.occupants[at];
        base.occupants.push(new);
        for allow_growth in [false, true] {
            for axis in [axis, axis.perpendicular()] {
                let mut candidate = base.clone();
                let across = axis.perpendicular();
                let along_min = i64::from(axis.min_dimension(old.min))
                    + i64::from(GAP)
                    + i64::from(axis.min_dimension(new.min));
                if !allow_growth
                    && (i64::from(axis.length(rect)) < along_min
                        || across.length(rect)
                            < across
                                .min_dimension(old.min)
                                .max(across.min_dimension(new.min)))
                {
                    continue;
                }
                candidate.bsp.0.push(Cut::half(at, axis));
                let (root_minima, children) = candidate.bsp.minimums(candidate.minimums(area));
                if allow_growth && !candidate.grid.fit(area, &root_minima) {
                    continue;
                }
                let root_rects = candidate.grid.rects(area);
                if !root_rects
                    .iter()
                    .zip(&root_minima)
                    .all(|(r, &(w, h))| r.w >= w && r.h >= h)
                {
                    continue;
                }
                candidate.bsp.fit(root_rects, &children);
                candidate.settle_invalid_pairs(area);
                candidate.last_area = Some(area);
                *self = candidate;
                return true;
            }
        }
        false
    }

    pub fn remove(&mut self, id: Id) {
        let Some(mut slot) = self.slot(id) else {
            return;
        };
        if let Some(index) = self
            .shared
            .iter()
            .position(|pair| pair.filler().is_some() && pair.slots().any(|s| s == slot))
        {
            self.freeze_pair(
                index,
                self.last_area.expect("admitted windows have a work area"),
            );
            slot = self.slot(id).expect("closing window remains tiled");
        }
        if !self.bsp.0.is_empty() {
            self.remove_local(slot);
            return;
        }
        let area = self.last_area.expect("admitted windows have a work area");
        let cells = self.cell_rects(area);
        for i in (0..self.shared.len()).rev() {
            self.dismiss_pair(i, &cells);
        }
        let slot = self.slot(id).expect("managed occupant");
        self.occupants.remove(slot);
        if let Some(grid) = self.grid_for(&self.occupants, area) {
            self.grid = grid
        } else {
            // Even when no balanced repartition fits a mix of large native
            // minimums, dropping the old slot cannot reduce remaining space.
            self.grid.remove_slot(slot);
            let minima: Vec<_> = self.occupants.iter().map(|w| w.min).collect();
            self.grid.fit(area, &minima);
        }
    }

    fn remove_local(&mut self, slot: usize) {
        let roots = self.occupants.len() - self.bsp.0.len();
        self.shared.retain(|p| !p.slots().any(|s| s == slot));
        let (removed, survivor) = if let Some((child, parent)) = self.bsp.remove_leaf(slot, roots) {
            // The remaining subtree takes its parent's entire rectangle. Its
            // earlier sibling index survives, even if the first window closed.
            if slot == parent {
                self.occupants.swap(parent, child);
            }
            (child, parent)
        } else {
            self.grid.remove_slot(slot);
            for cut in &mut self.bsp.0 {
                cut.at -= usize::from(cut.at > slot);
            }
            (slot, slot)
        };
        self.occupants.remove(removed);
        let remap = |index: usize| {
            if index == removed {
                survivor
            } else {
                index - usize::from(index > removed)
            }
        };
        for pair in &mut self.shared {
            pair.remap(remap);
        }
        let area = self.last_area.expect("admitted windows have a work area");
        let (minimums, children) = self.bsp.minimums(self.minimums(area));
        let fits = self.grid.fit(area, &minimums);
        debug_assert!(fits, "removing a leaf only releases space");
        self.bsp.fit(self.grid.rects(area), &children);
        self.settle_invalid_pairs(area);
    }

    pub fn neighbor(&self, id: Id, direction: Direction, area: Rect) -> Option<Id> {
        let tiles = self.placements(area);
        let slot = self.slot(id)?;
        let index = nearest(
            tiles[slot].1,
            direction,
            tiles.iter().map(|(_, rect)| *rect),
        )?;
        Some(self.occupants[index].id)
    }

    pub fn move_window(&mut self, id: Id, direction: Direction, area: Rect) -> bool {
        let Some(source) = self.slot(id) else {
            return false;
        };
        let visible = self.placements(area);
        let from = visible[source].1;
        let mut next = self.clone();
        let completed_entry = if let Some(index) = next
            .shared
            .iter()
            .position(|pair| pair.source == source && pair.direction == direction)
        {
            let pair = next.shared.swap_remove(index);
            let cells = next.cell_rects(area);
            if !next.exchange(source, pair.target, area, &cells) {
                return false;
            }
            pair.filler().map(|_| cells[pair.target])
        } else {
            let neighbor = nearest(from, direction, visible.iter().map(|tile| tile.1));
            // A narrower window can enter the facing portion of its host if
            // an adjacent tile in its old row can reclaim the vacancy.
            let filler = neighbor.and_then(|target| {
                let side = from.entry_side(visible[target].1, direction)?;
                visible.iter().position(|&(_, r)| {
                    side.distance(from, r) == i64::from(GAP)
                        && from.joined(r, side.axis()).is_some()
                })
            });
            let entered = filler.is_some()
                && next.move_from_visible(source, neighbor, filler, direction, area, &visible);
            if !entered {
                if filler.is_some() {
                    // Failed previews may already have materialized the filler's
                    // pending move. Retry without disturbing that bystander.
                    next = self.clone();
                }
                if !next.move_from_visible(source, neighbor, None, direction, area, &visible) {
                    return false;
                }
            }
            None
        };
        let to = next.placements(area)[next.slot(id).expect("moving window remains tiled")].1;
        // A window already inside the target row/column can finish by taking
        // the rest of that very slot without changing its center along the arrow.
        let took_host = completed_entry.is_some_and(|host| {
            host.contains(from)
                && to != from
                && to.contains(from)
                && direction.axis().bounds(to) == direction.axis().bounds(from)
        });
        if !took_host && !direction.advances(from, to) {
            return false;
        }
        *self = next;
        true
    }

    fn move_from_visible(
        &mut self,
        source: usize,
        target: Option<usize>,
        filler: Option<usize>,
        direction: Direction,
        area: Rect,
        visible: &[(Id, Rect)],
    ) -> bool {
        // A different command starts from the displayed tiles. Bake only the
        // previews it touches; other pairs keep their pending second press.
        let mut cells = self.cell_rects(area);
        let mut remaining = self.shared.clone();
        remaining.retain(|pair| {
            let involved = pair
                .slots()
                .any(|s| s == source || Some(s) == target || Some(s) == filler);
            if involved {
                for slot in pair.slots() {
                    cells[slot] = visible[slot].1;
                }
            }
            !involved
        });
        let (source, target, filler) = if remaining.len() != self.shared.len() {
            if !self.rebuild_cells(area, &cells, remaining) {
                return false;
            }
            cells = self.cell_rects(area);
            let remap = |slot: usize| self.slot(visible[slot].0).expect("visible tile remains");
            (remap(source), target.map(remap), filler.map(remap))
        } else {
            (source, target, filler)
        };
        let Some(target) = target else {
            return self.move_to_edge(source, direction, area);
        };
        let pair = match filler {
            Some(filler) => Some(SharedPair {
                source,
                target,
                direction,
                partition: Partition::Enter { filler },
            }),
            None => SharedPair::new(source, target, direction, &cells, &self.occupants),
        };
        if let Some(pair) = pair {
            self.shared.push(pair);
            if SharedPair::can_freeze(&self.shared, area, &cells, &self.occupants) {
                return true;
            }
            self.shared.pop();
        }
        filler.is_none() && self.exchange(source, target, area, &cells)
    }

    fn exchange(&mut self, source: usize, target: usize, area: Rect, cells: &[Rect]) -> bool {
        if self.swap_fits(source, target, cells) {
            self.occupants.swap(source, target);
            return true;
        }
        // Keep the same grid and BSP branches. Move their existing dividers
        // only as required by the windows' minimums in their new slots.
        let mut minima = self.minimums(area);
        minima.swap(source, target);
        let (roots, children) = self.bsp.minimums(minima);
        let mut grid = self.grid.clone();
        if !grid.refit(area, &roots) {
            return false;
        }
        self.bsp.fit(grid.rects(area), &children);
        self.grid = grid;
        self.occupants.swap(source, target);
        self.settle_invalid_pairs(area);
        true
    }

    pub fn resize(
        &mut self,
        id: Id,
        direction: Direction,
        shrink: bool,
        area: Rect,
    ) -> Option<Direction> {
        let slot = self.slot(id)?;
        let cells = self.cell_rects(area);
        let mut companion = None;
        if let Some(index) = self
            .shared
            .iter()
            .position(|p| p.slots().any(|s| s == slot))
        {
            if self.shared[index].filler().is_some() {
                // All three windows can start a resize from their visible frame.
                let mut displayed = self.clone();
                if !displayed.freeze_pair(index, area) {
                    return None;
                }
                let edge = displayed.resize(id, direction, shrink, area)?;
                *self = displayed;
                return Some(edge);
            }
            let pair = &mut self.shared[index];
            companion = pair.other(slot);
            let (source, target) = pair.rectangles(&cells, &self.occupants)?;
            let previous = pair.partition;
            let Partition::Divider {
                axis,
                source_first,
                part,
                whole,
            } = &mut pair.partition
            else {
                // An L's corner is fixed by the surrounding cells. Resize
                // from the settled pair, keeping a failed resize a no-op.
                let mut settled = self.clone();
                let pair = settled.shared.swap_remove(index);
                settled.exchange(pair.source, pair.target, area, &cells);
                let edge = settled.resize(id, direction, shrink, area)?;
                *self = settled;
                return Some(edge);
            };
            let first = if *source_first { source } else { target };
            let edge = axis.direction((slot == pair.source) != *source_first);
            if direction.axis() == *axis && (!shrink || direction == edge) {
                let total = axis.length(source) + axis.length(target);
                let (a, b) = (
                    axis.min_dimension(self.occupants[pair.source].min),
                    axis.min_dimension(self.occupants[pair.target].min),
                );
                let minimums = if *source_first { (a, b) } else { (b, a) };
                *part = shifted_divider(axis.length(first), total, minimums, direction, shrink)?;
                *whole = total;
                if !SharedPair::can_freeze(&self.shared, area, &cells, &self.occupants) {
                    self.shared[index].partition = previous;
                    return None;
                }
                return Some(edge);
            }
        }
        let (root_minima, children) = self.bsp.minimums(self.minimums(area));
        let roots = self.grid.rects(area);
        let count = roots.len();
        let edge = if let Some(edge) = self
            .bsp
            .resize(slot, direction, shrink, roots, companion, &children)
        {
            edge
        } else {
            self.grid.resize(
                self.bsp.root(slot, count),
                direction,
                shrink,
                area,
                companion.map(|s| self.bsp.root(s, count)),
                &root_minima,
            )?
        };
        self.bsp.fit(self.grid.rects(area), &children);
        self.settle_invalid_pairs(area);
        Some(edge)
    }
}

fn shifted_divider(
    current: i32,
    total: i32,
    minimums: (i32, i32),
    direction: Direction,
    shrink: bool,
) -> Option<i32> {
    let (min, max) = (minimums.0, total - minimums.1);
    if min > max {
        return None;
    }
    let step = (total / 20).max(1);
    let offset = current - (total / 2 + total % 2);
    let next = if direction.towards_start() == shrink {
        current.saturating_add(step - offset.rem_euclid(step))
    } else {
        current.saturating_sub(step - (-offset).rem_euclid(step))
    }
    .clamp(min, max);
    (next != current).then_some(next)
}

fn nearest(origin: Rect, direction: Direction, rects: impl Iterator<Item = Rect>) -> Option<usize> {
    let across = direction.axis().perpendicular();
    let (a, b) = across.bounds(origin);
    rects
        .enumerate()
        .filter_map(|(slot, rect)| {
            let distance = direction.distance(origin, rect);
            let (c, d) = across.bounds(rect);
            let overlap = b.min(d) - a.max(c);
            (distance >= 0 && overlap > 0).then_some((
                distance,
                -overlap,
                (a + b - c - d).abs(),
                c,
                slot,
            ))
        })
        .min()
        .map(|(_, _, _, _, slot)| slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_the_wide_tile_into_a_corner_shares_then_takes_the_exact_slot() {
        let area = Rect {
            x: -599,
            y: -16,
            w: 1444,
            h: 901,
        };
        let mut layout = Layout::default();
        for id in 1..=5 {
            assert!(layout.insert(id, area));
        }
        let before = layout.placements(area);
        let (upper, lower) = (before[0].1, before[3].1);
        assert!(upper.w < lower.w && upper.x == lower.x);
        assert!(layout.move_window(4, Direction::Up, area));
        let shared = layout.placements(area);
        assert_eq!(
            shared[3].1,
            Rect {
                h: upper.h + GAP + lower.h,
                ..upper
            }
        );
        assert_eq!(
            shared[0].1,
            Rect {
                x: upper.x + upper.w + GAP,
                w: lower.w - upper.w - GAP,
                ..lower
            }
        );
        for i in [1, 2, 4] {
            assert_eq!(shared[i], before[i]);
        }
        assert!(layout.move_window(4, Direction::Up, area));
        let mut exchanged = before;
        exchanged[0].0 = 4;
        exchanged[3].0 = 1;
        assert_eq!(layout.placements(area), exchanged);
    }

    #[test]
    fn gutter_scales_without_sacrificing_small_windows() {
        let screen = Rect {
            x: -1920,
            y: 40,
            w: 1921,
            h: 1040,
        };
        assert_eq!(
            screen.inset_for_tiles(12),
            Rect {
                x: -1908,
                y: 52,
                w: 1897,
                h: 1016
            }
        );
        let small = Rect {
            w: MIN_WIDTH + 5,
            h: MIN_HEIGHT + 2,
            ..screen
        };
        assert_eq!(
            small.inset_for_tiles(12),
            Rect {
                x: -1918,
                y: 41,
                w: MIN_WIDTH + 1,
                h: MIN_HEIGHT
            }
        );
    }

    #[test]
    fn resize_preserves_unrelated_tracks_and_reverses_at_minimum() {
        let area = Rect {
            x: -889,
            y: 18,
            w: 1309,
            h: 582,
        };
        let mut layout = Layout::default();
        for id in 1..=6 {
            assert!(layout.insert(id, area));
        }
        let original = layout.cell_rects(area);
        let mut before_minimum = original.clone();
        loop {
            let before = layout.cell_rects(area);
            if layout.resize(1, Direction::Right, false, area).is_none() {
                break;
            }
            before_minimum = before;
        }
        let final_cells = layout.cell_rects(area);
        assert_eq!(final_cells[1].w, MIN_WIDTH);
        assert_eq!(&final_cells[2..], &original[2..]);
        assert_eq!(
            layout.resize(1, Direction::Left, false, area),
            Some(Direction::Right)
        );
        assert_eq!(layout.cell_rects(area), before_minimum);
    }
}
