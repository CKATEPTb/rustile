use std::collections::BTreeMap;

use super::{Direction, GAP, Id, Layout, MIN_HEIGHT, MIN_WIDTH, Rect};

const AREA: Rect = Rect {
    x: -1903,
    y: -41,
    w: 1901,
    h: 1067,
};

type Frames = BTreeMap<Id, Rect>;

fn populated(count: Id, area: Rect) -> Layout {
    let mut layout = Layout::default();
    for id in 1..=count {
        assert!(layout.insert(id, area), "could not insert window {id}");
    }
    layout
}

fn frames(layout: &Layout, area: Rect) -> Frames {
    let placed = layout.placements(area);
    let frames: Frames = placed.iter().copied().collect();
    assert_eq!(placed.len(), frames.len(), "duplicate window in layout");
    frames
}

fn rows(frames: &Frames) -> Vec<Vec<Id>> {
    let mut ordered: BTreeMap<i32, Vec<Id>> = BTreeMap::new();
    for (&id, rect) in frames {
        ordered.entry(rect.y).or_default().push(id);
    }
    for row in ordered.values_mut() {
        row.sort_by_key(|id| frames[id].x);
    }
    ordered.into_values().collect()
}

fn transpose(rect: Rect) -> Rect {
    Rect {
        x: rect.y,
        y: rect.x,
        w: rect.h,
        h: rect.w,
    }
}

// Each band must extend all the way to both edges. Counting windows alone
// would miss a phantom cell at the end of an incomplete row.
fn assert_packed(layout: &Layout, count: usize, area: Rect) {
    let placed = frames(layout, area);
    assert_eq!(placed.len(), count);
    let mut cells = layout.cell_rects(area);
    assert_eq!(cells.len(), count);
    if count == 0 {
        return;
    }
    for rect in placed.values() {
        assert!(rect.w >= MIN_WIDTH && rect.h >= MIN_HEIGHT, "{rect:?}");
    }
    let sort_key = |r: &Rect| (r.y, r.x, r.h, r.w);
    let mut windows: Vec<_> = placed.values().copied().collect();
    cells.sort_by_key(sort_key);
    windows.sort_by_key(sort_key);
    assert_eq!(
        cells, windows,
        "ordinary layout must occupy every real cell"
    );

    let (bands, area) = if area.w >= area.h {
        (placed, area)
    } else {
        (
            placed
                .into_iter()
                .map(|(id, r)| (id, transpose(r)))
                .collect(),
            transpose(area),
        )
    };
    let mut top = area.y;
    for band in rows(&bands) {
        let mut left = area.x;
        let height = bands[&band[0]].h;
        for id in band {
            let cell = bands[&id];
            assert_eq!((cell.x, cell.y, cell.h), (left, top, height));
            left += cell.w + GAP;
        }
        assert_eq!(left - GAP, area.x + area.w);
        top += height + GAP;
    }
    assert_eq!(top - GAP, area.y + area.h);
}

fn stationary(before: &Frames, after: &Frames, moving: Id, neighbor: Id) {
    assert!(before.keys().eq(after.keys()));
    for (&id, old) in before {
        if id != moving && id != neighbor {
            assert_eq!(after[&id], *old, "unrelated tile {id} moved");
        }
    }
}

#[test]
fn packed_bands_follow_window_count_on_wide_and_tall_work_areas() {
    let large = Rect {
        w: 3427,
        h: 2191,
        ..AREA
    };
    for area in [large, transpose(large)] {
        for count in [0, 1, 2, 3, 5, 6, 10, 17, 40, 81, 103] {
            assert_packed(&populated(count, area), count as usize, area);
        }
    }
    let two = frames(&populated(2, AREA), AREA);
    assert_eq!(rows(&two).iter().map(Vec::len).collect::<Vec<_>>(), [2]);
    assert!((two[&1].w - two[&2].w).abs() <= 1);
    let five = frames(&populated(5, AREA), AREA);
    assert_eq!(rows(&five).iter().map(Vec::len).collect::<Vec<_>>(), [3, 2]);
}

fn assert_horizontal_share(before: &Frames, after: &Frames, source: Id, neighbor: Id) {
    stationary(before, after, source, neighbor);
    let old = before[&source];
    let host = before[&neighbor];
    let moved = after[&source];
    let reduced = after[&neighbor];
    let (upper, lower, direction) = if old.x < host.x {
        (reduced, moved, Direction::Right)
    } else {
        (moved, reduced, Direction::Left)
    };
    let combined = old.joined(host, super::Axis::X, GAP).unwrap();
    assert_eq!(upper.joined(lower, super::Axis::Y, GAP), Some(combined));
    assert_above(upper, lower);
    assert_progress(old, moved, direction);
}

fn assert_progress(old: Rect, moved: Rect, direction: Direction) {
    let (a, b) = direction.axis().bounds(old);
    let (c, d) = direction.axis().bounds(moved);
    assert!(
        if direction.towards_start() {
            c + d < a + b
        } else {
            c + d > a + b
        },
        "center failed to move {direction:?}: {old:?} -> {moved:?}"
    );
    assert!(
        old.x < moved.x
            || old.y < moved.y
            || old.x + old.w > moved.x + moved.w
            || old.y + old.h > moved.y + moved.h,
        "move {direction:?} only expanded the existing tile: {old:?} -> {moved:?}"
    );
}

fn assert_tiled(tiles: &Frames, area: Rect) {
    // Assign each tile the gutter to its right and below. These extended
    // rectangles must exactly cover the similarly extended work area.
    let footprint = |r: Rect| i64::from(r.w + GAP) * i64::from(r.h + GAP);
    let mut covered = 0;
    for (&id, &a) in tiles {
        assert!(a.w >= MIN_WIDTH && a.h >= MIN_HEIGHT);
        assert!(a.x >= area.x && a.x + a.w <= area.x + area.w);
        assert!(a.y >= area.y && a.y + a.h <= area.y + area.h);
        for (_, &b) in tiles.range((id + 1)..) {
            assert!(
                a.x + a.w + GAP <= b.x
                    || b.x + b.w + GAP <= a.x
                    || a.y + a.h + GAP <= b.y
                    || b.y + b.h + GAP <= a.y,
                "{a:?} touches {b:?}"
            );
        }
        covered += footprint(a);
    }
    assert_eq!(covered, footprint(area), "hole in layout: {tiles:?}");
}

#[test]
fn horizontal_repeats_share_only_the_next_cell_then_exchange_its_exact_slot() {
    for direction in [Direction::Right, Direction::Left] {
        let mut layout = populated(6, AREA);
        let original = frames(&layout, AREA);
        let mut order = rows(&original).remove(0);
        if direction == Direction::Left {
            order.reverse();
        }
        let [a, b, c] = order[..] else {
            panic!("expected a row of three windows")
        };

        assert!(layout.move_window(a, direction, AREA));
        assert_horizontal_share(&original, &frames(&layout, AREA), a, b);
        assert!(layout.move_window(a, direction, AREA));
        let exchanged = frames(&layout, AREA);
        stationary(&original, &exchanged, a, b);
        assert_eq!([exchanged[&a], exchanged[&b]], [original[&b], original[&a]]);

        assert!(layout.move_window(a, direction, AREA));
        assert_horizontal_share(&exchanged, &frames(&layout, AREA), a, c);
        assert!(layout.move_window(a, direction, AREA));
        let edge = frames(&layout, AREA);
        stationary(&exchanged, &edge, a, c);
        assert_eq!([edge[&a], edge[&c]], [original[&c], original[&b]]);
        // The direct neighbors are exhausted. The tile below still shares
        // the same outer edge, so another press can use that column.
        let column = rows(&original)[1][if direction == Direction::Left { 0 } else { 2 }];
        assert!(layout.move_window(a, direction, AREA));
        let extended = frames(&layout, AREA);
        stationary(&edge, &extended, a, column);
        let (outer, inner) = (extended[&a], extended[&column]);
        assert_eq!(outer.h, AREA.h);
        assert_eq!(inner.h, AREA.h);
        if direction == Direction::Left {
            assert_eq!(outer.x + outer.w + GAP, inner.x);
        } else {
            assert_eq!(inner.x + inner.w + GAP, outer.x);
        }
    }
}

#[test]
fn aligned_vertical_neighbors_share_their_combined_area_and_leave_other_cells_in_place() {
    for direction in [Direction::Down, Direction::Up] {
        let mut layout = populated(6, AREA);
        let original = frames(&layout, AREA);
        let bands = rows(&original);
        let pair = [bands[0][1], bands[1][1]];
        let [moving, host] = if direction == Direction::Down {
            pair
        } else {
            [pair[1], pair[0]]
        };

        assert!(layout.move_window(moving, direction, AREA));
        let shared = frames(&layout, AREA);
        stationary(&original, &shared, moving, host);
        assert_progress(original[&moving], shared[&moving], direction);
        let mut allocated = [shared[&moving], shared[&host]];
        allocated.sort_by_key(|r| r.x);
        let [left, right] = allocated;
        let [top, bottom] = pair.map(|id| original[&id]);
        for tile in allocated {
            assert_eq!(tile.y, top.y);
            assert_eq!(tile.y + tile.h, bottom.y + bottom.h);
        }
        assert_eq!(left.x, top.x);
        assert_eq!(left.x + left.w + GAP, right.x);
        assert_eq!(right.x + right.w, top.x + top.w);

        assert!(layout.move_window(moving, direction, AREA));
        let exchanged = frames(&layout, AREA);
        stationary(&original, &exchanged, moving, host);
        assert_eq!(
            [exchanged[&moving], exchanged[&host]],
            [original[&host], original[&moving]]
        );
    }
}

#[test]
fn moving_a_different_pair_preserves_an_existing_shared_pair() {
    let mut layout = populated(6, AREA);
    let original = frames(&layout, AREA);
    let bands = rows(&original);
    assert!(layout.move_window(bands[0][0], Direction::Right, AREA));
    let shared = frames(&layout, AREA);
    assert_horizontal_share(&original, &shared, bands[0][0], bands[0][1]);
    assert!(layout.move_window(bands[1][0], Direction::Right, AREA));
    assert_horizontal_share(&shared, &frames(&layout, AREA), bands[1][0], bands[1][1]);
}

#[test]
fn a_filler_extending_past_the_host_can_reclaim_the_vacated_junction_tile() {
    let mut layout = populated(5, AREA);
    let original = frames(&layout, AREA);
    let bands = rows(&original);
    let moving = bands[0][0];
    let host = bands[1][0];
    let filler = bands[0][1];
    assert_ne!(original[&moving].w, original[&host].w);
    assert!(layout.move_window(moving, Direction::Down, AREA));
    let after = frames(&layout, AREA);
    assert_progress(original[&moving], after[&moving], Direction::Down);
    assert_eq!(
        after[&moving],
        Rect {
            w: original[&moving].w,
            ..original[&host]
        }
    );
    assert_eq!(
        after[&moving].joined(after[&host], super::Axis::X, GAP),
        Some(original[&host])
    );
    assert_eq!(
        after[&filler],
        original[&moving]
            .joined(original[&filler], super::Axis::X, GAP)
            .unwrap()
    );
    for id in [bands[0][2], bands[1][1]] {
        assert_eq!(after[&id], original[&id]);
    }
    assert!(layout.move_window(moving, Direction::Down, AREA));
    let taken = frames(&layout, AREA);
    stationary(&original, &taken, moving, host);
    assert_eq!(
        [taken[&moving], taken[&host]],
        [original[&host], original[&moving]]
    );
}

#[test]
fn changing_direction_can_only_touch_the_source_and_its_old_and_new_hosts() {
    let mut layout = populated(6, AREA);
    let original = frames(&layout, AREA);
    let bands = rows(&original);
    let source = bands[0][0];
    let old_host = bands[0][1];
    assert!(layout.move_window(source, Direction::Right, AREA));
    let before = frames(&layout, AREA);
    assert!(layout.move_window(source, Direction::Down, AREA));
    let after = frames(&layout, AREA);
    let downstream: Vec<_> = bands[1]
        .iter()
        .copied()
        .filter(|id| before[id] != after[id])
        .collect();
    let [new_host] = downstream[..] else {
        panic!("changing rows must involve exactly one window below the pair");
    };
    assert!(bands[1][..2].contains(&new_host));
    for (&id, old) in &before {
        if ![source, old_host, new_host].contains(&id) {
            assert_eq!(after[&id], *old);
        }
    }
    assert_eq!(after[&old_host], before[&old_host]);
    assert_progress(before[&source], after[&source], Direction::Down);
    assert!(after[&source].y + after[&source].h > before[&new_host].y);
}

#[test]
fn committed_resize_survives_ordinary_moves() {
    let mut layout = populated(6, AREA);
    let original = frames(&layout, AREA);
    let bands = rows(&original);
    let moving = bands[0][0];
    let host = bands[0][1];
    assert!(
        layout
            .resize(moving, Direction::Right, false, AREA)
            .is_some()
    );
    let custom = frames(&layout, AREA);
    assert!(custom[&moving].w > original[&moving].w);
    // Only the second band changes from three cells to two. The surviving
    // three-cell band still carries its manually positioned divider.
    let mut count_changed = layout.clone();
    count_changed.remove(bands[1][2]);
    let five = frames(&count_changed, AREA);
    assert_eq!(
        [five[&moving], five[&host]],
        [custom[&moving], custom[&host]]
    );

    assert!(layout.move_window(bands[1][0], Direction::Right, AREA));
    assert_eq!(frames(&layout, AREA)[&moving], custom[&moving]);
    assert!(layout.move_window(moving, Direction::Right, AREA));
    assert!(layout.move_window(moving, Direction::Right, AREA));
    let after = frames(&layout, AREA);
    assert_eq!(
        [after[&moving], after[&host]],
        [custom[&host], custom[&moving]]
    );
}

#[test]
fn count_changes_repack_all_cells_and_start_the_next_move_at_the_new_layout() {
    for added in [false, true] {
        let mut layout = populated(if added { 5 } else { 6 }, AREA);
        let initial = frames(&layout, AREA);
        let bands = rows(&initial);
        assert!(layout.move_window(bands[0][0], Direction::Right, AREA));
        if added {
            assert!(layout.insert(6, AREA));
        } else {
            layout.remove(6);
        }
        assert_packed(&layout, if added { 6 } else { 5 }, AREA);
        let repacked = frames(&layout, AREA);
        let band = &rows(&repacked)[0];
        assert!(layout.move_window(band[0], Direction::Right, AREA));
        assert_horizontal_share(&repacked, &frames(&layout, AREA), band[0], band[1]);
    }
}

#[test]
fn physical_minima_limit_admission_and_manual_resizing_without_losing_windows() {
    let area = Rect {
        w: 2 * MIN_WIDTH + GAP,
        h: MIN_HEIGHT,
        ..AREA
    };
    let mut layout = populated(2, area);
    let original = frames(&layout, area);
    let row = &rows(&original)[0];
    assert!(!layout.insert(row[1], area));
    assert!(!layout.insert(3, area));
    assert_eq!(layout.resize(row[0], Direction::Right, false, area), None);
    assert!(!layout.move_window(row[0], Direction::Left, area));
    assert!(!layout.move_window(999, Direction::Right, area));
    layout.remove(999);
    assert_eq!(frames(&layout, area), original);
    assert_packed(&layout, 2, area);
}

#[test]
fn application_minimum_heights_are_respected_on_the_realistic_four_window_layout() {
    // Equal bands here would be 564 px, less than the tallest application's minimum.
    let area = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1136,
    };
    let minimums = BTreeMap::from([(1, 600), (2, 408), (3, 513), (4, 243)]);
    let mut layout = Layout::default();
    for (&id, &height) in &minimums {
        assert!(layout.insert_with_minimum(id, area, (MIN_WIDTH, height)));
    }
    let check = |layout: &Layout| {
        let actual = frames(layout, area);
        assert_eq!(actual.len(), minimums.len());
        for (&id, a) in &actual {
            assert!(a.h >= minimums[&id], "window {id} below its minimum: {a:?}");
            assert!(a.x >= 0 && a.y >= 0 && a.x + a.w <= area.w && a.y + a.h <= area.h);
            for (&other, b) in actual.range((id + 1)..) {
                assert!(
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y,
                    "windows {id} and {other} overlap: {a:?} and {b:?}",
                );
            }
        }
    };
    assert_packed(&layout, 4, area);
    check(&layout);
    let original = frames(&layout, area);
    let bands = rows(&original);
    let moving = bands[0][0];
    let host = bands[1][0];
    assert_eq!(moving, 1);
    let mut moved = layout.clone();
    assert!(moved.move_window(moving, Direction::Down, area));
    stationary(&original, &frames(&moved, area), moving, host);
    check(&moved);
    assert!(moved.move_window(moving, Direction::Down, area));
    check(&moved);
    let exchanged = frames(&moved, area);
    assert_eq!(exchanged[&moving].h, minimums[&moving]);
    assert_eq!(exchanged[&host].h, original[&host].h);
    assert_eq!(
        exchanged[&moving].y,
        exchanged[&host].y + exchanged[&host].h + GAP
    );
    assert_eq!(exchanged[&bands[0][1]].h, exchanged[&host].h);
    assert_eq!(exchanged[&bands[1][1]].h, exchanged[&moving].h);
    for dir in [Direction::Up, Direction::Down] {
        for _ in 0..30 {
            layout.resize(moving, dir, false, area);
            check(&layout);
        }
        assert!(layout.resize(moving, dir, false, area).is_none());
    }
}

const DESKTOP: Rect = Rect {
    x: 8,
    y: 8,
    w: 1904,
    h: 1136,
};
const TALL: Id = 1;
const BROWSER: Id = 2;
const FOLDER: Id = 3;
const TELEGRAM: Id = 4;

// IDs 1 and 2 are the wide windows. IDs 3вЂ“6 are their four narrow neighbors.
fn explorer_junction(direction: Direction, source_width: i32, host_min: i32) -> (Layout, Rect) {
    let orient = |mut r: Rect| {
        if matches!(direction, Direction::Down | Direction::Right) {
            r.y = DESKTOP.y + DESKTOP.h - (r.y - DESKTOP.y) - r.h;
        }
        if direction.axis() == super::Axis::X {
            transpose(r)
        } else {
            r
        }
    };
    let area = orient(DESKTOP);
    let (top, bottom) = super::Axis::Y.split_at(DESKTOP, 564, GAP);
    let (top_left, host) = super::Axis::X.split_at(top, 948, GAP);
    let (left, right) = super::Axis::X.split_at(bottom, top_left.w, GAP);
    let (third, fourth) = super::Axis::X.split_at(left, 470, GAP);
    let (fifth, sixth) = super::Axis::X.split_at(right, source_width, GAP);
    let cells = [top_left, host, third, fourth, fifth, sixth].map(orient);
    let mut layout = Layout::default();
    for (i, min_width) in [160, host_min, 160, 160, 430, 160].into_iter().enumerate() {
        let min = if direction.axis() == super::Axis::X {
            (236, min_width)
        } else {
            (min_width, 236)
        };
        assert!(layout.insert_with_minimum(i as Id + 1, area, min));
    }
    assert!(layout.rebuild_cells(area, &cells, vec![]));
    (layout, area)
}

#[test]
fn narrow_window_enters_its_aligned_half_before_taking_the_wide_neighbors_slot() {
    for dir in [
        Direction::Up,
        Direction::Down,
        Direction::Left,
        Direction::Right,
    ] {
        for (source, donor) in [(5, 6), (6, 5)] {
            let (mut layout, area) = explorer_junction(dir, 470, 160);
            let before = frames(&layout, area);
            let host = before[&2];
            let old = before[&source];
            let adjacent = before[&donor];
            let across = dir.axis().perpendicular();
            let in_host = |r: Rect| match dir.axis() {
                super::Axis::Y => Rect {
                    x: r.x,
                    w: r.w,
                    ..host
                },
                super::Axis::X => Rect {
                    y: r.y,
                    h: r.h,
                    ..host
                },
            };
            let mut shared = before.clone();
            shared.insert(source, in_host(old));
            shared.insert(2, in_host(adjacent));
            shared.insert(donor, old.joined(adjacent, across, GAP).unwrap());
            assert_eq!(layout.neighbor(source, dir, area), Some(2));
            assert!(layout.move_window(source, dir, area));
            assert_eq!(frames(&layout, area), shared, "{dir:?} from tile {source}");
            assert_progress(old, shared[&source], dir);
            assert_tiled(&shared, area);

            assert!(layout.move_window(source, dir, area));
            let mut exchanged = before;
            exchanged.insert(source, host);
            exchanged.insert(2, old);
            assert_eq!(frames(&layout, area), exchanged);
        }
    }
}

#[test]
fn junction_split_can_move_its_upper_divider_to_respect_the_hosts_minimum() {
    let (mut layout, area) = explorer_junction(Direction::Up, 600, 480);
    let original = frames(&layout, area);
    assert!(layout.move_window(5, Direction::Up, area));
    let shared = frames(&layout, area);
    let (left, right) = super::Axis::X.split_at(original[&2], original[&2].w - GAP - 480, GAP);
    assert_eq!([shared[&5], shared[&2]], [left, right]);
    assert_eq!(
        shared[&6],
        original[&5]
            .joined(original[&6], super::Axis::X, GAP)
            .unwrap()
    );
    for id in [1, 3, 4] {
        assert_eq!(shared[&id], original[&id]);
    }
    assert!(layout.move_window(5, Direction::Up, area));
    let taken = frames(&layout, area);
    stationary(&original, &taken, 5, 2);
    assert_eq!([taken[&5], taken[&2]], [original[&2], original[&5]]);
}

#[test]
fn junction_with_too_little_room_for_both_minimum_widths_moves_directly() {
    let (mut layout, area) = explorer_junction(Direction::Up, 600, 555);
    let original = frames(&layout, area);
    assert!(430 + GAP + 555 > original[&2].w);
    assert!(original[&5].w >= 555);
    assert!(layout.move_window(5, Direction::Up, area));
    let taken = frames(&layout, area);
    stationary(&original, &taken, 2, 5);
    assert_eq!([taken[&5], taken[&2]], [original[&2], original[&5]]);
}

#[test]
fn adjacent_junctions_keep_their_independent_second_presses() {
    let (mut layout, area) = explorer_junction(Direction::Up, 470, 160);
    let original = frames(&layout, area);
    assert!(layout.move_window(5, Direction::Up, area));
    let right = frames(&layout, area);
    assert!(layout.move_window(3, Direction::Up, area));
    let both = frames(&layout, area);
    for id in [2, 5, 6] {
        assert_eq!(both[&id], right[&id]);
    }
    assert_eq!(
        both[&3].joined(both[&1], super::Axis::X, GAP),
        Some(original[&1])
    );
    assert!(layout.move_window(5, Direction::Up, area));
    let exchanged = frames(&layout, area);
    for id in [1, 3, 4] {
        assert_eq!(exchanged[&id], both[&id]);
    }
    assert_eq!(exchanged[&5], original[&2]);
    assert!(layout.move_window(3, Direction::Up, area));
    let final_frames = frames(&layout, area);
    for (id, slot) in [(1, 3), (2, 5), (3, 1), (4, 4), (5, 2), (6, 6)] {
        assert_eq!(final_frames[&id], original[&slot]);
    }
}

#[test]
fn junction_participants_use_their_displayed_tiles_for_subsequent_commands() {
    let (mut shared, area) = explorer_junction(Direction::Up, 470, 160);
    assert!(shared.move_window(5, Direction::Up, area));
    let shown = frames(&shared, area);
    assert_eq!(shared.neighbor(5, Direction::Right, area), Some(2));
    let mut turned = shared.clone();
    assert!(turned.move_window(5, Direction::Right, area));
    assert_horizontal_share(&shown, &frames(&turned, area), 5, 2);
    let mut from_filler = shared.clone();
    assert_eq!(from_filler.neighbor(6, Direction::Up, area), Some(5));
    assert!(from_filler.move_window(6, Direction::Up, area));
    let moved = frames(&from_filler, area);
    stationary(&shown, &moved, 6, 5);
    assert_progress(shown[&6], moved[&6], Direction::Up);
    assert_ne!(moved[&5], shown[&5]);

    for id in [2, 5, 6] {
        let mut opened = shared.clone();
        assert!(opened.insert_near(7, id, area, (160, 236)));
        let split = frames(&opened, area);
        assert!(
            [super::Axis::X, super::Axis::Y]
                .into_iter()
                .any(|axis| split[&7].joined(split[&id], axis, GAP) == Some(shown[&id]))
        );
        for (&other, &prev) in &shown {
            if other != id {
                assert_eq!(split[&other], prev);
            }
        }
        let mut closed = shared.clone();
        closed.remove(id);
        let rest = frames(&closed, area);
        assert!(
            rest.keys()
                .copied()
                .eq(shown.keys().copied().filter(|&other| other != id))
        );
        assert_tiled(&rest, area);
        for other in rest.keys() {
            for dir in [Direction::Up, Direction::Down] {
                let mut next = closed.clone();
                next.move_window(*other, dir, area);
                assert_tiled(&frames(&next, area), area);
            }
        }
    }
}

#[test]
fn unrelated_window_arrivals_and_departures_preserve_a_pending_junction() {
    let (mut shared, area) = explorer_junction(Direction::Up, 470, 160);
    let original = frames(&shared, area);
    assert!(shared.move_window(5, Direction::Up, area));
    let shown = frames(&shared, area);
    for open in [true, false] {
        let mut layout = shared.clone();
        if open {
            assert!(layout.insert_near(7, 3, area, (160, 236)));
        } else {
            layout.remove(3);
        }
        let before_takeover = frames(&layout, area);
        for id in [2, 5, 6] {
            assert_eq!(before_takeover[&id], shown[&id]);
        }
        assert!(layout.move_window(5, Direction::Up, area));
        let taken = frames(&layout, area);
        assert_eq!(
            [taken[&2], taken[&5], taken[&6]],
            [original[&5], original[&2], original[&6]]
        );
        for (&id, tile) in &before_takeover {
            if ![2, 5, 6].contains(&id) {
                assert_eq!(taken[&id], *tile);
            }
        }
    }
}

const APP_MINIMA: [(Id, (i32, i32)); 4] = [
    (BROWSER, (502, 401)),
    (TALL, (480, 600)),
    (FOLDER, (160, 236)),
    (TELEGRAM, (382, 506)),
];

fn check_desktop(layout: &Layout) -> Frames {
    let tiles = frames(layout, DESKTOP);
    for (id, (w, h)) in APP_MINIMA {
        assert!(tiles[&id].w >= w && tiles[&id].h >= h, "{tiles:?}");
    }
    let cells: Vec<_> = tiles.values().copied().collect();
    assert!(
        super::bsp::Bsp::from_rects(DESKTOP, &cells, GAP).is_some(),
        "{tiles:?}"
    );
    tiles
}

fn assert_above(upper: Rect, lower: Rect) {
    assert_eq!(upper.y + upper.h + GAP, lower.y);
    assert!(upper.x < lower.x + lower.w && lower.x < upper.x + upper.w);
}

// A Down preview in the left half of a 2Г—2 grid produces the same visible
// desktop as an already committed pair of tall columns.
fn chat_and_telegram_columns(pending: bool) -> Layout {
    let mut layout = Layout::default();
    for (id, minimum) in [
        (TALL, (480, 600)),
        (FOLDER, (160, 236)),
        (TELEGRAM, (382, 506)),
        (BROWSER, (502, 401)),
    ] {
        assert!(layout.insert_with_minimum(id, DESKTOP, minimum));
    }
    assert!(layout.move_window(TALL, Direction::Down, DESKTOP));
    let preview = check_desktop(&layout);
    if !pending {
        layout.freeze_pair(0, DESKTOP);
        assert_eq!(check_desktop(&layout), preview);
    }
    assert_eq!(preview[&TALL], Rect { w: 480, ..DESKTOP });
    assert_eq!(
        preview[&TELEGRAM],
        Rect {
            x: 496,
            w: 460,
            ..DESKTOP
        }
    );
    layout
}

#[test]
fn sideways_move_uses_the_visible_portrait_pair_then_takes_the_neighbors_column() {
    for pending in [true, false] {
        for (mover, direction) in [(TALL, Direction::Right), (TELEGRAM, Direction::Left)] {
            let mut layout = chat_and_telegram_columns(pending);
            let columns = check_desktop(&layout);
            assert!(layout.move_window(mover, direction, DESKTOP));
            let stacked = check_desktop(&layout);
            stationary(&columns, &stacked, TALL, TELEGRAM);
            let top = stacked[&TELEGRAM];
            let bottom = stacked[&TALL];
            assert_above(top, bottom);
            assert_eq!((top.x, top.w, top.y), (8, 948, 8));
            assert_eq!((bottom.x, bottom.w, bottom.y + bottom.h), (8, 948, 1144));
            assert_eq!((top.h, bottom.y, bottom.h), (528, 544, 600));

            assert!(layout.move_window(mover, direction, DESKTOP));
            let taken = check_desktop(&layout);
            stationary(&columns, &taken, TALL, TELEGRAM);
            // ChatGPT needs 480px in Telegram's former 460px slot. Only the
            // divider between these two columns needs to move.
            assert_eq!(taken[&TELEGRAM], Rect { w: 460, ..DESKTOP });
            assert_eq!(
                taken[&TALL],
                Rect {
                    x: 476,
                    w: 480,
                    ..DESKTOP
                }
            );
        }
    }
}

#[test]
fn resizing_a_portrait_move_uses_the_horizontal_seam_for_either_occupant() {
    let mut preview = chat_and_telegram_columns(false);
    assert!(preview.move_window(TALL, Direction::Right, DESKTOP));
    let before = check_desktop(&preview);
    for (active, seam) in [(TELEGRAM, Direction::Down), (TALL, Direction::Up)] {
        let mut layout = preview.clone();
        assert_eq!(
            layout.resize(active, Direction::Up, false, DESKTOP),
            Some(seam)
        );
        let raised = check_desktop(&layout);
        stationary(&before, &raised, TALL, TELEGRAM);
        assert!(raised[&TELEGRAM].h < before[&TELEGRAM].h);
        assert!(raised[&TALL].h > before[&TALL].h);
        assert_eq!(raised[&TALL].w, before[&TALL].w);
        assert_above(raised[&TELEGRAM], raised[&TALL]);
        if active == TELEGRAM {
            layout = preview.clone();
        }
        let previous = check_desktop(&layout);
        assert_eq!(layout.resize(active, seam, true, DESKTOP), Some(seam));
        let shrunk = check_desktop(&layout);
        assert!(shrunk[&active].h < previous[&active].h);
        stationary(&previous, &shrunk, TALL, TELEGRAM);
        assert_above(shrunk[&TELEGRAM], shrunk[&TALL]);
    }
}

#[test]
fn a_pair_that_cannot_stack_moves_directly_to_the_neighbors_slot() {
    let area = Rect { w: 948, ..DESKTOP };
    let mut layout = Layout::default();
    assert!(layout.insert_with_minimum(1, area, (200, 600)));
    assert!(layout.insert_near(2, 1, area, (200, 600)));
    let columns = frames(&layout, area);
    assert!(layout.move_window(1, Direction::Right, area));
    let taken = frames(&layout, area);
    assert_eq!([taken[&1], taken[&2]], [columns[&2], columns[&1]]);
    assert!(layout.shared.is_empty());
    assert!(!layout.move_window(1, Direction::Right, area));
    assert_eq!(frames(&layout, area), taken);
}

#[test]
fn narrow_stacked_windows_move_up_or_down_immediately() {
    let area = Rect {
        w: 400,
        h: 1024,
        ..AREA
    };
    for (moving, direction) in [(1, Direction::Down), (2, Direction::Up)] {
        let mut layout = Layout::default();
        for id in 1..=2 {
            assert!(layout.insert_with_minimum(id, area, (260, 200)));
        }
        let before = frames(&layout, area);
        assert_eq!(
            before[&1].joined(before[&2], super::Axis::Y, GAP),
            Some(area)
        );
        assert!(layout.move_window(moving, direction, area));
        let taken = frames(&layout, area);
        assert_eq!([taken[&2], taken[&1]], [before[&1], before[&2]]);
        assert!(layout.shared.is_empty());
    }
}

#[test]
fn changing_windows_and_directions_always_makes_visible_progress_or_leaves_all_windows_in_place() {
    let mut seed = 391u32;
    let mut next = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed >> 8
    };
    let directions = [
        Direction::Up,
        Direction::Left,
        Direction::Right,
        Direction::Down,
    ];
    let mut successes = [0; 4];
    for count in [2, 5, 12, 20] {
        let mut layout = populated(count, AREA);
        for _ in 0..100 {
            let source = Id::try_from(next()).unwrap() % count + 1;
            let d = next() as usize % directions.len();
            let dir = directions[d];
            for _ in 0..2 {
                let before = frames(&layout, AREA);
                // A narrow tile which has already entered a larger host
                // confirms by taking precisely that host's entire old slot.
                let takeover = layout.shared.iter().find_map(|p| {
                    if layout.occupants[p.source].id != source || p.direction != dir {
                        return None;
                    }
                    let original = layout.cell_rects(AREA);
                    let host = original[p.target];
                    (host.contains(before[&source]) && host != before[&source]).then_some((
                        layout.occupants[p.target].id,
                        host,
                        original[p.source],
                    ))
                });
                let moved = layout.move_window(source, dir, AREA);
                let after = frames(&layout, AREA);
                if !moved {
                    assert_eq!(after, before);
                    break;
                }
                successes[d] += 1;
                if let Some((neighbor, host, original)) = takeover {
                    assert_eq!([after[&source], after[&neighbor]], [host, original]);
                } else {
                    assert_progress(before[&source], after[&source], dir);
                }
                assert!(before.keys().eq(after.keys()));
                assert_tiled(&after, AREA);
            }
        }
    }
    assert!(successes.into_iter().all(|n| n > 0));
}

#[test]
fn intersecting_corner_moves_in_five_tiles_keep_next_commands_usable() {
    let area = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1136,
    };
    let minima = [(500, 401), (490, 230), (260, 250), (210, 236), (307, 231)];
    let mut layout = Layout::default();
    for (index, min) in minima.into_iter().enumerate() {
        assert!(layout.insert_with_minimum(index as Id + 1, area, min));
    }
    assert!(layout.move_window(2, Direction::Up, area));
    let initial = frames(&layout, area);
    assert_eq!(layout.neighbor(3, Direction::Down, area), Some(5));
    assert!(layout.move_window(3, Direction::Down, area));
    let before = frames(&layout, area);
    stationary(&initial, &before, 3, 5);
    assert_progress(initial[&3], before[&3], Direction::Down);
    // A shared corner here can create a pinwheel with no straight divider
    // spanning the workspace. Whether the previous key shared or took over,
    // window 5 must still respond to its next arrow.
    let neighbor = layout.neighbor(5, Direction::Right, area);
    assert!(layout.move_window(5, Direction::Right, area));
    let moved = frames(&layout, area);
    assert_progress(before[&5], moved[&5], Direction::Right);
    if let Some(neighbor) = neighbor {
        assert_eq!(neighbor, 3);
        stationary(&before, &moved, 5, 3);
    } else {
        stationary(&before, &moved, 5, 2);
    }
    // A different window uses the neighbor now visible on screen.
    let mut redirected = layout.clone();
    let dir = if redirected.neighbor(3, Direction::Down, area).is_some() {
        Direction::Down
    } else {
        Direction::Left
    };
    assert_eq!(redirected.neighbor(3, dir, area), Some(4));
    assert!(redirected.move_window(3, dir, area));
    let taken = frames(&redirected, area);
    stationary(&moved, &taken, 3, 4);
    assert_progress(moved[&3], taken[&3], dir);
    assert_ne!(moved[&4], taken[&4]);
    // Minimizing a bystander or either moved window releases its tile.
    for id in [2, 3, 5] {
        let mut remaining = layout.clone();
        remaining.remove(id);
        let actual = frames(&remaining, area);
        assert_eq!(actual.len(), 4);
        assert!(!actual.contains_key(&id));
        assert_tiled(&actual, area);
        for (&other, rect) in &actual {
            let (w, h) = minima[other as usize - 1];
            assert!(rect.w >= w && rect.h >= h);
        }
    }
}

#[test]
fn a_window_can_enter_a_visible_corner_after_edge_moves_elsewhere() {
    use Direction::{Down, Left, Right, Up};
    let area = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1136,
    };
    let mut layout = populated(12, area);
    for (id, dir) in [
        (2, Down),
        (3, Left),
        (6, Left),
        (6, Right),
        (12, Right),
        (3, Up),
    ] {
        layout.move_window(id, dir, area);
    }
    let before = frames(&layout, area);
    let neighbor = layout
        .neighbor(11, Up, area)
        .expect("window directly above 11");
    assert!(layout.move_window(11, Up, area));
    let after = frames(&layout, area);
    stationary(&before, &after, 11, neighbor);
    assert_progress(before[&11], after[&11], Up);
    assert_ne!(after[&neighbor], before[&neighbor]);
}

#[test]
fn moving_left_from_chat_in_three_columns_releases_its_old_position_on_the_first_press() {
    let (chat, browser, folder) = (TALL, BROWSER, FOLDER);
    for browser_min_height in [401, 540] {
        let mut layout = Layout::default();
        for (id, min) in [
            (browser, (502, browser_min_height)),
            (chat, (480, 600)),
            (folder, (160, 236)),
        ] {
            assert!(layout.insert_with_minimum(id, DESKTOP, min));
        }
        let columns = [(8, 683), (699, 502), (1209, 703)].map(|(x, w)| Rect { x, w, ..DESKTOP });
        assert!(layout.rebuild_cells(DESKTOP, &columns, vec![]));
        let before = frames(&layout, DESKTOP);
        assert_eq!([before[&browser], before[&chat], before[&folder]], columns);
        assert!(layout.move_window(chat, Direction::Left, DESKTOP));
        let first = frames(&layout, DESKTOP);
        assert_progress(before[&chat], first[&chat], Direction::Left);
        assert_eq!(first[&folder], before[&folder]);
        if browser_min_height + GAP + 600 <= DESKTOP.h {
            assert_horizontal_share(&before, &first, chat, browser);
            assert!(layout.move_window(chat, Direction::Left, DESKTOP));
        }
        let taken = frames(&layout, DESKTOP);
        assert_eq!([taken[&chat], taken[&browser], taken[&folder]], columns);
        assert!(layout.shared.is_empty());
    }
}

#[test]
fn down_at_the_edge_puts_the_folder_below_telegram_inside_the_same_column() {
    let mut layout = Layout::default();
    assert!(layout.insert_with_minimum(BROWSER, DESKTOP, APP_MINIMA[0].1));
    assert!(layout.insert_near(TALL, BROWSER, DESKTOP, APP_MINIMA[1].1));
    assert!(layout.insert_near(FOLDER, BROWSER, DESKTOP, APP_MINIMA[2].1));
    assert!(layout.insert_near(TELEGRAM, FOLDER, DESKTOP, APP_MINIMA[3].1));
    // Reproduce the two user-positioned dividers from the desktop capture.
    layout.bsp.0[0].part = 969;
    layout.bsp.0[0].whole = DESKTOP.w - GAP;
    layout.bsp.0[1].part = 622;
    layout.bsp.0[1].whole = DESKTOP.h - GAP;
    let before = check_desktop(&layout);
    let measured = [BROWSER, FOLDER, TELEGRAM, TALL].map(|id| {
        let Rect { x, y, w, h } = before[&id];
        (x, y, w, h)
    });
    assert_eq!(
        measured,
        [
            (8, 8, 969, 622),
            (8, 638, 481, 506),
            (497, 638, 480, 506),
            (985, 8, 927, 1136),
        ]
    );

    assert!(layout.move_window(FOLDER, Direction::Down, DESKTOP));
    let after = check_desktop(&layout);
    assert_eq!(
        after[&TALL], before[&TALL],
        "the right column has no part in this move"
    );
    assert_above(after[&TELEGRAM], after[&FOLDER]);
    assert_eq!(after[&FOLDER].y + after[&FOLDER].h, DESKTOP.y + DESKTOP.h);
    assert!(layout.insert_near(5, FOLDER, DESKTOP, (MIN_WIDTH, MIN_HEIGHT)));
    let inserted = check_desktop(&layout);
    assert_eq!(inserted.len(), 5);
    layout.remove(5);
    assert_eq!(check_desktop(&layout), after);
}

#[test]
fn down_at_the_edge_works_in_the_grid_and_after_a_horizontal_takeover() {
    let mut grid = Layout::default();
    for (id, minimum) in APP_MINIMA {
        assert!(grid.insert_with_minimum(id, DESKTOP, minimum));
    }
    let original = check_desktop(&grid);
    assert_eq!(
        original[&FOLDER],
        Rect {
            x: 8,
            y: 616,
            w: 948,
            h: 528
        }
    );
    for moved_left in [false, true] {
        let mut layout = grid.clone();
        if moved_left {
            assert!(layout.move_window(TELEGRAM, Direction::Left, DESKTOP));
            let visible = check_desktop(&layout);
            assert_eq!(visible[&FOLDER], original[&TELEGRAM]);
            assert_eq!(visible[&TELEGRAM], original[&FOLDER]);
        }
        assert!(layout.move_window(FOLDER, Direction::Down, DESKTOP));
        let moved = check_desktop(&layout);
        assert_above(moved[&TELEGRAM], moved[&FOLDER]);
        assert_eq!(moved[&FOLDER].y + moved[&FOLDER].h, DESKTOP.y + DESKTOP.h);
    }
}

#[test]
fn two_side_by_side_windows_can_stack_at_either_edge_and_vice_versa() {
    for dir in [
        Direction::Down,
        Direction::Up,
        Direction::Left,
        Direction::Right,
    ] {
        let along_y = matches!(dir, Direction::Down | Direction::Up);
        let area = if along_y { DESKTOP } else { transpose(DESKTOP) };
        let mut layout = populated(2, area);
        let before = frames(&layout, area);
        assert_eq!(before[&1].y == before[&2].y, along_y);
        assert!(layout.move_window(1, dir, area));
        let after = frames(&layout, area);
        let a = after[&1];
        let b = after[&2];
        let (a, b) = if along_y {
            (a, b)
        } else {
            (transpose(a), transpose(b))
        };
        if matches!(dir, Direction::Down | Direction::Right) {
            assert_above(b, a);
        } else {
            assert_above(a, b);
        }
        assert_eq!(a.x, b.x);
        assert_eq!(a.w, b.w);
        assert!(!layout.move_window(1, dir, area));
        assert_eq!(frames(&layout, area), after);
    }
}

#[test]
fn an_edge_move_that_cannot_fit_preserves_the_pending_move() {
    let area = Rect {
        w: 1000,
        h: 510,
        ..DESKTOP
    };
    let mut layout = Layout::default();
    for id in 1..=2 {
        assert!(layout.insert_with_minimum(id, area, (200, 240)));
    }
    let original = frames(&layout, area);
    assert!(layout.move_window(2, Direction::Left, area));
    let preview = frames(&layout, area);
    assert_horizontal_share(&original, &preview, 2, 1);
    assert!(!layout.move_window(1, Direction::Down, area));
    assert_eq!(frames(&layout, area), preview);
    assert!(layout.move_window(2, Direction::Left, area));
    assert_eq!(frames(&layout, area)[&2], original[&1]);
}

#[test]
fn rotating_a_lower_pair_keeps_the_upper_pair_pending() {
    let mut layout = populated(6, AREA);
    let original = frames(&layout, AREA);
    let bands = rows(&original);
    let (a, b) = (bands[0][0], bands[0][1]);
    assert!(layout.move_window(a, Direction::Right, AREA));
    let preview = frames(&layout, AREA);
    let moving = bands[1][0];
    assert!(layout.move_window(moving, Direction::Down, AREA));
    let rotated = frames(&layout, AREA);
    assert_above(rotated[&bands[1][1]], rotated[&moving]);
    for id in &bands[0] {
        assert_eq!(rotated[id], preview[id]);
    }
    assert!(layout.move_window(a, Direction::Right, AREA));
    let exchanged = frames(&layout, AREA);
    assert_eq!([exchanged[&a], exchanged[&b]], [original[&b], original[&a]]);
    for id in &bands[1] {
        assert_eq!(exchanged[id], rotated[id]);
    }
}

fn folder_beside_tall_window(browser_min_height: i32) -> Layout {
    let mut layout = Layout::default();
    assert!(layout.insert_with_minimum(BROWSER, DESKTOP, (502, browser_min_height)));
    assert!(layout.insert_near(TALL, BROWSER, DESKTOP, (480, 600)));
    assert!(layout.insert_near(FOLDER, TALL, DESKTOP, (147, 236)));
    // The tall window establishes a 600 / 528 split on the right. Move it
    // to the left column, keeping that same divider behind it.
    let left = frames(&layout, DESKTOP)[&BROWSER];
    assert!(layout.move_window(TALL, Direction::Left, DESKTOP));
    if frames(&layout, DESKTOP)[&TALL] != left {
        assert!(layout.move_window(TALL, Direction::Left, DESKTOP));
    }
    let original = frames(&layout, DESKTOP);
    assert_eq!(original[&TALL], Rect { w: 948, ..DESKTOP });
    assert_eq!(
        original[&BROWSER],
        Rect {
            x: 964,
            y: 8,
            w: 948,
            h: 600
        }
    );
    assert_eq!(
        original[&FOLDER],
        Rect {
            x: 964,
            y: 616,
            w: 948,
            h: 528
        }
    );
    layout
}

#[test]
fn folder_enters_the_lower_part_of_the_left_host_before_a_minimum_constrained_takeover() {
    let mut layout = folder_beside_tall_window(401);
    let original = frames(&layout, DESKTOP);
    assert!(layout.move_window(FOLDER, Direction::Left, DESKTOP));
    let split = frames(&layout, DESKTOP);
    assert_progress(original[&FOLDER], split[&FOLDER], Direction::Left);
    assert_eq!(
        split[&FOLDER],
        Rect {
            x: original[&TALL].x,
            ..original[&FOLDER]
        }
    );
    assert_eq!(
        split[&FOLDER].joined(split[&TALL], super::Axis::Y, GAP),
        Some(original[&TALL])
    );
    assert_eq!(
        split[&BROWSER],
        original[&BROWSER]
            .joined(original[&FOLDER], super::Axis::Y, GAP)
            .unwrap()
    );
    assert!(layout.move_window(FOLDER, Direction::Left, DESKTOP));
    let exchanged = frames(&layout, DESKTOP);
    assert_progress(original[&FOLDER], exchanged[&FOLDER], Direction::Left);
    assert_eq!(exchanged[&FOLDER], original[&TALL]);
    assert_eq!(
        exchanged[&TALL],
        Rect {
            y: 544,
            h: 600,
            ..original[&FOLDER]
        }
    );
    assert_eq!(
        exchanged[&BROWSER],
        Rect {
            h: 528,
            ..original[&BROWSER]
        }
    );
    assert!(!layout.move_window(FOLDER, Direction::Left, DESKTOP));
    assert_eq!(frames(&layout, DESKTOP), exchanged);
}

#[test]
fn a_valid_junction_split_survives_a_takeover_that_cannot_fit_the_new_owner() {
    let mut layout = folder_beside_tall_window(580);
    let before = frames(&layout, DESKTOP);
    assert!(layout.move_window(FOLDER, Direction::Left, DESKTOP));
    let shown = frames(&layout, DESKTOP);
    assert_progress(before[&FOLDER], shown[&FOLDER], Direction::Left);
    assert_eq!(
        shown[&TALL].joined(shown[&FOLDER], super::Axis::Y, GAP),
        Some(before[&TALL])
    );
    assert!(!layout.move_window(FOLDER, Direction::Left, DESKTOP));
    assert_eq!(frames(&layout, DESKTOP), shown);

    layout.remove(BROWSER);
    let remaining = frames(&layout, DESKTOP);
    assert_eq!(remaining.len(), 2);
    assert!(remaining[&TALL].h >= 600);
    assert!(
        [super::Axis::X, super::Axis::Y]
            .into_iter()
            .any(|axis| remaining[&TALL].joined(remaining[&FOLDER], axis, GAP) == Some(DESKTOP))
    );
}

#[test]
fn changing_commands_midway_through_a_constrained_move_keeps_every_window_valid() {
    let mut pending = chat_and_telegram_columns(false);
    assert!(pending.move_window(TALL, Direction::Right, DESKTOP));
    let preview = frames(&pending, DESKTOP);
    let check = |layout: &Layout| {
        let placed = frames(layout, DESKTOP);
        assert!(placed.keys().eq(preview.keys()));
        for (&id, rect) in &placed {
            let (width, height) = match id {
                TALL => (480, 600),
                BROWSER => (502, 401),
                FOLDER => (MIN_WIDTH, 236),
                TELEGRAM => (382, 506),
                _ => unreachable!(),
            };
            assert!(
                rect.w >= width && rect.h >= height,
                "window {id} shrank too far: {rect:?}"
            );
        }
        // The four rectangles must fill the workspace with only normal gutters.
        let cells: Vec<_> = placed.into_values().collect();
        assert!(
            super::bsp::Bsp::from_rects(DESKTOP, &cells, GAP).is_some(),
            "{cells:?}"
        );
    };
    for id in [TALL, BROWSER, FOLDER, TELEGRAM] {
        for dir in [
            Direction::Up,
            Direction::Left,
            Direction::Right,
            Direction::Down,
        ] {
            let mut moved = pending.clone();
            if moved.move_window(id, dir, DESKTOP) {
                assert_progress(preview[&id], frames(&moved, DESKTOP)[&id], dir);
            } else {
                assert_eq!(frames(&moved, DESKTOP), preview);
            }
            check(&moved);
            for shrink in [false, true] {
                let mut resized = pending.clone();
                if resized.resize(id, dir, shrink, DESKTOP).is_none() {
                    assert_eq!(frames(&resized, DESKTOP), preview);
                }
                check(&resized);
            }
        }
    }
}
