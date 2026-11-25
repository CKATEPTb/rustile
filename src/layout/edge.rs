use super::{Bsp, Cut, Direction, Grid, Layout, Rect, Shape};

impl Layout {
    // Moving outwards can turn a split: two windows beside one another become
    // a stack. Start with an adjacent pair, then try the enclosing BSP branches
    // from smallest to largest when the pair's native minima prevent that turn.
    pub(super) fn move_to_edge(&mut self, slot: usize, dir: Direction, area: Rect) -> bool {
        let cells = self.cell_rects(area);
        let from = cells[slot];
        let across = dir.axis().perpendicular();
        if across.length(from) == across.length(area) {
            return false;
        }
        let mut scopes: Vec<_> = cells
            .iter()
            .filter_map(|&r| from.joined(r, across, self.grid.gap))
            .collect();
        if let Some((tree, order)) = Bsp::from_rects(area, &cells, self.grid.gap) {
            let at = order.iter().position(|&i| i == slot).unwrap();
            scopes.extend(tree.ancestors(area, at, self.grid.gap));
        }
        scopes.sort_by_key(|r| i64::from(r.w) * i64::from(r.h));
        scopes.dedup();
        for scope in scopes {
            if across.length(scope) <= across.length(from) {
                continue;
            }
            let slots: Vec<_> = (0..cells.len())
                .filter(|&i| scope.contains(cells[i]))
                .collect();
            let local: Vec<_> = slots.iter().map(|&i| cells[i]).collect();
            let Some((mut tree, order)) = Bsp::from_rects(scope, &local, self.grid.gap) else {
                continue;
            };
            let mut rest: Vec<_> = order.iter().map(|&i| slots[i]).collect();
            let at = rest.iter().position(|&i| i == slot).unwrap();
            let (child, parent) = tree.remove_leaf(at, 1).expect("scope contains a neighbor");
            if at == parent {
                rest.swap(child, parent);
            }
            rest.remove(child);
            let proposal = EdgeMove {
                area,
                scope,
                slot,
                dir,
                cells: &cells,
            };
            if proposal.apply(self, &tree, &rest) {
                return true;
            }
            // Keeping the other windows in their old stack may cost more
            // height than exists on the monitor. Try packing those in this
            // branch across the remaining space, in their visual order.
            if rest.len() < 2 {
                continue;
            }
            rest.sort_by_key(|&i| {
                let rect = cells[i];
                (across.bounds(rect).0, dir.axis().bounds(rect).0)
            });
            let mins: Vec<_> = rest.iter().map(|&i| self.occupants[i].min).collect();
            for shape in Shape::candidates(rest.len(), scope, self.grid.gap) {
                let mut grid = Grid::new(self.grid.gap);
                grid.set_shape(shape);
                if !grid.fit(scope, &mins) {
                    continue;
                }
                if let Some((packed, order)) =
                    Bsp::from_rects(scope, &grid.rects(scope), self.grid.gap)
                {
                    let order: Vec<_> = order.iter().map(|&i| rest[i]).collect();
                    if proposal.apply(self, &packed, &order) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

struct EdgeMove<'a> {
    area: Rect,
    scope: Rect,
    slot: usize,
    dir: Direction,
    cells: &'a [Rect],
}

impl EdgeMove<'_> {
    fn apply(&self, layout: &mut Layout, remaining: &Bsp, rest: &[usize]) -> bool {
        let axis = self.dir.axis();
        let across = axis.perpendicular();
        let source = self.cells[self.slot];
        let total = axis.length(self.scope) - layout.grid.gap;
        if total < 2 {
            return false;
        }
        // Keep the active window's footprint when turning or widening it.
        let area = i64::from(source.w) * i64::from(source.h);
        let width = i64::from(across.length(self.scope));
        let preferred = ((area + width / 2) / width) as i32;
        let (needs, _) = remaining.minimums(
            rest.iter().map(|&i| layout.occupants[i].min).collect(),
            layout.grid.gap,
        );
        let min = axis.min_dimension(layout.occupants[self.slot].min);
        let max = total - axis.min_dimension(needs[0]);
        let active = if min <= max {
            preferred.clamp(min, max)
        } else {
            preferred.clamp(1, total - 1)
        };
        let first = self.dir.towards_start();
        let (a, b) = Cut {
            at: 0,
            axis,
            part: if first { active } else { total - active },
            whole: total,
        }
        .parts(self.scope, layout.grid.gap);
        let (front, back) = if first { (a, b) } else { (b, a) };
        let mut rects = self.cells.to_vec();
        rects[self.slot] = front;
        for (&i, r) in rest
            .iter()
            .zip(remaining.rects(vec![back], layout.grid.gap))
        {
            if r.w <= 0 || r.h <= 0 {
                return false;
            }
            rects[i] = r;
        }
        let shares = layout
            .shared
            .iter()
            .filter(|pair| pair.slots().all(|slot| rects[slot] == self.cells[slot]))
            .cloned()
            .collect();
        let mut candidate = layout.clone();
        if !candidate.rebuild_cells(self.area, &rects, shares) {
            return false;
        }
        let (needs, children) = candidate
            .bsp
            .minimums(candidate.minimums(self.area), layout.grid.gap);
        if self.area.w < needs[0].0 || self.area.h < needs[0].1 {
            return false;
        }
        candidate
            .bsp
            .fit(vec![self.area], &children, layout.grid.gap);
        candidate.settle_invalid_pairs(self.area);
        let id = layout.occupants[self.slot].id;
        let slot = candidate.slot(id).expect("edge move keeps its window");
        if !self
            .dir
            .advances(source, candidate.cell_rects(self.area)[slot])
        {
            return false;
        }
        *layout = candidate;
        true
    }
}
