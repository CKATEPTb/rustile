use super::{Axis, Direction, Rect};

#[derive(Clone, Debug)]
pub(super) struct Vacancy {
    recipients: Vec<usize>,
    fill: Fill,
}

#[derive(Clone, Debug)]
enum Fill {
    // Order is geometric, so remapping slot indices preserves the band.
    Band { axis: Axis, source_at: usize },
    Edge(Direction),
}

impl Vacancy {
    pub(super) fn candidates(
        source: usize,
        target: usize,
        direction: Direction,
        cells: &[Rect],
        gap: i32,
    ) -> Vec<Self> {
        let from = cells[source];
        let host = cells[target];
        let across = direction.axis().perpendicular();
        let (a, b) = across.bounds(from);
        let (c, d) = across.bounds(host);
        let preferred = (b - a < d - c && (a == c || b == d)).then(|| across.direction(b == d));
        let mut candidates = Vec::new();
        let mut add = |candidate: Self| {
            if !candidates
                .iter()
                .any(|other: &Self| other.recipients == candidate.recipients)
            {
                candidates.push(candidate);
            }
        };
        // Aligned T junctions already have a natural continuation of the seam.
        if let Some(side) = preferred
            && let Some(i) = cells.iter().position(|&r| {
                side.distance(from, r) == i64::from(gap) && from.joined(r, across, gap).is_some()
            })
        {
            add(Self {
                recipients: vec![i],
                fill: Fill::Edge(side),
            });
        }

        let mut line: Vec<_> = (0..cells.len())
            .filter(|&i| {
                i != target && direction.axis().bounds(cells[i]) == direction.axis().bounds(from)
            })
            .collect();
        line.sort_unstable_by_key(|&i| across.bounds(cells[i]).0);
        let adjacent = |&i: &usize, &j: &usize| {
            across.bounds(cells[j]).0 - across.bounds(cells[i]).1 == i64::from(gap)
        };
        if let Some(run) = line
            .chunk_by(adjacent)
            .find(|run| run.len() > 1 && run.contains(&source))
        {
            let mut recipients = run.to_vec();
            let source_at = recipients.iter().position(|&i| i == source).unwrap();
            recipients.remove(source_at);
            add(Self {
                recipients,
                fill: Fill::Band {
                    axis: across,
                    source_at,
                },
            });
        }

        // A subdivided neighboring branch may have several windows facing
        // the vacancy. Try each full side, including the side behind motion.
        for side in [
            across.direction(true),
            across.direction(false),
            direction.axis().direction(!direction.towards_start()),
        ] {
            let along = side.axis().perpendicular();
            let (start, end) = along.bounds(from);
            let mut recipients: Vec<_> = (0..cells.len())
                .filter(|&i| {
                    let (lo, hi) = along.bounds(cells[i]);
                    i != source
                        && i != target
                        && side.distance(from, cells[i]) == i64::from(gap)
                        && lo >= start
                        && hi <= end
                })
                .collect();
            recipients.sort_unstable_by_key(|&i| along.bounds(cells[i]).0);
            if full_edge(side, from, &recipients, cells, gap) {
                add(Self {
                    recipients,
                    fill: Fill::Edge(side),
                });
            }
        }
        candidates
    }

    pub(super) fn slots(&self) -> impl Iterator<Item = usize> + '_ {
        self.recipients.iter().copied()
    }

    pub(super) fn remap(&mut self, map: impl Fn(usize) -> usize) {
        for i in &mut self.recipients {
            *i = map(*i);
        }
    }

    pub(super) fn apply(&self, source: usize, cells: &mut [Rect], gap: i32) -> bool {
        let from = cells[source];
        let ids = &self.recipients;
        if from.w <= 0 || from.h <= 0 || ids.iter().any(|&i| cells[i].w <= 0 || cells[i].h <= 0) {
            return false;
        }
        match self.fill {
            Fill::Band { axis, source_at } => {
                let mut run = ids[..source_at]
                    .iter()
                    .copied()
                    .chain([source])
                    .chain(ids[source_at..].iter().copied());
                let first = cells[run.next().unwrap()];
                let mut end = axis.bounds(first).1;
                for i in run {
                    let next = cells[i];
                    let (lo, hi) = axis.bounds(next);
                    if lo - end != i64::from(gap)
                        || axis.perpendicular().bounds(next) != axis.perpendicular().bounds(first)
                    {
                        return false;
                    }
                    end = hi;
                }
                // Distribute the released tile and its now-redundant gutter
                // by prior widths, rounding cumulative shares so no pixel is lost.
                let total: i64 = ids.iter().map(|&i| i64::from(axis.length(cells[i]))).sum();
                let extra = i64::from(axis.length(from)) + i64::from(gap);
                let mut pos = axis.bounds(first).0;
                let mut consumed = 0;
                let mut assigned = 0;
                for &i in ids {
                    let width = i64::from(axis.length(cells[i]));
                    consumed += width;
                    let share = (extra * consumed + total / 2) / total;
                    let length = width + share - assigned;
                    cells[i] = with_span(cells[i], axis, pos, length);
                    pos += length + i64::from(gap);
                    assigned = share;
                }
            }
            Fill::Edge(side) => {
                if !full_edge(side, from, ids, cells, gap) {
                    return false;
                }
                let axis = side.axis();
                let (lo, hi) = axis.bounds(from);
                for &i in ids {
                    let (a, b) = axis.bounds(cells[i]);
                    let start = lo.min(a);
                    cells[i] = with_span(cells[i], axis, start, hi.max(b) - start);
                }
            }
        }
        true
    }
}

fn full_edge(side: Direction, from: Rect, ids: &[usize], cells: &[Rect], gap: i32) -> bool {
    let along = side.axis().perpendicular();
    let (mut next, end) = along.bounds(from);
    for &i in ids {
        let (a, b) = along.bounds(cells[i]);
        if next != a || side.distance(from, cells[i]) != i64::from(gap) {
            return false;
        }
        next = b + i64::from(gap);
    }
    !ids.is_empty() && next == end + i64::from(gap)
}

fn with_span(rect: Rect, axis: Axis, start: i64, length: i64) -> Rect {
    match axis {
        Axis::X => Rect {
            x: start as i32,
            w: length as i32,
            ..rect
        },
        Axis::Y => Rect {
            y: start as i32,
            h: length as i32,
            ..rect
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    #[test]
    fn a_move_into_the_host_reclaims_the_source_through_a_subdivided_side() {
        let rect = |x, y, w, h| Rect { x, y, w, h };
        let area = rect(0, 0, 1400, 900);
        let cells = [
            rect(0, 0, 1400, 300),    // host above the three columns
            rect(470, 308, 320, 592), // source in the middle column
            rect(0, 308, 462, 261),
            rect(0, 577, 462, 323),
            rect(798, 308, 602, 238),
            rect(798, 554, 602, 346),
        ];
        let mut layout = Layout::default();
        for id in 0..cells.len() {
            assert!(layout.insert(id as isize, area));
        }
        assert!(layout.rebuild_cells(area, &cells, vec![]).is_some());
        let frames = |layout: &Layout| {
            let mut by_id = layout.placements(area);
            by_id.sort_by_key(|p| p.0);
            by_id.into_iter().map(|p| p.1).collect::<Vec<_>>()
        };
        assert!(layout.move_window(1, Direction::Up, area));
        let shared = frames(&layout);
        assert_eq!(shared[1].joined(shared[0], Axis::X, 8), Some(cells[0]));
        assert_eq!(shared[2], Rect { w: 790, ..cells[2] });
        assert_eq!(shared[3], Rect { w: 790, ..cells[3] });
        assert_eq!(shared[4..], cells[4..]);

        assert!(layout.move_window(1, Direction::Up, area));
        let mut taken = cells;
        taken.swap(0, 1);
        assert_eq!(frames(&layout), taken);
    }

    #[test]
    fn a_subdivided_edge_can_reclaim_the_source_only_while_its_full_span_matches() {
        let rect = |x, y, w, h| Rect { x, y, w, h };
        let cells = [
            rect(150, 0, 790, 292),   // host above
            rect(400, 300, 300, 400), // source
            rect(180, 300, 212, 150), // two tiles along its left edge
            rect(60, 458, 332, 242),
            rect(400, 708, 300, 200), // alternate filler behind the movement
        ];
        let plans = Vacancy::candidates(1, 0, Direction::Up, &cells, 8);
        assert_eq!(plans.len(), 2);
        let mut shared = cells;
        assert!(plans[0].apply(1, &mut shared, 8));
        assert_eq!(shared[2], rect(180, 300, 520, 150));
        assert_eq!(shared[3], rect(60, 458, 640, 242));
        assert_eq!(shared[4], cells[4]);

        let mut changed = cells;
        changed[3].y += 2;
        let before = changed;
        assert!(!plans[0].apply(1, &mut changed, 8));
        assert_eq!(changed, before);
        assert!(plans[1].apply(1, &mut changed, 8));
        assert_eq!(changed[4], rect(400, 300, 300, 608));
    }
}
