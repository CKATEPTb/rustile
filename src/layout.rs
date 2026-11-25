mod bsp;
mod drop_target;
mod edge;
mod grid;
mod vacancy;

#[cfg(test)]
mod drop_tests;
#[cfg(test)]
mod focused_insert_tests;
#[cfg(test)]
mod mouse_resize_tests;
#[cfg(test)]
mod move_sequence_tests;
#[cfg(test)]
mod work_area_tests;

use bsp::{Bsp, Cut};
pub use drop_target::DropZone;
use grid::{Grid, Shape};
use vacancy::Vacancy;

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
    fn joined(self, other: Self, axis: Axis, gap: i32) -> Option<Self> {
        let across = axis.perpendicular();
        if across.bounds(self) != across.bounds(other) {
            return None;
        }
        let (a, b) = axis.bounds(self);
        let (c, d) = axis.bounds(other);
        if a.max(c) - b.min(d) != i64::from(gap) {
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

    // Split the destination across the direction of travel. Matching outer
    // edges preserve an existing divider; offset tiles enter the closer half.
    fn enter_host(
        self,
        host: Self,
        movement: Direction,
        moving: Window,
        resident: Window,
        gap: i32,
    ) -> Option<(Self, Self)> {
        let across = movement.axis().perpendicular();
        let (a, b) = across.bounds(self);
        let (c, d) = across.bounds(host);
        if movement.distance(self, host) != i64::from(gap)
            || a.max(c) >= b.min(d)
            || (a, b) == (c, d)
        {
            return None;
        }
        let (min, max) = across.divider_limits(host, moving, resident, gap)?;
        let usable = across.length(host) - gap;
        let length = if (a == c || b == d) && across.length(self) < across.length(host) {
            across.length(self)
        } else {
            usable / 2 + usable % 2
        }
        .clamp(min, max);
        Some(if a + b <= c + d {
            across.split_at(host, length, gap)
        } else {
            let (resident, moving) = across.split_at(host, usable - length, gap);
            (moving, resident)
        })
    }

    // For an L-shaped pair the shared outer edge fixes the new divider.
    // The narrower cell extends through both bands; the wider one's arm
    // remains confined to its original band.
    fn corner_parts(self, other: Self, direction: Direction, gap: i32) -> Option<(Rect, Rect)> {
        if direction.distance(self, other) != i64::from(gap) {
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
            b + i64::from(gap)
        } else if b == d {
            c
        } else {
            return None;
        };
        let strip_length = across.length(wide) - across.length(narrow) - gap;
        if strip_length <= 0 {
            return None;
        }
        let strip_start = i32::try_from(strip_start).ok()?;
        let full_start = axis.bounds(narrow).0.min(axis.bounds(wide).0) as i32;
        let full_length = axis
            .length(narrow)
            .checked_add(gap)?
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
    fn coordinate(self, rect: Rect) -> i64 {
        let (first, last) = self.axis().bounds(rect);
        if self.towards_start() { first } else { last }
    }

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
    fn divider_limits(
        self,
        region: Rect,
        first: Window,
        second: Window,
        gap: i32,
    ) -> Option<(i32, i32)> {
        let across = self.perpendicular();
        let min = self.min_dimension(first.min);
        let max = self.length(region) - gap - self.min_dimension(second.min);
        (min <= max
            && across.length(region)
                >= across
                    .min_dimension(first.min)
                    .max(across.min_dimension(second.min)))
        .then_some((min, max))
    }
    fn split_at(self, rect: Rect, first: i32, gap: i32) -> (Rect, Rect) {
        let gap = gap.min(self.length(rect));
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

#[derive(Clone, Debug)]
enum Partition {
    Divider {
        axis: Axis,
        source_first: bool,
        part: i32,
        whole: i32,
    },
    Corner,
    Enter(Vacancy),
}

impl SharedPair {
    // Check both the displayed layout and its independent second presses.
    // In particular, a T junction can become a pinwheel with no full gutter.
    fn can_freeze(
        pairs: &[Self],
        area: Rect,
        cells: &[Rect],
        windows: &[Window],
        gap: i32,
    ) -> bool {
        let mut displayed = cells.to_vec();
        for pair in pairs {
            if !pair.apply(&mut displayed, windows, gap) {
                return false;
            }
        }
        if Bsp::from_rects(area, &displayed, gap).is_none() {
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
        Bsp::separates_pairs(&mut regions.into_iter().flatten().collect::<Vec<_>>(), gap)
    }

    fn new(
        source: usize,
        target: usize,
        direction: Direction,
        cells: &[Rect],
        windows: &[Window],
        gap: i32,
    ) -> Option<Self> {
        let a = cells[source];
        let b = cells[target];
        let Some(joined) = a.joined(b, direction.axis(), gap) else {
            let (through, arm) = a.corner_parts(b, direction, gap)?;
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
        let usable = axis.length(joined) - gap;
        let (min, max) = axis.divider_limits(joined, windows[source], windows[target], gap)?;
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

    fn rectangles(&self, cells: &[Rect], windows: &[Window], gap: i32) -> Option<(Rect, Rect)> {
        let (axis, source_first, part, whole) = match self.partition {
            Partition::Divider {
                axis,
                source_first,
                part,
                whole,
            } => (axis, source_first, part, whole),
            Partition::Corner => {
                let (through, arm) =
                    cells[self.source].corner_parts(cells[self.target], self.direction, gap)?;
                return (windows[self.source].fits(through) && windows[self.target].fits(arm))
                    .then_some((through, arm));
            }
            Partition::Enter(_) => {
                return cells[self.source].enter_host(
                    cells[self.target],
                    self.direction,
                    windows[self.source],
                    windows[self.target],
                    gap,
                );
            }
        };
        let region = cells[self.source].joined(cells[self.target], self.direction.axis(), gap)?;
        let total = axis.length(region) - gap;
        let (first, second) = if source_first {
            (self.source, self.target)
        } else {
            (self.target, self.source)
        };
        let (min, max) = axis.divider_limits(region, windows[first], windows[second], gap)?;
        let first =
            ((i64::from(part) * i64::from(total) + i64::from(whole) / 2) / i64::from(whole)) as i32;
        let (leading, trailing) = axis.split_at(region, first.clamp(min, max), gap);
        Some(if source_first {
            (leading, trailing)
        } else {
            (trailing, leading)
        })
    }

    fn vacancy(&self) -> Option<&Vacancy> {
        match &self.partition {
            Partition::Enter(vacancy) => Some(vacancy),
            _ => None,
        }
    }

    fn slots(&self) -> impl Iterator<Item = usize> {
        [self.source, self.target]
            .into_iter()
            .chain(self.vacancy().into_iter().flat_map(Vacancy::slots))
    }

    fn remap(&mut self, slot: impl Fn(usize) -> usize) {
        self.source = slot(self.source);
        self.target = slot(self.target);
        if let Partition::Enter(vacancy) = &mut self.partition {
            vacancy.remap(slot);
        }
    }

    fn apply(&self, cells: &mut [Rect], windows: &[Window], gap: i32) -> bool {
        let Some((source, target)) = self.rectangles(cells, windows, gap) else {
            return false;
        };
        if let Some(vacancy) = self.vacancy()
            && !vacancy.apply(self.source, cells, gap)
        {
            return false;
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
    pub fn with_gap(gap: i32) -> Self {
        Self {
            grid: Grid::new(gap),
            ..Self::default()
        }
    }

    /// Keep the existing tracks, BSP cuts and pending moves whenever their
    /// occupants fit the new spacing. Nothing is applied on failure.
    pub fn with_spacing(mut self, area: Rect, gap: i32) -> Option<Self> {
        self.fit_spacing(area, gap).then_some(self)
    }

    fn fit_spacing(&mut self, area: Rect, gap: i32) -> bool {
        self.grid.gap = gap;
        let minima = self.occupants.iter().map(|window| window.min).collect();
        let (roots, children) = self.bsp.minimums(minima, gap);
        if !self.grid.fit_spacing(area, &roots) {
            return false;
        }
        self.bsp.fit(self.grid.rects(area), &children, gap);
        self.last_area = Some(area);
        self.settle_invalid_pairs(area);
        true
    }

    /// An external work-area change cannot be rejected like a preference.
    /// Return the windows that must float so their native minima are respected.
    pub fn refit_work_area(
        &mut self,
        area: Rect,
        gap: i32,
        mut minimum: impl FnMut(Id) -> Option<(i32, i32)>,
    ) -> Vec<Id> {
        let mut updated = self.clone();
        for window in &mut updated.occupants {
            if let Some((w, h)) = minimum(window.id) {
                window.min = (w.max(MIN_WIDTH), h.max(MIN_HEIGHT));
            }
        }
        if updated.fit_spacing(area, gap) {
            *self = updated;
            return Vec::new();
        }

        // A different topology is necessary. Use the old visible reading
        // order, including any pending move, rather than BSP storage order.
        let old = self.last_area.expect("nonempty layouts have a work area");
        let mut ordered: Vec<_> = self
            .visible_rects(old)
            .into_iter()
            .zip(updated.occupants)
            .collect();
        ordered.sort_by_key(|(rect, _)| (rect.y, rect.x));
        let windows: Vec<_> = ordered.into_iter().map(|(_, window)| window).collect();
        let mut replacement = Self::with_gap(gap);
        if let Some(grid) = replacement.grid_for(&windows, area) {
            replacement.occupants = windows;
            replacement.grid = grid;
            replacement.last_area = Some(area);
            *self = replacement;
            return Vec::new();
        }

        // Including each tile's right/bottom gutter gives disjoint
        // footprints, also for BSP layouts with staggered rows.
        let footprint =
            |w: i32, h: i32| (i64::from(w) + i64::from(gap)) * (i64::from(h) + i64::from(gap));
        let mut free = footprint(area.w, area.h);
        let mut released = Vec::new();
        let mut failed_minima: Vec<(i32, i32)> = Vec::new();
        for window in windows {
            let (w, h) = window.min;
            let required = footprint(w, h);
            // While the accepted group is unchanged, a larger window cannot
            // fit a split that already failed for a smaller one.
            if !window.fits(area)
                || required > free
                || failed_minima.iter().any(|&(x, y)| x <= w && y <= h)
            {
                released.push(window.id);
                continue;
            }
            // Large and small windows sometimes fit in an asymmetric BSP
            // even though no balanced grid fits them together.
            if replacement.insert_with_minimum(window.id, area, window.min)
                || replacement
                    .placements(area)
                    .into_iter()
                    .any(|(anchor, _)| replacement.insert_near(window.id, anchor, area, window.min))
            {
                free -= required;
                failed_minima.clear();
            } else {
                failed_minima.retain(|&(x, y)| x < w || y < h);
                failed_minima.push(window.min);
                released.push(window.id);
            }
        }
        *self = replacement;
        released
    }

    pub fn cell_rects(&self, area: Rect) -> Vec<Rect> {
        self.bsp.rects(self.grid.rects(area), self.grid.gap)
    }

    fn visible_rects(&self, area: Rect) -> Vec<Rect> {
        let mut visible = self.cell_rects(area);
        for pair in &self.shared {
            // Pending moves have disjoint participants.
            pair.apply(&mut visible, &self.occupants, self.grid.gap);
        }
        visible
    }

    pub fn placements(&self, area: Rect) -> Vec<(Id, Rect)> {
        self.occupants
            .iter()
            .map(|w| w.id)
            .zip(self.visible_rects(area))
            .collect()
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
        if !SharedPair::can_freeze(&self.shared, area, &cells, &self.occupants, self.grid.gap) {
            for pair in std::mem::take(&mut self.shared) {
                self.shared.push(pair);
                if !SharedPair::can_freeze(
                    &self.shared,
                    area,
                    &cells,
                    &self.occupants,
                    self.grid.gap,
                ) {
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
        Shape::candidates(windows.len(), area, self.grid.gap)
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
        if !pair.apply(&mut cells, &self.occupants, self.grid.gap) {
            self.dismiss_pair(index, &cells);
            return false;
        }
        let mut remaining = self.shared.clone();
        remaining.swap_remove(index);
        if self.rebuild_cells(area, &cells, remaining).is_none() {
            // A non-slicing arrangement can have no straight cut across the
            // region. Finish only the active pair before splitting its slot.
            let original = self.cell_rects(area);
            self.dismiss_pair(index, &original);
            return false;
        }
        true
    }

    fn rebuild_cells(
        &mut self,
        area: Rect,
        cells: &[Rect],
        shared: Vec<SharedPair>,
    ) -> Option<Vec<usize>> {
        let (bsp, order) = Bsp::from_rects(area, cells, self.grid.gap)?;
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
        self.grid
            .set_shape(Shape::candidates(1, area, self.grid.gap)[0]);
        Some(slots)
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
                    + i64::from(self.grid.gap)
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
                let (root_minima, children) = candidate
                    .bsp
                    .minimums(candidate.minimums(area), self.grid.gap);
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
                candidate.bsp.fit(root_rects, &children, self.grid.gap);
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
            .position(|pair| pair.vacancy().is_some() && pair.slots().any(|s| s == slot))
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
        let (minimums, children) = self.bsp.minimums(self.minimums(area), self.grid.gap);
        let fits = self.grid.fit(area, &minimums);
        debug_assert!(fits, "removing a leaf only releases space");
        self.bsp
            .fit(self.grid.rects(area), &children, self.grid.gap);
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
        let visible = self.visible_rects(area);
        let from = visible[source];
        let (next, completed_entry) = if let Some(index) = self
            .shared
            .iter()
            .position(|pair| pair.source == source && pair.direction == direction)
        {
            let mut next = self.clone();
            let pair = next.shared.swap_remove(index);
            let cells = next.cell_rects(area);
            if !next.exchange(source, pair.target, area, &cells) {
                return false;
            }
            (next, pair.vacancy().map(|_| cells[pair.target]))
        } else {
            let Some(next) = self.begin_move(source, direction, area, &visible) else {
                return false;
            };
            (next, None)
        };
        let to = next.visible_rects(area)[next.slot(id).expect("moving window remains tiled")];
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

    fn begin_move(
        &self,
        source: usize,
        direction: Direction,
        area: Rect,
        visible: &[Rect],
    ) -> Option<Self> {
        let from = visible[source];
        let target = nearest(from, direction, visible.iter().copied());
        let gap = self.grid.gap;
        let attempt = |pair| {
            let mut next = self.clone();
            next.move_from_visible(source, target, pair, direction, area, visible)
                .then_some(next)
        };
        if let Some(to) = target {
            let host = visible[to];
            let across = direction.axis().perpendicular();
            let mut local = SharedPair::new(source, to, direction, visible, &self.occupants, gap);
            // A wider source can turn an L locally. A narrower source should
            // enter the host first so it visibly leaves its original band.
            if across.length(from) >= across.length(host)
                && let Some(pair) = local.take()
                && let Some(next) = attempt(Some(pair))
            {
                return Some(next);
            }
            if from
                .enter_host(
                    host,
                    direction,
                    self.occupants[source],
                    self.occupants[to],
                    gap,
                )
                .is_some()
            {
                for vacancy in Vacancy::candidates(source, to, direction, visible, gap) {
                    let entry = SharedPair {
                        source,
                        target: to,
                        direction,
                        partition: Partition::Enter(vacancy),
                    };
                    if let Some(next) = attempt(Some(entry)) {
                        return Some(next);
                    }
                }
            }
            if let Some(pair) = local
                && let Some(next) = attempt(Some(pair))
            {
                return Some(next);
            }
        }
        attempt(None)
    }

    fn move_from_visible(
        &mut self,
        source: usize,
        target: Option<usize>,
        mut preview: Option<SharedPair>,
        direction: Direction,
        area: Rect,
        visible: &[Rect],
    ) -> bool {
        // A different command starts from the displayed tiles. Bake only the
        // previews it touches; other pairs keep their pending second press.
        let mut cells = self.cell_rects(area);
        let (mut source, mut target) = (source, target);
        if !self.shared.is_empty() {
            let mut touched = vec![false; cells.len()];
            touched[source] = true;
            if let Some(to) = target {
                touched[to] = true;
            }
            if let Some(pair) = &preview {
                for i in pair.slots() {
                    touched[i] = true;
                }
            }
            let mut remaining = self.shared.clone();
            remaining.retain(|pair| {
                if pair.slots().any(|s| touched[s]) {
                    for slot in pair.slots() {
                        cells[slot] = visible[slot];
                    }
                    return false;
                }
                true
            });
            if remaining.len() != self.shared.len() {
                let Some(slots) = self.rebuild_cells(area, &cells, remaining) else {
                    return false;
                };
                if let Some(pair) = &mut preview {
                    pair.remap(|s| slots[s]);
                }
                source = slots[source];
                target = target.map(|s| slots[s]);
                cells = self.cell_rects(area);
            }
        }
        let Some(target) = target else {
            return self.move_to_edge(source, direction, area);
        };
        if let Some(pair) = preview {
            self.shared.push(pair);
            return SharedPair::can_freeze(
                &self.shared,
                area,
                &cells,
                &self.occupants,
                self.grid.gap,
            );
        }
        self.exchange(source, target, area, &cells)
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
        let (roots, children) = self.bsp.minimums(minima, self.grid.gap);
        let mut grid = self.grid.clone();
        if !grid.refit(area, &roots) {
            return false;
        }
        self.bsp.fit(grid.rects(area), &children, self.grid.gap);
        self.grid = grid;
        self.occupants.swap(source, target);
        self.settle_invalid_pairs(area);
        true
    }

    /// Move the specified visible frame edge in physical screen coordinates.
    /// Work on a proposal so a clamped drag cannot accidentally commit a move preview.
    pub fn resize_edge_to(&mut self, id: Id, edge: Direction, coordinate: i32, area: Rect) -> bool {
        let Some(slot) = self.slot(id) else {
            return false;
        };
        let current = edge.coordinate(self.visible_rects(area)[slot]);
        if current == edge.coordinate(area) || current == i64::from(coordinate) {
            return false;
        }
        let mut proposal = self.clone();
        if !proposal.drag_divider(id, edge, coordinate, area) {
            return false;
        }
        *self = proposal;
        true
    }

    fn drag_divider(&mut self, id: Id, edge: Direction, coordinate: i32, area: Rect) -> bool {
        let axis = edge.axis();
        let across = axis.perpendicular();
        let gap = self.grid.gap;
        loop {
            let slot = self.slot(id).expect("the resized window remains tiled");
            let cells = self.cell_rects(area);
            if let Some(i) = self
                .shared
                .iter()
                .position(|p| p.slots().any(|s| s == slot))
            {
                if let Some(changed) =
                    self.drag_shared_divider(i, slot, edge, coordinate, area, &cells)
                {
                    return changed;
                }
                // An external edge of a rotated pair, an L-shaped pair, or a
                // reclaimed vacancy belongs to the currently displayed tiling.
                if !self.freeze_pair(i, area) {
                    return false;
                }
                continue;
            }
            let current = edge.coordinate(cells[slot]);
            let roots = self.grid.rects(area);
            let root = self.bsp.root(slot, roots.len());
            let scope = self.bsp.divider_scope(slot, edge, &roots, gap).or_else(|| {
                (edge.coordinate(roots[root]) == current)
                    .then(|| self.grid.divider_scope(root, edge, area, &roots))
                    .flatten()
            });
            let Some(scope) = scope else { return false };
            let (start, end) = across.bounds(scope);
            let divider = current
                - if edge.towards_start() {
                    i64::from(gap)
                } else {
                    0
                };
            let side = |rect: Rect| {
                let (a, b) = across.bounds(rect);
                if a < start || b > end {
                    return None;
                }
                let (lo, hi) = axis.bounds(rect);
                if hi == divider {
                    Some(false)
                } else if lo == divider + i64::from(gap) {
                    Some(true)
                } else {
                    None
                }
            };
            if let Some(i) = self
                .shared
                .iter()
                .position(|p| p.slots().any(|s| side(cells[s]).is_some()))
            {
                if !self.freeze_pair(i, area) {
                    return false;
                }
                continue;
            }

            // Only rectangles facing the selected seam give or receive space.
            // This keeps every other edge fixed, including interior BSP cuts.
            let (mut min, mut max) = (i64::MIN, i64::MAX);
            for (&cell, window) in cells.iter().zip(&self.occupants) {
                let spare = i64::from(axis.length(cell) - axis.min_dimension(window.min));
                match side(cell) {
                    Some(true) => max = max.min(spare),
                    Some(false) => min = min.max(-spare),
                    None => {}
                }
            }
            if min == i64::MIN || max == i64::MAX || min > max {
                return false;
            }
            let delta = (i64::from(coordinate) - current).clamp(min, max) as i32;
            if delta == 0 {
                return false;
            }
            let mut resized = cells;
            for cell in &mut resized {
                match (axis, side(*cell)) {
                    (Axis::X, Some(false)) => cell.w += delta,
                    (Axis::Y, Some(false)) => cell.h += delta,
                    (Axis::X, Some(true)) => {
                        cell.x = cell.x.saturating_add(delta);
                        cell.w -= delta;
                    }
                    (Axis::Y, Some(true)) => {
                        cell.y = cell.y.saturating_add(delta);
                        cell.h -= delta;
                    }
                    (_, None) => {}
                }
            }
            let Some(outer) = self.bsp.update_dividers(resized, gap) else {
                return false;
            };
            self.grid.update_dividers(&roots, &outer);
            return self.shared.is_empty()
                || SharedPair::can_freeze(
                    &self.shared,
                    area,
                    &self.cell_rects(area),
                    &self.occupants,
                    gap,
                );
        }
    }

    // A drag on the temporary divider can preserve the pending second move.
    fn drag_shared_divider(
        &mut self,
        index: usize,
        slot: usize,
        edge: Direction,
        coordinate: i32,
        area: Rect,
        cells: &[Rect],
    ) -> Option<bool> {
        let pair = &mut self.shared[index];
        let (source, target) = pair.rectangles(cells, &self.occupants, self.grid.gap)?;
        let Partition::Divider {
            axis,
            source_first,
            part,
            whole,
        } = &mut pair.partition
        else {
            return None;
        };
        if edge != axis.direction((pair.source == slot) != *source_first) {
            return None;
        }
        let (first, second, a, b) = if *source_first {
            (source, target, pair.source, pair.target)
        } else {
            (target, source, pair.target, pair.source)
        };
        let current = axis.length(first);
        let available = current + axis.length(second);
        let delta = i64::from(coordinate)
            - edge.coordinate(if slot == pair.source { source } else { target });
        let next = (i64::from(current) + delta).clamp(
            i64::from(axis.min_dimension(self.occupants[a].min)),
            i64::from(available - axis.min_dimension(self.occupants[b].min)),
        ) as i32;
        if next == current {
            return Some(false);
        }
        *part = next;
        *whole = available;
        Some(SharedPair::can_freeze(
            &self.shared,
            area,
            cells,
            &self.occupants,
            self.grid.gap,
        ))
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
            if self.shared[index].vacancy().is_some() {
                // Every participant starts a resize from its displayed frame.
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
            let (source, target) = pair.rectangles(&cells, &self.occupants, self.grid.gap)?;
            let previous = pair.partition.clone();
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
                if !SharedPair::can_freeze(
                    &self.shared,
                    area,
                    &cells,
                    &self.occupants,
                    self.grid.gap,
                ) {
                    self.shared[index].partition = previous;
                    return None;
                }
                return Some(edge);
            }
        }
        let (root_minima, children) = self.bsp.minimums(self.minimums(area), self.grid.gap);
        let roots = self.grid.rects(area);
        let count = roots.len();
        let edge = if let Some(edge) = self.bsp.resize(
            slot,
            (direction, shrink),
            roots,
            companion,
            &children,
            self.grid.gap,
        ) {
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
        self.bsp
            .fit(self.grid.rects(area), &children, self.grid.gap);
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
    fn sharing_swapping_and_resizing_use_the_configured_gap() {
        let area = Rect {
            x: -30,
            y: 15,
            w: 1100,
            h: 780,
        };
        for gap in [0, 23] {
            let mut layout = Layout::with_gap(gap);
            assert!(layout.insert(1, area));
            assert!(layout.insert_near(2, 1, area, (MIN_WIDTH, MIN_HEIGHT)));
            let start = layout.placements(area);
            assert!(layout.move_window(2, Direction::Left, area));
            let shared = layout.placements(area);
            assert_eq!(shared[0].1.joined(shared[1].1, Axis::Y, gap), Some(area));
            assert!(layout.move_window(2, Direction::Left, area));
            let moved = layout.placements(area);
            assert!(moved.contains(&(2, start[0].1)));
            assert!(moved.contains(&(1, start[1].1)));
            assert!(layout.resize(2, Direction::Right, false, area).is_some());
            let resized = layout.placements(area);
            assert_eq!(resized[0].1.joined(resized[1].1, Axis::X, gap), Some(area));
        }
    }

    #[test]
    fn spacing_changes_preserve_bsp_order_and_enforce_native_minima() {
        let area = Rect {
            x: 11,
            y: 19,
            w: 910,
            h: 500,
        };
        let mut layout = Layout::with_gap(0);
        assert!(layout.insert_with_minimum(1, area, (450, 150)));
        assert!(layout.insert_near(2, 1, area, (450, 150)));
        assert!(layout.insert_near(3, 2, area, (450, 150)));
        let frames = |layout: &Layout| {
            let frames: std::collections::BTreeMap<_, _> =
                layout.placements(area).into_iter().collect();
            [frames[&1], frames[&2], frames[&3]]
        };
        let [left, upper, lower] = frames(&layout);
        assert_eq!(left.x + left.w, upper.x);
        assert_eq!(upper.y + upper.h, lower.y);
        assert!(layout.clone().with_spacing(area, 11).is_none());
        let adjusted = layout.with_spacing(area, 10).unwrap();
        let [left, upper, lower] = frames(&adjusted);
        assert_eq!(left.w, 450);
        assert_eq!(upper.w, 450);
        assert_eq!(left.x + left.w + 10, upper.x);
        assert_eq!(upper.y + upper.h + 10, lower.y);
        assert_eq!(lower.x + lower.w, area.x + area.w);
        assert_eq!(lower.y + lower.h, area.y + area.h);
    }

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
