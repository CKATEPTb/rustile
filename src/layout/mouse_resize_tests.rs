use super::*;
use std::collections::BTreeMap;

const AREA: Rect = Rect {
    x: -1561,
    y: 13,
    w: 1721,
    h: 1043,
};

fn grid(count: Id) -> Layout {
    let mut l = Layout::default();
    for id in 1..=count {
        assert!(l.insert(id, AREA));
    }
    l
}

fn frames(l: &Layout) -> BTreeMap<Id, Rect> {
    l.placements(AREA).into_iter().collect()
}

fn check_edge(old: Rect, new: Rect, edge: Direction, coordinate: i32) {
    for d in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        let expected = if d == edge {
            i64::from(coordinate)
        } else {
            d.coordinate(old)
        };
        assert_eq!(d.coordinate(new), expected, "{d:?} while dragging {edge:?}");
    }
}

#[test]
fn all_four_grid_edges_grow_and_shrink_by_exact_pixels() {
    for edge in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        let mut l = grid(9);
        let old = frames(&l)[&5];
        for delta in [1, -7, 19, -1, 0] {
            let requested = edge.coordinate(old) as i32 + delta;
            assert!(l.resize_edge_to(5, edge, requested, AREA));
            check_edge(old, frames(&l)[&5], edge, requested);
            assert!(!l.resize_edge_to(5, edge, requested, AREA));
        }
    }
}

#[test]
fn dragging_an_outer_edge_never_selects_an_opposite_divider() {
    let mut l = grid(9);
    let before = frames(&l);
    for (id, edge) in [
        (1, Direction::Left),
        (3, Direction::Right),
        (2, Direction::Up),
        (8, Direction::Down),
    ] {
        assert!(!l.resize_edge_to(id, edge, 25, AREA));
    }
    assert!(!l.resize_edge_to(9999, Direction::Right, 50, AREA));
    assert_eq!(frames(&l), before);
}

#[test]
fn resizing_the_parent_of_a_nested_split_keeps_interior_dividers_in_place() {
    let mut l = grid(1);
    assert!(l.insert_near(2, 1, AREA, (160, 100)));
    assert!(l.insert_near(3, 2, AREA, (160, 100)));
    assert!(l.insert_near(4, 2, AREA, (180, 100)));
    let original = frames(&l);
    let coordinate = original[&2].x + 17;
    assert!(l.resize_edge_to(2, Direction::Left, coordinate, AREA));
    let changed = frames(&l);
    check_edge(original[&2], changed[&2], Direction::Left, coordinate);
    assert_eq!(changed[&4], original[&4]);
    assert_eq!(changed[&3].x, coordinate);
    assert_eq!(changed[&1].x + changed[&1].w + GAP, coordinate);
    // The edge stops exactly when window 2 reaches its 160px minimum.
    let limit = original[&2].x + original[&2].w - MIN_WIDTH;
    assert!(l.resize_edge_to(2, Direction::Left, i32::MAX, AREA));
    check_edge(original[&2], frames(&l)[&2], Direction::Left, limit);
    assert!(!l.resize_edge_to(2, Direction::Left, i32::MAX, AREA));
}

#[test]
fn every_tile_facing_the_dragged_seam_contributes_its_native_minimum() {
    let mut l = grid(1);
    assert!(l.insert_near(2, 1, AREA, (160, 100)));
    assert!(l.insert_near(3, 1, AREA, (701, 100)));
    let old = frames(&l);
    assert!(l.resize_edge_to(1, Direction::Right, i32::MIN, AREA));
    let changed = frames(&l);
    check_edge(old[&1], changed[&1], Direction::Right, AREA.x + 701);
    assert_eq!(changed[&3].w, 701);
    assert_eq!(changed[&2].x, AREA.x + 701 + GAP);
    assert!(!l.resize_edge_to(1, Direction::Right, i32::MIN, AREA));
}

#[test]
fn dragging_a_preview_divider_keeps_its_second_keyboard_step() {
    let mut l = grid(2);
    let slots = frames(&l);
    assert!(l.move_window(1, Direction::Right, AREA));
    let before = frames(&l);
    let edge = if before[&1].y == AREA.y {
        Direction::Down
    } else {
        Direction::Up
    };
    let requested = edge.coordinate(before[&1]) as i32 + 17;
    assert!(l.resize_edge_to(1, edge, requested, AREA));
    check_edge(before[&1], frames(&l)[&1], edge, requested);
    assert!(l.move_window(1, Direction::Right, AREA));
    assert_eq!(frames(&l)[&1], slots[&2]);
    assert_eq!(frames(&l)[&2], slots[&1]);
}

#[test]
fn mouse_resize_uses_the_displayed_frame_of_a_reclaimed_vacancy() {
    let mut l = grid(1);
    assert!(l.insert_near(2, 1, AREA, (160, 100)));
    assert!(l.insert_near(3, 1, AREA, (160, 100)));
    assert!(l.move_window(1, Direction::Right, AREA));
    assert!(l.shared.iter().any(|p| p.vacancy().is_some()));
    let before = frames(&l);
    let active = before[&1];
    let host = before[&2];
    let edge = if active.x + active.w + GAP == host.x {
        Direction::Right
    } else if host.x + host.w + GAP == active.x {
        Direction::Left
    } else if active.y + active.h + GAP == host.y {
        Direction::Down
    } else {
        assert_eq!(host.y + host.h + GAP, active.y);
        Direction::Up
    };
    let requested = edge.coordinate(active) as i32 + 7;
    assert!(l.resize_edge_to(1, edge, requested, AREA));
    let after = frames(&l);
    check_edge(active, after[&1], edge, requested);
    assert_eq!(after[&3], before[&3]);
}

#[test]
fn corner_drag_changes_both_requested_edges_and_preserves_a_distant_pending_move() {
    let mut l = grid(9);
    let original = frames(&l);
    assert!(l.move_window(7, Direction::Right, AREA));
    let before = frames(&l);
    let active = before[&2];
    let right = active.x + active.w + 13;
    let bottom = active.y + active.h - 7;
    assert!(l.resize_edge_to(2, Direction::Right, right, AREA));
    assert!(l.resize_edge_to(2, Direction::Down, bottom, AREA));
    let after = frames(&l);
    assert_eq!(
        after[&2],
        Rect {
            w: right - active.x,
            h: bottom - active.y,
            ..active
        }
    );
    for id in [7, 8, 9] {
        assert_eq!(after[&id], before[&id]);
    }
    assert!(l.move_window(7, Direction::Right, AREA));
    assert_eq!(frames(&l)[&7], original[&8]);
}
