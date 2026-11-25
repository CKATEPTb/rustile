use super::*;
use std::collections::BTreeMap;

const AREA: Rect = Rect {
    x: -1001,
    y: 47,
    w: 1907,
    h: 1153,
};
const NATIVE: (i32, i32) = (160, 100);

fn frames(layout: &Layout) -> BTreeMap<Id, Rect> {
    layout.placements(AREA).into_iter().collect()
}

#[test]
fn splits_alternate_per_branch_and_follow_refocus() {
    let mut l = Layout::default();
    assert!(l.insert(1, AREA));
    assert!(l.insert_near(2, 1, AREA, NATIVE));
    let two = frames(&l);
    assert_eq!(two[&1].joined(two[&2], Axis::X, GAP), Some(AREA));
    assert!(l.insert_near(3, 2, AREA, NATIVE));
    let three = frames(&l);
    assert_eq!(three[&1], two[&1]);
    assert_eq!(three[&2].joined(three[&3], Axis::Y, GAP), Some(two[&2]));
    assert!(three[&3].y > three[&2].y);
    assert!(l.insert_near(4, 3, AREA, NATIVE));
    let four = frames(&l);
    assert_eq!(four[&3].joined(four[&4], Axis::X, GAP), Some(three[&3]));
    assert_eq!(four[&2], three[&2]);
    assert!(l.insert_near(5, 1, AREA, NATIVE));
    let five = frames(&l);
    assert_eq!(five[&1].joined(five[&5], Axis::Y, GAP), Some(two[&1]));
    for id in [2, 3, 4] {
        assert_eq!(five[&id], four[&id]);
    }
    l.remove(5);
    assert_eq!(frames(&l), four);
    l.remove(3);
    assert_eq!(frames(&l)[&4], three[&3]);
    l.remove(2);
    assert_eq!(frames(&l)[&4], two[&2]);
    l.remove(1);
    assert_eq!(frames(&l)[&4], AREA);
}

#[test]
fn two_startup_tiles_on_an_ultrawide_remember_the_existing_cut() {
    let mut l = Layout::default();
    let wide = Rect { w: 4301, ..AREA };
    for id in 1..=2 {
        assert!(l.insert(id, wide));
    }
    let original = l.placements(wide)[0].1;
    assert!(l.insert_near(3, 1, wide, NATIVE));
    let p: BTreeMap<_, _> = l.placements(wide).into_iter().collect();
    assert_eq!(p[&1].joined(p[&3], Axis::Y, GAP), Some(original));
}

#[test]
fn portrait_begins_below_and_impossible_insert_is_transactional() {
    let mut l = Layout::default();
    let tall = Rect {
        w: 601,
        h: 1381,
        ..AREA
    };
    assert!(l.insert(1, tall));
    assert!(l.insert_near(2, 1, tall, NATIVE));
    let before = l.placements(tall);
    assert_eq!(before[0].1.joined(before[1].1, Axis::Y, GAP), Some(tall));
    assert!(!l.insert_near(3, 2, tall, (700, 1500)));
    assert!(!l.insert_near(2, 1, tall, NATIVE));
    assert!(!l.insert_near(3, 99, tall, NATIVE));
    assert_eq!(l.placements(tall), before);
}

#[test]
fn native_minima_try_local_perpendicular_then_redistribute_only_if_needed() {
    let mut l = Layout::default();
    assert!(l.insert(1, AREA));
    assert!(l.insert_near(2, 1, AREA, NATIVE));
    let two = frames(&l);
    // Children of the right half normally split down. This pair is too
    // tall to stack but fits side by side using the original right half.
    assert!(l.insert_near(3, 2, AREA, (200, 1100)));
    let three = frames(&l);
    assert_eq!(three[&1], two[&1]);
    assert_eq!(three[&2].joined(three[&3], Axis::X, GAP), Some(two[&2]));
    // More width is needed than in the focused tile. Borrow sufficient
    // space to keep the newcomer adjacent while honoring its minimum.
    assert!(l.insert_near(4, 3, AREA, (720, 800)));
    let four = frames(&l);
    assert!(four[&4].w >= 720 && four[&4].h >= 800);
    assert!(four[&3].w >= 200 && four[&3].h >= 1100);
    assert!(four[&4].joined(four[&3], Axis::X, GAP).is_some());
}

#[test]
fn inserting_during_a_move_splits_the_visible_tile_and_preserves_disjoint_moves() {
    let mut l = Layout::default();
    for id in 1..=6 {
        assert!(l.insert(id, AREA));
    }
    assert!(l.move_window(1, Direction::Right, AREA));
    assert!(l.move_window(4, Direction::Right, AREA));
    let displayed = frames(&l);
    assert!(l.insert_near(7, 1, AREA, NATIVE));
    let split = frames(&l);
    assert!(
        [Axis::X, Axis::Y]
            .iter()
            .any(|axis| split[&1].joined(split[&7], *axis, GAP) == Some(displayed[&1]))
    );
    for id in 2..=6 {
        assert_eq!(split[&id], displayed[&id]);
    }
    assert!(l.move_window(4, Direction::Right, AREA));
    assert!(frames(&l)[&4].x > split[&4].x);
    l.remove(7);
    assert_eq!(frames(&l)[&1], displayed[&1]);
}

#[test]
fn resizing_and_two_step_moves_use_the_new_dividers() {
    let mut l = Layout::default();
    assert!(l.insert(1, AREA));
    assert!(l.insert_near(2, 1, AREA, NATIVE));
    assert!(l.insert_near(3, 1, AREA, NATIVE));
    let before = frames(&l);
    assert!(
        l.resize(1, Direction::Down, false, AREA, &mut None)
            .is_some()
    );
    let resized = frames(&l);
    assert!(resized[&1].h > before[&1].h);
    assert_eq!(resized[&2], before[&2]);
    assert!(l.move_window(1, Direction::Down, AREA));
    assert!(l.move_window(1, Direction::Down, AREA));
    let moved = frames(&l);
    assert_eq!((moved[&1], moved[&3]), (resized[&3], resized[&1]));
    assert_eq!(moved[&2], before[&2]);
}
