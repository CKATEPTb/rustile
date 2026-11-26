use super::{Axis, Direction, Rect};

type Size = (i32, i32);

// Find a full gutter with a roughly balanced number of tiles on each side.
fn gutter<T>(
    members: &mut [T],
    bounds: impl Fn(&T) -> (i64, i64),
    gap: i32,
) -> Option<(usize, i64)> {
    members.sort_unstable_by_key(|tile| bounds(tile).0);
    let mut edge = i64::MIN;
    let mut best = None;
    for split in 1..members.len() {
        edge = edge.max(bounds(&members[split - 1]).1);
        if bounds(&members[split]).0 - edge == i64::from(gap) {
            let distance = split.abs_diff(members.len() / 2);
            if best.is_none_or(|(score, _, _)| distance < score) {
                best = Some((distance, split, edge));
            }
        }
    }
    best.map(|(_, split, edge)| (split, edge))
}

/// Split the leaf currently at `at`. Its first child's share of the space
/// left after the gutter is `part / whole`, so the cut scales with its parent.
#[derive(Debug, Clone, Copy)]
pub(super) struct Cut {
    pub at: usize,
    pub axis: Axis,
    pub part: i32,
    pub whole: i32,
}

impl Cut {
    pub fn half(at: usize, axis: Axis) -> Self {
        Self {
            at,
            axis,
            part: 1,
            whole: 2,
        }
    }

    pub fn parts(self, rect: Rect, gap: i32) -> (Rect, Rect) {
        let length = self.axis.length(rect);
        let gap = gap.min(length);
        let available = length - gap;
        let first = ((i64::from(available) * i64::from(self.part) + i64::from(self.whole) / 2)
            / i64::from(self.whole)) as i32;
        self.axis.split_at(rect, first, gap)
    }
}

/// A sequence of binary cuts rooted in the packed grid cells. Each cut leaves
/// its first child at the original index and appends its second child, so a
/// single forward pass resolves all leaf rectangles.
#[derive(Debug, Clone, Default)]
pub(super) struct Bsp(pub Vec<Cut>);

impl Bsp {
    // Each pending pair must live on its own side of some lasting gutters.
    // Then finishing any selection of pairs leaves a representable layout.
    pub fn separates_pairs(regions: &mut [(Rect, bool)], gap: i32) -> bool {
        if regions.iter().filter(|r| r.1).nth(1).is_none() {
            return true;
        }
        for axis in [Axis::X, Axis::Y] {
            if let Some((split, _)) = gutter(regions, |r| axis.bounds(r.0), gap) {
                let (left, right) = regions.split_at_mut(split);
                return Self::separates_pairs(left, gap) && Self::separates_pairs(right, gap);
            }
        }
        false
    }

    // A half-completed keyboard move can rotate a pair or change a T junction.
    // Recover cuts from the displayed gutters when such a pair becomes a
    // permanent part of the layout. The returned order maps slots to old slots.
    pub fn from_rects(area: Rect, cells: &[Rect], gap: i32) -> Option<(Self, Vec<usize>)> {
        let mut bsp = Self::default();
        let mut order = vec![0];
        bsp.carve(0, area, (0..cells.len()).collect(), cells, &mut order, gap)?;
        Some((bsp, order))
    }

    fn carve(
        &mut self,
        at: usize,
        area: Rect,
        mut members: Vec<usize>,
        cells: &[Rect],
        order: &mut Vec<usize>,
        gap: i32,
    ) -> Option<()> {
        if let [only] = members[..] {
            order[at] = only;
            return (cells[only] == area).then_some(());
        }
        let longer = if area.w >= area.h { Axis::X } else { Axis::Y };
        for axis in [longer, longer.perpendicular()] {
            if let Some((split, edge)) = gutter(&mut members, |&i| axis.bounds(cells[i]), gap) {
                let cut = Cut {
                    at,
                    axis,
                    part: (edge - axis.bounds(area).0) as i32,
                    whole: axis.length(area) - gap,
                };
                let (a, b) = cut.parts(area, gap);
                let right_at = order.len();
                order.push(0);
                self.0.push(cut);
                let right = members.split_off(split);
                self.carve(at, a, members, cells, order, gap)?;
                return self.carve(right_at, b, right, cells, order, gap);
            }
        }
        None
    }

    pub fn rects(&self, mut cells: Vec<Rect>, gap: i32) -> Vec<Rect> {
        cells.reserve(self.0.len());
        for cut in &self.0 {
            let (a, b) = cut.parts(cells[cut.at], gap);
            cells[cut.at] = a;
            cells.push(b);
        }
        cells
    }

    pub fn parent_cut(&self, slot: usize, roots: usize) -> Option<usize> {
        self.0
            .iter()
            .enumerate()
            .rposition(|(i, cut)| slot == cut.at || slot == roots + i)
    }

    pub fn ancestors(&self, area: Rect, mut slot: usize, gap: i32) -> Vec<Rect> {
        let (_, parents) = self.leaves_and_parents(vec![area], gap);
        let mut path = Vec::new();
        for (i, cut) in self.0.iter().enumerate().rev() {
            if slot == cut.at || slot == i + 1 {
                path.push(parents[i]);
                slot = cut.at;
            }
        }
        path
    }

    fn leaves_and_parents(&self, mut cells: Vec<Rect>, gap: i32) -> (Vec<Rect>, Vec<Rect>) {
        let mut parents = Vec::with_capacity(self.0.len());
        for cut in &self.0 {
            parents.push(cells[cut.at]);
            let (first, second) = cut.parts(cells[cut.at], gap);
            cells[cut.at] = first;
            cells.push(second);
        }
        (cells, parents)
    }

    // Promote the other child into the removed leaf's parent. Slot indices
    // after the cut shift down when its appended child is removed.
    pub fn remove_leaf(&mut self, slot: usize, roots: usize) -> Option<(usize, usize)> {
        let i = self.parent_cut(slot, roots)?;
        let child = roots + i;
        let parent = self.0.remove(i).at;
        for cut in &mut self.0 {
            if cut.at == child {
                cut.at = parent;
            } else if cut.at > child {
                cut.at -= 1;
            }
        }
        Some((child, parent))
    }

    pub fn root(&self, mut slot: usize, roots: usize) -> usize {
        while slot >= roots {
            slot = self.0[slot - roots].at;
        }
        slot
    }

    pub fn divider_scope(
        &self,
        mut slot: usize,
        edge: Direction,
        roots: &[Rect],
        gap: i32,
    ) -> Option<Rect> {
        if self.0.is_empty() {
            return None;
        }
        let (leaves, parents) = self.leaves_and_parents(roots.to_vec(), gap);
        let coordinate = edge.coordinate(leaves[slot]);
        for (i, cut) in self.0.iter().enumerate().rev() {
            let trailing = slot == roots.len() + i;
            if !trailing && slot != cut.at {
                continue;
            }
            slot = cut.at;
            if cut.axis == edge.axis() && trailing == edge.towards_start() {
                let (a, b) = cut.parts(parents[i], gap);
                if edge.coordinate(if trailing { b } else { a }) == coordinate {
                    return Some(parents[i]);
                }
            }
        }
        None
    }

    // Keep the existing tree. Folding its leaves back into their original
    // parents also keeps interior dividers stationary on a mouse drag.
    pub fn update_dividers(&mut self, mut cells: Vec<Rect>, gap: i32) -> Option<Vec<Rect>> {
        for cut in self.0.iter_mut().rev() {
            let trailing = cells.pop()?;
            let leading = cells[cut.at];
            let parent = leading.joined(trailing, cut.axis, gap)?;
            if cut.parts(parent, gap).0 != leading {
                cut.part = cut.axis.length(leading);
                cut.whole = cut.axis.length(parent) - gap;
            }
            cells[cut.at] = parent;
        }
        Some(cells)
    }

    // Fold child requirements upward before fitting tracks or moving a
    // divider. Save each pair before its leading slot is reused by its parent.
    pub fn minimums(&self, mut sizes: Vec<Size>, gap: i32) -> (Vec<Size>, Vec<(Size, Size)>) {
        let mut children = Vec::with_capacity(self.0.len());
        for cut in self.0.iter().rev() {
            let b = sizes.pop().expect("a second child for every cut");
            let a = sizes[cut.at];
            children.push((a, b));
            sizes[cut.at] = match cut.axis {
                Axis::X => (a.0.saturating_add(gap).saturating_add(b.0), a.1.max(b.1)),
                Axis::Y => (a.0.max(b.0), a.1.saturating_add(gap).saturating_add(b.1)),
            };
        }
        children.reverse();
        (sizes, children)
    }

    pub fn fit(&mut self, mut cells: Vec<Rect>, minimums: &[(Size, Size)], gap: i32) {
        for (cut, &(a, b)) in self.0.iter_mut().zip(minimums) {
            let rect = cells[cut.at];
            let total = cut.axis.length(rect) - gap;
            let (mut first, mut second) = cut.parts(rect, gap);
            let old = cut.axis.length(first);
            let next = old.clamp(cut.axis.min_dimension(a), total - cut.axis.min_dimension(b));
            if old != next {
                cut.part = next;
                cut.whole = total;
                (first, second) = cut.parts(rect, gap);
            }
            cells[cut.at] = first;
            cells.push(second);
        }
    }
}
