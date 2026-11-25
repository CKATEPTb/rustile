use super::*;
use Direction::{Down, Left, Right, Up};

const AREA: Rect = Rect {
    x: -1471,
    y: 37,
    w: 1751,
    h: 1063,
};
const GAP_PX: i32 = 11;

fn grid(count: Id) -> Layout {
    let mut layout = Layout::with_gap(GAP_PX);
    for id in 1..=count {
        assert!(layout.insert(id, AREA));
    }
    layout
}

fn frame(layout: &Layout, id: Id) -> Rect {
    layout
        .placements(AREA)
        .into_iter()
        .find(|p| p.0 == id)
        .unwrap()
        .1
}

fn check_step(
    layout: &mut Layout,
    id: Id,
    arrow: Direction,
    shift: bool,
    edge: Direction,
    stops: &mut Option<ResizeStops>,
) {
    let before = frame(layout, id);
    assert_eq!(layout.resize(id, arrow, shift, AREA, stops), Some(edge));
    let after = frame(layout, id);
    for side in [Left, Right, Up, Down] {
        let delta = side.coordinate(after) - side.coordinate(before);
        let expected = if side != edge {
            0
        } else if arrow.towards_start() != shift {
            -1
        } else {
            1
        };
        assert_eq!(
            delta.signum(),
            expected,
            "edge {side:?} on {arrow:?}, shift={shift}"
        );
    }
}

#[test]
fn shift_reverses_the_same_visible_edge_in_every_direction() {
    let layout = grid(9);
    for arrow in [Left, Right, Up, Down] {
        let mut layout = layout.clone();
        let target = i32::try_from(arrow.coordinate(frame(&layout, 5))).unwrap() + 13;
        assert!(layout.resize_edge_to(5, arrow, target, AREA));
        assert_eq!(arrow.coordinate(frame(&layout, 5)), i64::from(target));
        let mut stops = None;
        let initial = layout.placements(AREA);
        check_step(&mut layout, 5, arrow, false, arrow, &mut stops);
        let first_step = layout.placements(AREA);
        check_step(&mut layout, 5, arrow, false, arrow, &mut stops);
        check_step(&mut layout, 5, arrow, true, arrow, &mut stops);
        assert_eq!(layout.placements(AREA), first_step);
        check_step(&mut layout, 5, arrow, true, arrow, &mut stops);
        assert_eq!(layout.placements(AREA), initial);
    }
}

#[test]
fn outward_arrows_compress_against_the_work_area_and_shift_expands_back() {
    let layout = grid(9);
    for (id, outer, inner) in [
        (4, Left, Right),
        (6, Right, Left),
        (2, Up, Down),
        (8, Down, Up),
    ] {
        let mut layout = layout.clone();
        let mut stops = None;
        let initial = layout.placements(AREA);
        assert_eq!(outer.coordinate(frame(&layout, id)), outer.coordinate(AREA));
        check_step(&mut layout, id, outer, false, inner, &mut stops);
        check_step(&mut layout, id, outer, true, inner, &mut stops);
        assert_eq!(layout.placements(AREA), initial);
        check_step(&mut layout, id, inner, false, inner, &mut stops);
        check_step(&mut layout, id, inner, true, inner, &mut stops);
        assert_eq!(layout.placements(AREA), initial);
    }
}

#[test]
fn both_commands_leave_a_full_span_alone() {
    let mut layout = grid(1);
    let before = layout.placements(AREA);
    let mut stops = None;
    for arrow in [Left, Right, Up, Down] {
        for shift in [false, true] {
            assert_eq!(layout.resize(1, arrow, shift, AREA, &mut stops), None);
            assert_eq!(layout.placements(AREA), before);
        }
    }
}

#[test]
fn reversing_a_clamped_step_restores_the_manually_sized_divider() {
    let mut layout = Layout::with_gap(GAP_PX);
    for id in 1..=9 {
        let width = if id == 4 { 371 } else { MIN_WIDTH };
        assert!(layout.insert_with_minimum(id, AREA, (width, MIN_HEIGHT)));
    }
    let target = frame(&layout, 4).x + 371 + GAP_PX + 7;
    assert!(layout.resize_edge_to(5, Left, target, AREA));
    assert_eq!(frame(&layout, 5).x, target);
    let manual = layout.placements(AREA);
    let mut stops = None;
    check_step(&mut layout, 5, Left, false, Left, &mut stops);
    let left = frame(&layout, 4);
    assert_eq!(left.w, 371);
    assert_eq!(left.x + left.w + GAP_PX, frame(&layout, 5).x);
    let before = layout.placements(AREA);
    assert_eq!(layout.resize(5, Left, false, AREA, &mut stops), None);
    assert_eq!(layout.placements(AREA), before);
    check_step(&mut layout, 5, Left, true, Left, &mut stops);
    assert_eq!(layout.placements(AREA), manual);
}

#[test]
fn a_nested_tile_uses_its_visible_edge_and_keeps_the_far_divider_still() {
    let mut layout = grid(1);
    for (id, anchor) in [(2, 1), (3, 2), (4, 2)] {
        assert!(layout.insert_near(id, anchor, AREA, (MIN_WIDTH, MIN_HEIGHT)));
    }
    let far = frame(&layout, 4);
    let mut stops = None;
    check_step(&mut layout, 2, Left, false, Left, &mut stops);
    assert_eq!(frame(&layout, 4), far);
    check_step(&mut layout, 2, Left, true, Left, &mut stops);
    assert_eq!(frame(&layout, 4), far);
    assert_eq!(
        Right.coordinate(frame(&layout, 1)) + i64::from(GAP_PX),
        Left.coordinate(frame(&layout, 2))
    );
}

#[test]
fn a_rotated_pair_can_resize_both_ways_at_the_displayed_divider_and_complete_its_move() {
    let mut layout = grid(2);
    let destination = frame(&layout, 2);
    assert!(layout.move_window(1, Right, AREA));
    assert_eq!(frame(&layout, 1).y + frame(&layout, 1).h, AREA.y + AREA.h);
    let mut stops = None;
    check_step(&mut layout, 1, Down, false, Up, &mut stops);
    check_step(&mut layout, 1, Down, true, Up, &mut stops);
    assert_eq!(
        Down.coordinate(frame(&layout, 2)) + i64::from(GAP_PX),
        Up.coordinate(frame(&layout, 1))
    );
    assert!(layout.move_window(1, Right, AREA));
    assert_eq!(frame(&layout, 1), destination);
}
