use super::*;

fn area(w: i32, h: i32) -> Rect {
    Rect { x: 11, y: 19, w, h }
}

fn inventory(count: Id, area: Rect) -> Layout {
    let mut layout = Layout::default();
    for id in 1..=count {
        assert!(layout.insert(id, area));
    }
    layout
}

#[test]
fn repeated_odd_pixel_panel_reservations_restore_the_original_dividers() {
    let full = area(1904, 1134);
    let mut layout = inventory(4, full);
    assert!(layout.insert_near(5, 3, full, (160, 100)));
    let before = layout.placements(full);
    for _ in 0..8 {
        for (top, right) in [(33, 0), (33, 31), (0, 31), (0, 0)] {
            let reserved = Rect {
                y: full.y + top,
                h: full.h - top,
                w: full.w - right,
                ..full
            };
            assert!(layout.refit_work_area(reserved, GAP, |_| None).is_empty());
        }
        assert_eq!(layout.placements(full), before);
    }
}

#[test]
fn clamping_a_row_to_its_minimum_preserves_unconstrained_column_weights() {
    let full = area(1904, 1134);
    let mut layout = inventory(4, full);
    let before = layout.placements(full);
    let reserved = Rect {
        w: full.w - 31,
        h: full.h - 75,
        ..full
    };
    assert!(
        layout
            .refit_work_area(reserved, GAP, |id| (id == 1).then_some((160, 548)))
            .is_empty()
    );
    assert_eq!(layout.placements(reserved)[0].1.h, 548);
    assert!(layout.refit_work_area(full, GAP, |_| None).is_empty());
    for ((id, a), (same, b)) in before.into_iter().zip(layout.placements(full)) {
        assert_eq!((id, a.x, a.w), (same, b.x, b.w));
    }
}

#[test]
fn reserving_a_panel_preserves_custom_dividers_and_a_pending_move() {
    let old = area(1901, 1053);
    let mut layout = inventory(6, old);
    assert!(layout.resize(1, Direction::Right, false, old).is_some());
    assert!(layout.move_window(1, Direction::Right, old));
    let before = layout.placements(old);
    let reserved = Rect {
        y: old.y + 32,
        h: old.h - 32,
        ..old
    };
    assert!(layout.refit_work_area(reserved, GAP, |_| None).is_empty());
    let new = layout.placements(reserved);
    assert!(new.iter().all(|(_, r)| reserved.contains(*r)));
    for ((id, a), (other, b)) in before.iter().zip(&new) {
        assert_eq!(id, other);
        assert_eq!((a.x, a.w), (b.x, b.w));
    }
    let cells = layout.cell_rects(reserved);
    assert_ne!(new[0].1, cells[0]);
    assert!(layout.move_window(1, Direction::Right, reserved));
    assert_eq!(layout.placements(reserved)[1], (1, cells[1]));
}

#[test]
fn narrow_work_area_rotates_an_infeasible_grid_and_keeps_every_window() {
    let mut layout = inventory(3, area(1020, 140));
    let narrow = area(250, 740);
    assert!(layout.refit_work_area(narrow, 12, |_| None).is_empty());
    let frames = layout.placements(narrow);
    let mut y = narrow.y;
    for (i, (id, r)) in frames.into_iter().enumerate() {
        assert_eq!(id as usize, i + 1);
        assert_eq!((r.x, r.y, r.w), (narrow.x, y, narrow.w));
        y += r.h + 12;
    }
    assert_eq!(y - 12, narrow.y + narrow.h);
}

#[test]
fn heterogeneous_minima_can_still_fit_as_a_column_beside_a_stack() {
    let mut layout = inventory(3, area(1200, 240));
    let new = area(780, 600);
    let minima = [(540, 590), (200, 250), (200, 250)];
    let released = layout.refit_work_area(new, GAP, |id| Some(minima[id as usize - 1]));
    assert!(released.is_empty());
    let frames = layout.placements(new);
    assert_eq!(frames.len(), 3);
    for (id, rect) in frames {
        assert!(new.contains(rect));
        let (w, h) = minima[id as usize - 1];
        assert!(rect.w >= w && rect.h >= h);
    }
}

#[test]
fn unfit_windows_are_released_without_reserving_space_and_can_be_readmitted() {
    let old = area(1600, 700);
    let new = area(510, 147);
    let mut layout = inventory(3, old);
    assert_eq!(
        layout.refit_work_area(new, GAP, |id| {
            Some(if id == 1 { (800, 300) } else { (210, 120) })
        }),
        vec![1]
    );
    let frames = layout.placements(new);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].1.joined(frames[1].1, Axis::X, GAP), Some(new));
    assert!(layout.refit_work_area(old, GAP, |_| None).is_empty());
    assert!(layout.insert_near(1, frames[0].0, old, (800, 300)));
    let mut all = layout.refit_work_area(area(90, 80), GAP, |_| None);
    all.sort_unstable();
    assert_eq!(all, vec![1, 2, 3]);
    assert!(layout.placements(area(90, 80)).is_empty());
}
