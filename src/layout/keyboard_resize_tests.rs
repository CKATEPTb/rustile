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

fn check_step(layout: &mut Layout, id: Id, arrow: Direction, shift: bool, edge: Direction) {
    let before = frame(layout, id);
    assert_eq!(layout.resize(id, arrow, shift, AREA), Some(edge));
    let after = frame(layout, id);
    for side in [Left, Right, Up, Down] {
        let delta = side.coordinate(after) - side.coordinate(before);
        let expected = if side != edge {
            0
        } else if arrow.towards_start() {
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
fn interior_arrows_expand_the_named_edge_and_shift_moves_the_opposite_edge() {
    let layout = grid(9);
    for (arrow, opposite) in [(Left, Right), (Right, Left), (Up, Down), (Down, Up)] {
        check_step(&mut layout.clone(), 5, arrow, false, arrow);
        check_step(&mut layout.clone(), 5, arrow, true, opposite);
    }
}

#[test]
fn outward_arrows_at_the_work_area_shrink_only_the_opposite_edge() {
    let layout = grid(9);
    for (id, outer, inner) in [
        (4, Left, Right),
        (6, Right, Left),
        (2, Up, Down),
        (8, Down, Up),
    ] {
        let mut layout = layout.clone();
        assert_eq!(outer.coordinate(frame(&layout, id)), outer.coordinate(AREA));
        check_step(&mut layout, id, outer, false, inner);
        let before = layout.placements(AREA);
        assert_eq!(layout.resize(id, inner, true, AREA), None);
        assert_eq!(layout.placements(AREA), before);
    }
}

#[test]
fn a_neighbor_at_its_minimum_does_not_redirect_a_resize_to_the_other_edge() {
    let mut layout = Layout::with_gap(GAP_PX);
    for id in 1..=9 {
        let width = if id == 4 { 371 } else { MIN_WIDTH };
        assert!(layout.insert_with_minimum(id, AREA, (width, MIN_HEIGHT)));
    }
    assert!(layout.resize_edge_to(5, Left, i32::MIN, AREA));
    let left = frame(&layout, 4);
    assert_eq!(left.w, 371);
    assert_eq!(left.x + left.w + GAP_PX, frame(&layout, 5).x);
    let before = layout.placements(AREA);
    assert_eq!(layout.resize(5, Left, false, AREA), None);
    assert_eq!(layout.placements(AREA), before);
}

#[test]
fn a_nested_tile_uses_its_visible_edge_and_keeps_the_far_divider_still() {
    let mut layout = grid(1);
    for (id, anchor) in [(2, 1), (3, 2), (4, 2)] {
        assert!(layout.insert_near(id, anchor, AREA, (MIN_WIDTH, MIN_HEIGHT)));
    }
    let far = frame(&layout, 4);
    check_step(&mut layout, 2, Left, false, Left);
    assert_eq!(frame(&layout, 4), far);
    assert_eq!(
        Right.coordinate(frame(&layout, 1)) + i64::from(GAP_PX),
        Left.coordinate(frame(&layout, 2))
    );
}

#[test]
fn a_rotated_pair_can_shrink_at_the_displayed_divider_and_complete_its_move() {
    let mut layout = grid(2);
    let destination = frame(&layout, 2);
    assert!(layout.move_window(1, Right, AREA));
    assert_eq!(frame(&layout, 1).y + frame(&layout, 1).h, AREA.y + AREA.h);
    check_step(&mut layout, 1, Down, true, Up);
    assert_eq!(
        Down.coordinate(frame(&layout, 2)) + i64::from(GAP_PX),
        Up.coordinate(frame(&layout, 1))
    );
    assert!(layout.move_window(1, Right, AREA));
    assert_eq!(frame(&layout, 1), destination);
}
