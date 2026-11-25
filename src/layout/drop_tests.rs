use super::*;
use std::collections::BTreeMap;

const AREA: Rect = Rect {
    x: -1911,
    y: 23,
    w: 1904,
    h: 1134,
};
const MIN: (i32, i32) = (MIN_WIDTH, MIN_HEIGHT);

fn grid(n: Id) -> Layout {
    let mut layout = Layout::default();
    for id in 1..=n {
        assert!(layout.insert(id, AREA));
    }
    layout
}

fn frames(layout: &Layout) -> BTreeMap<Id, Rect> {
    layout.placements(AREA).into_iter().collect()
}

#[test]
fn hit_testing_uses_the_middle_half_and_normalized_edges_of_the_visible_tile() {
    let layout = grid(2);
    let tiles = frames(&layout);
    let r = tiles[&2];
    for (fx, fy, zone) in [
        (50, 50, DropZone::Center),
        (26, 72, DropZone::Center),
        (7, 47, DropZone::Edge(Direction::Left)),
        (91, 48, DropZone::Edge(Direction::Right)),
        (48, 9, DropZone::Edge(Direction::Up)),
        (48, 99, DropZone::Edge(Direction::Down)),
        (18, 8, DropZone::Edge(Direction::Up)),
        (94, 81, DropZone::Edge(Direction::Right)),
    ] {
        let pointer = (r.x + r.w * fx / 100, r.y + r.h * fy / 100);
        assert_eq!(layout.drop_target(1, pointer, AREA), Some((2, zone)));
    }
    let left = tiles[&1];
    for p in [
        (left.x + 10, left.y + 10),
        (r.x - 1, r.y),
        (r.x + r.w, r.y),
        (r.x, r.y - 1),
    ] {
        assert!(layout.drop_target(1, p, AREA).is_none());
    }
    assert!(layout.drop_target(999, (r.x, r.y), AREA).is_none());
}

#[test]
fn center_exchanges_exactly_the_two_visible_rectangles_or_rejects_the_drop() {
    let mut layout = grid(5);
    assert!(layout.move_window(1, Direction::Down, AREA));
    let before = frames(&layout);
    let (next, r) = layout.plan_drop(1, 5, DropZone::Center, AREA).unwrap();
    assert_eq!(r, before[&5]);
    let mut expected = before.clone();
    expected.insert(1, before[&5]);
    expected.insert(5, before[&1]);
    assert_eq!(frames(&next), expected);
    assert_eq!(frames(&layout), before);
    assert!(layout.plan_drop(1, 1, DropZone::Center, AREA).is_none());
    assert!(layout.plan_drop(1, 99, DropZone::Center, AREA).is_none());
    let mut limited = Layout::default();
    assert!(limited.insert_with_minimum(1, AREA, (1150, 100)));
    assert!(limited.insert_with_minimum(2, AREA, MIN));
    let limited_before = frames(&limited);
    assert!(limited.plan_drop(1, 2, DropZone::Center, AREA).is_none());
    assert_eq!(frames(&limited), limited_before);
}

#[test]
fn edges_split_the_hovered_slot_and_reclaim_the_source_without_global_reflow() {
    let layout = grid(6);
    let original = frames(&layout);
    for edge in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        let (next, preview) = layout.plan_drop(1, 6, DropZone::Edge(edge), AREA).unwrap();
        let after = frames(&next);
        assert_eq!(after[&1], preview);
        let host = after[&6];
        assert_eq!(host.joined(preview, edge.axis(), GAP), Some(original[&6]));
        assert_eq!(
            edge.axis().bounds(preview).0 < edge.axis().bounds(host).0,
            edge.towards_start()
        );
        let affected = (2..=5).filter(|i| original[i] != after[i]).count();
        assert_eq!(
            affected, 1,
            "only one neighbor is needed to fill the vacancy"
        );
        assert_eq!(frames(&layout), original);
    }
}

#[test]
fn two_windows_can_rotate_stack_and_reverse_side_while_preserving_native_minima() {
    let mut l = Layout::default();
    assert!(l.insert_with_minimum(1, AREA, (800, 700)));
    assert!(l.insert_with_minimum(2, AREA, (900, 300)));
    let before = frames(&l);
    assert!(
        l.plan_drop(1, 2, DropZone::Edge(Direction::Left), AREA)
            .is_none()
    );
    let (stack, r) = l
        .plan_drop(1, 2, DropZone::Edge(Direction::Down), AREA)
        .unwrap();
    let host = frames(&stack)[&2];
    assert_eq!(r.joined(host, Axis::Y, GAP), Some(AREA));
    assert_eq!(r.h, 700);
    assert_eq!(r.y, host.y + host.h + GAP);
    let (flipped, r) = l
        .plan_drop(1, 2, DropZone::Edge(Direction::Right), AREA)
        .unwrap();
    assert_eq!(r, before[&2]);
    assert_eq!(frames(&flipped)[&2], before[&1]);
    let mut too_tall = Layout::default();
    for id in 1..=2 {
        assert!(too_tall.insert_with_minimum(id, AREA, (500, 650)));
    }
    assert!(
        too_tall
            .plan_drop(1, 2, DropZone::Edge(Direction::Up), AREA)
            .is_none()
    );
}

#[test]
fn dropping_onto_a_pending_pair_uses_visible_bounds_and_keeps_a_distant_second_step() {
    let mut l = grid(9);
    assert!(l.move_window(1, Direction::Right, AREA));
    assert!(l.move_window(7, Direction::Right, AREA));
    let old = frames(&l);
    let host = old[&1];
    assert_eq!(
        l.drop_target(3, (host.x + host.w / 2, host.y + host.h / 2), AREA),
        Some((1, DropZone::Center))
    );
    let (mut next, preview) = l
        .plan_drop(3, 1, DropZone::Edge(Direction::Right), AREA)
        .unwrap();
    let after = frames(&next);
    assert_eq!(after[&3], preview);
    assert_eq!(preview.joined(after[&1], Axis::X, GAP), Some(host));
    for i in [7, 8, 9] {
        assert_eq!(after[&i], old[&i]);
    }
    assert!(next.move_window(7, Direction::Right, AREA));
    assert_eq!(frames(&next)[&7], frames(&grid(9))[&8]);
}

#[test]
fn when_the_target_is_too_short_the_preview_includes_required_branch_growth() {
    let mut l = grid(9);
    assert!(l.move_window(1, Direction::Right, AREA));
    let before = frames(&l);
    assert!(before[&1].h < 2 * MIN_HEIGHT + GAP);
    let (next, preview) = l
        .plan_drop(3, 1, DropZone::Edge(Direction::Up), AREA)
        .unwrap();
    let after = frames(&next);
    let region = preview.joined(after[&1], Axis::Y, GAP).unwrap();
    assert_eq!(preview, after[&3]);
    assert_eq!(region.h, 2 * MIN_HEIGHT + GAP);
    assert_eq!(preview.y + preview.h + GAP, after[&1].y);
    assert_eq!(frames(&l), before);
}
