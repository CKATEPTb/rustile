use super::{Cut, Direction, Id, Layout, Rect, SharedPair, Vacancy, Window};

/// Intent inferred from the pointer inside a visible target tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropZone {
    /// Exchange occupants if they both fit in the other's frame.
    Center,
    /// Place the dragged window along this edge of the destination.
    Edge(Direction),
}

impl DropZone {
    fn at(rect: Rect, (x, y): (i32, i32)) -> Option<Self> {
        let (w, h) = (i64::from(rect.w), i64::from(rect.h));
        let (dx, dy) = (
            i64::from(x) - i64::from(rect.x),
            i64::from(y) - i64::from(rect.y),
        );
        if !(0..w).contains(&dx) || !(0..h).contains(&dy) {
            return None;
        }
        // Use pixel centers, so opposite edges have identical hit zones even
        // on odd-sized frames. Compare fractions without aspect-ratio bias.
        let (x, y) = (2 * dx + 1, 2 * dy + 1);
        let horizontal = x.min(2 * w - x);
        let vertical = y.min(2 * h - y);
        if 2 * horizontal >= w && 2 * vertical >= h {
            return Some(Self::Center);
        }
        let edge = if horizontal * h <= vertical * w {
            if x <= w {
                Direction::Left
            } else {
                Direction::Right
            }
        } else if y <= h {
            Direction::Up
        } else {
            Direction::Down
        };
        Some(Self::Edge(edge))
    }
}

impl Layout {
    pub fn drop_target(&self, source: Id, point: (i32, i32), area: Rect) -> Option<(Id, DropZone)> {
        self.slot(source)?;
        self.occupants
            .iter()
            .zip(self.visible_rects(area))
            .filter(|(w, _)| w.id != source)
            .find_map(|(w, rect)| DropZone::at(rect, point).map(|zone| (w.id, zone)))
    }

    /// Return a candidate layout and the dragged window's resulting visible
    /// frame. The manager can draw that frame before committing the layout.
    pub fn plan_drop(
        &self,
        source: Id,
        target: Id,
        zone: DropZone,
        area: Rect,
    ) -> Option<(Self, Rect)> {
        if source == target {
            return None;
        }
        let (from, to) = (self.slot(source)?, self.slot(target)?);
        let visible = self.visible_rects(area);
        if zone == DropZone::Center && !self.swap_fits(from, to, &visible) {
            return None;
        }
        let (previous, destination) = (visible[from], visible[to]);
        let mut next = self.clone();
        // Use the actual frames under the cursor even between two keyboard
        // moves. Distant previews keep their independent second step.
        while let Some(i) = next.shared.iter().position(|p| {
            p.slots().any(|s| {
                let id = next.occupants[s].id;
                id == source || id == target
            })
        }) {
            if !next.freeze_pair(i, area) {
                return None;
            }
        }
        let (from, to) = (next.slot(source)?, next.slot(target)?);
        match zone {
            DropZone::Center => {
                next.occupants.swap(from, to);
                Some((next, destination))
            }
            DropZone::Edge(edge) => {
                let next = next.drop_on_edge(from, to, edge, area)?;
                let frame = next.cell_rects(area)[next.slot(source)?];
                (frame != previous).then_some((next, frame))
            }
        }
    }

    fn drop_on_edge(
        &self,
        source: usize,
        target: usize,
        edge: Direction,
        area: Rect,
    ) -> Option<Self> {
        let cells = self.visible_rects(area);
        let gap = self.grid.gap;
        let (from, host) = (cells[source], cells[target]);
        // Prefer a split inside the hovered tile's exact rectangle. Fill the
        // source using as few other windows as possible.
        if edge
            .axis()
            .divider_limits(host, self.occupants[source], self.occupants[target], gap)
            .is_some()
        {
            let mut vacancies = Vec::new();
            for dir in [edge, edge.axis().direction(!edge.towards_start())] {
                vacancies.extend(Vacancy::candidates(source, target, dir, &cells, gap));
            }
            vacancies.sort_by_key(|v| v.slots().count());
            for vacancy in vacancies {
                let mut released = cells.clone();
                if vacancy.apply(source, &mut released, gap)
                    && let Some(next) =
                        self.drop_into_space(source, target, edge, area, &cells, released)
                {
                    return Some(next);
                }
            }
        }
        // With two tiles, or when the host is the only available neighbor, it
        // can take the released space and share the combined region.
        for axis in [edge.axis(), edge.axis().perpendicular()] {
            if let Some(joined) = from.joined(host, axis, gap) {
                if axis == edge.axis()
                    && (axis.bounds(from).0 < axis.bounds(host).0) == edge.towards_start()
                {
                    return None;
                }
                let mut released = cells.clone();
                released[target] = joined;
                if let Some(next) =
                    self.drop_into_space(source, target, edge, area, &cells, released)
                {
                    return Some(next);
                }
            }
        }
        // A staggered subtree can have no single side that can fill the
        // vacancy. Promote its sibling, then split the target and let only
        // the dividers constrained by native minimums move.
        let mut next = self.clone();
        let (moving, target_id) = (self.occupants[source], self.occupants[target].id);
        let roots = self.occupants.len() - self.bsp.0.len();
        if next.bsp.parent_cut(source, roots).is_none() {
            next.rebuild_cells(area, &self.cell_rects(area), self.shared.clone())?;
        }
        next.remove_local(next.slot(moving.id)?);
        let at = next.slot(target_id)?;
        next.attach_at_edge(at, moving, edge);
        next.with_spacing(area, gap)
    }

    fn drop_into_space(
        &self,
        source: usize,
        target: usize,
        edge: Direction,
        area: Rect,
        visible: &[Rect],
        mut released: Vec<Rect>,
    ) -> Option<Self> {
        let gap = self.grid.gap;
        let moving = self.occupants[source];
        let host = self.occupants[target];
        edge.axis()
            .divider_limits(released[target], moving, host, gap)?;
        let raw = self.cell_rects(area);
        let mut remaining = Vec::new();
        for pair in &self.shared {
            if pair
                .slots()
                .all(|s| s != source && s != target && released[s] == visible[s])
            {
                for s in pair.slots() {
                    released[s] = raw[s];
                }
                let mut retained = pair.clone();
                retained.remap(|s| s - usize::from(s > source));
                remaining.push(retained);
            }
        }
        let mut next = self.clone();
        next.occupants.remove(source);
        released.remove(source);
        next.rebuild_cells(area, &released, remaining)?;
        let at = next.slot(host.id)?;
        next.attach_at_edge(at, moving, edge);
        let (_, children) = next
            .bsp
            .minimums(next.occupants.iter().map(|w| w.min).collect(), gap);
        next.bsp.fit(vec![area], &children, gap);
        (next.shared.is_empty()
            || SharedPair::can_freeze(
                &next.shared,
                area,
                &next.cell_rects(area),
                &next.occupants,
                gap,
            ))
        .then_some(next)
    }

    fn attach_at_edge(&mut self, at: usize, moving: Window, edge: Direction) {
        self.bsp.0.push(Cut::half(at, edge.axis()));
        let trailing = if edge.towards_start() {
            std::mem::replace(&mut self.occupants[at], moving)
        } else {
            moving
        };
        self.occupants.push(trailing);
    }
}
