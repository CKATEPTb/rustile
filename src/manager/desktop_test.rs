use std::{
    env, thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::RECT,
    Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
    System::{
        StationsAndDesktops::{GetThreadDesktop, GetUserObjectInformationW, UOI_NAME},
        Threading::GetCurrentThreadId,
    },
    UI::WindowsAndMessaging::{
        EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
        EVENT_SYSTEM_FOREGROUND, FindWindowExW, GWL_EXSTYLE, GWL_STYLE, GetDesktopWindow, GetPropW,
        GetWindowLongW, GetWindowRect, GetWindowTextW, HTTRANSPARENT, IsWindow, IsWindowVisible,
        IsZoomed, SW_HIDE, SW_MAXIMIZE, SW_SHOWNOACTIVATE, SendMessageW, SetPropW, SetWindowLongW,
        ShowWindowAsync, WM_NCHITTEST, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
        WS_MAXIMIZEBOX, WS_THICKFRAME,
    },
};
use windows_sys::core::w;

use super::{Direction, Id, Manager, Mode, Rect, native};
use crate::runtime::{
    CANCEL, CLOSE_WINDOW, CONFIRM, EXPAND, QUICK_RESIZE, QUICK_REVERSE, START_RESIZE,
    TOGGLE_MAXIMIZE, dispatch_mosaic, dispatch_with_foreground,
};

mod virtual_desktops;

// The C# harness creates the fixture windows on a private Win32 desktop.
// Confirm both the desktop and the window membership before touching geometry.
fn fixtures<const N: usize>() -> [Id; N] {
    let expected = env::var("RUSTILE_SMOKE_DESKTOP").unwrap();
    let uuid = expected.strip_prefix("rustile-smoke-").unwrap();
    assert!(uuid.len() == 32 && uuid.bytes().all(|b| b.is_ascii_hexdigit()));
    let mut name = [0u16; 128];
    let mut bytes = 0;
    unsafe {
        assert_ne!(
            GetUserObjectInformationW(
                GetThreadDesktop(GetCurrentThreadId()),
                UOI_NAME,
                name.as_mut_ptr().cast(),
                size_of_val(&name) as u32,
                &mut bytes,
            ),
            0
        );
    }
    assert_eq!(
        String::from_utf16_lossy(&name[..bytes as usize / 2 - 1]),
        expected
    );
    let windows: Vec<Id> = env::var("RUSTILE_SMOKE_WINDOWS")
        .unwrap()
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    let current = native::enumerate();
    assert!(windows.iter().all(|id| current.contains(id)));
    windows.try_into().unwrap()
}

fn until(description: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < Duration::from_secs(3), "{description}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn frame(id: Id) -> Rect {
    // Assert the DWM-visible frame directly, independent of Rustile's border
    // compensation and GetWindowRect fallback.
    let mut raw = RECT::default();
    let result = unsafe {
        DwmGetWindowAttribute(
            id as _,
            DWMWA_EXTENDED_FRAME_BOUNDS as _,
            (&raw mut raw).cast(),
            size_of::<RECT>() as u32,
        )
    };
    assert!(result >= 0, "DWM frame lookup failed: {result:#x}");
    Rect {
        x: raw.left,
        y: raw.top,
        w: raw.right - raw.left,
        h: raw.bottom - raw.top,
    }
}

fn resize_hint() -> Option<String> {
    let mut window = std::ptr::null_mut();
    loop {
        window = unsafe {
            FindWindowExW(
                std::ptr::null_mut(),
                window,
                w!("Rustile.ResizeHint"),
                std::ptr::null(),
            )
        };
        if window.is_null() {
            return None;
        }
        if unsafe { IsWindowVisible(window) } != 0 {
            let styles = unsafe { GetWindowLongW(window, GWL_EXSTYLE) } as u32;
            let required = WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT;
            assert_eq!(styles & required, required);
            assert_eq!(
                unsafe { SendMessageW(window, WM_NCHITTEST, 0, 0) },
                HTTRANSPARENT as isize
            );
            let mut text = [0u16; 512];
            let len = unsafe { GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32) };
            if len > 0 {
                return Some(String::from_utf16_lossy(&text[..len as usize]));
            }
        }
    }
}

fn settle(manager: &Manager, count: usize) {
    let expected = manager.workspace.layout.placements(manager.area.unwrap());
    assert_eq!(expected.len(), count);
    let start = Instant::now();
    loop {
        let actual: Vec<_> = expected.iter().map(|&(id, _)| (id, frame(id))).collect();
        if actual == expected {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "visible frames {actual:?}, requested {expected:?}"
        );
        thread::sleep(Duration::from_millis(15));
    }
}

fn rows(windows: &[Id], area: Rect, counts: &[usize]) -> Vec<Vec<Id>> {
    let mut order = windows.to_vec();
    order.sort_by_key(|&id| {
        let r = frame(id);
        (r.y, r.x)
    });
    assert_eq!(order.len(), counts.iter().sum::<usize>());
    let mut y = area.y;
    let mut result = Vec::new();
    for &count in counts {
        let row: Vec<_> = order.drain(..count).collect();
        let h = frame(row[0]).h;
        let mut x = area.x;
        for &id in &row {
            let r = frame(id);
            assert_eq!((r.x, r.y, r.h), (x, y, h), "rows should be full and level");
            assert!(r.w >= 160 && r.h >= 100);
            x += r.w + 8;
        }
        assert_eq!(x - 8, area.x + area.w);
        y += h + 8;
        result.push(row);
    }
    assert_eq!(y - 8, area.y + area.h);
    result
}

fn move_and_settle(manager: &mut Manager, id: Id, direction: Direction, windows: &[Id]) {
    manager.move_window(id, direction);
    settle(manager, windows.len());
    for &window in windows {
        manager.event(EVENT_OBJECT_LOCATIONCHANGE, window);
    }
}

fn move_into_wider_cell(manager: &mut Manager, windows: &[Id], active: Id, peer: Id) {
    let before: Vec<_> = windows.iter().map(|&id| (id, frame(id))).collect();
    let (top, bottom) = (frame(active), frame(peer));
    assert_eq!(top.x, bottom.x);
    assert!(top.w < bottom.w);
    let gap = bottom.y - (top.y + top.h);
    let residual_width = bottom.w - top.w - gap;
    let peer_min = manager.windows[&peer]
        .original
        .minimum_frame_size()
        .0
        .max(160);
    if residual_width < peer_min {
        return;
    }

    let &(filler, side) = before
        .iter()
        .find(|(_, r)| r.x == top.x + top.w + gap && r.y == top.y && r.h == top.h)
        .unwrap();
    // The filler in a row of three extends past the host in the row of two.
    // It can still cover the vacated tile by moving just its left border.
    move_and_settle(manager, active, Direction::Down, windows);
    assert_split(bottom, active, peer, true);
    assert_eq!(frame(active).w, top.w);
    assert_eq!(frame(filler), join_frames(top, side).unwrap());
    for &(id, tile) in &before {
        if ![active, peer, filler].contains(&id) {
            assert_eq!(frame(id), tile);
        }
    }
    move_and_settle(manager, active, Direction::Down, windows);
    assert_eq!((frame(active), frame(peer)), (bottom, top));
    for &(id, tile) in &before {
        if id != active && id != peer {
            assert_eq!(frame(id), tile);
        }
    }
    // In the opposite direction, turning the L-shaped seam moves the active
    // frame upwards while leaving a second upward step to the narrow tile.
    move_and_settle(manager, active, Direction::Up, windows);
    let (a, b) = (frame(active), frame(peer));
    // The new vertical gutter occupies only the lower row. The upper-right
    // neighbor keeps its frame; the old horizontal gutter is filled on the left.
    assert_eq!(
        a,
        Rect {
            h: bottom.y + bottom.h - top.y,
            ..top
        }
    );
    assert_eq!(
        b,
        Rect {
            x: top.x + top.w + gap,
            w: residual_width,
            ..bottom
        }
    );
    for &(id, tile) in &before {
        if id != active && id != peer {
            assert_eq!(frame(id), tile);
        }
    }

    move_and_settle(manager, active, Direction::Up, windows);
    for (id, tile) in before {
        assert_eq!(frame(id), tile);
    }
    unsafe {
        assert_ne!(
            SetPropW(
                active as _,
                w!("Rustile.Smoke.StaggeredPassed"),
                1usize as _
            ),
            0
        );
    }
}

fn five_windows(manager: &mut Manager, windows: &[Id], area: Rect) {
    for &id in windows {
        assert!(manager.admit(id, None));
    }
    manager.apply();
    settle(manager, 5);
    let initial_grid = rows(windows, area, &[3, 2]);
    move_into_wider_cell(manager, windows, initial_grid[0][0], initial_grid[1][0]);
    let grid = rows(windows, area, &[3, 2]);
    let (active, middle, end) = (grid[0][0], grid[0][1], grid[0][2]);
    let original_height = frame(active).h;
    dispatch_mosaic(manager, active, START_RESIZE);
    assert_eq!(manager.mode(), Mode::Resize);
    dispatch_mosaic(manager, active, EXPAND + 3); // Down
    settle(manager, 5);
    let applied_height = frame(active).h;
    assert!(applied_height > original_height);
    dispatch_mosaic(manager, active, CONFIRM);
    settle(manager, 5);
    assert_eq!(manager.mode(), Mode::Normal);
    assert_eq!(resize_hint(), None);
    rows(windows, area, &[3, 2]);
    assert_eq!(frame(active).h, applied_height);

    let initial: Vec<_> = windows.iter().map(|&id| (id, frame(id))).collect();
    let (left, center, right) = (frame(active), frame(middle), frame(end));
    // Start on a pixel chosen by a mouse drag, between the keyboard stops.
    assert!(
        manager
            .workspace
            .layout
            .resize_edge_to(middle, Direction::Left, center.x + 13, area)
    );
    manager.apply();
    settle(manager, 5);
    let frames = || windows.iter().map(|&id| frame(id)).collect::<Vec<_>>();
    let custom = frames();
    let offset_center = frame(middle);
    let offset_left = frame(active);
    dispatch_mosaic(manager, middle, QUICK_RESIZE);
    settle(manager, 5);
    assert_eq!(manager.mode(), Mode::Normal);
    assert_eq!(resize_hint(), None);
    let grown = frame(middle);
    assert!(grown.x < offset_center.x);
    assert_eq!(grown.x + grown.w, offset_center.x + offset_center.w);
    dispatch_mosaic(manager, middle, QUICK_REVERSE);
    settle(manager, 5);
    assert_eq!(manager.mode(), Mode::Normal);
    assert_eq!(resize_hint(), None);
    assert_eq!(frames(), custom);

    assert_eq!(left.x, area.x);
    dispatch_mosaic(manager, active, START_RESIZE);
    assert_eq!(manager.mode(), Mode::Resize);
    let hint = resize_hint().expect("modal resize shows its shortcut hint");
    assert!(
        hint.contains("Shift") && hint.contains("Esc") && hint.contains("Enter"),
        "{hint}"
    );
    assert!(!hint.contains('×'), "{hint}");
    dispatch_mosaic(manager, active, EXPAND); // Left into the work-area edge
    settle(manager, 5);
    let compressed = frame(active);
    assert_eq!(compressed.x, left.x);
    assert!(compressed.w < offset_left.w);
    let compressed_frames = frames();
    dispatch_mosaic(manager, active, CANCEL);
    settle(manager, 5);
    assert_eq!(manager.mode(), Mode::Normal);
    assert_eq!(resize_hint(), None);
    assert_eq!(frames(), compressed_frames);
    dispatch_mosaic(manager, active, QUICK_REVERSE);
    settle(manager, 5);
    assert_eq!(manager.mode(), Mode::Normal);
    assert_eq!(resize_hint(), None);
    assert_eq!(frames(), custom);
    assert!(
        manager
            .workspace
            .layout
            .resize_edge_to(middle, Direction::Left, i32::MIN, area)
    );
    manager.apply();
    settle(manager, 5);
    let min_x = frame(middle).x;
    assert!(
        manager
            .workspace
            .layout
            .resize_edge_to(middle, Direction::Left, min_x + 7, area)
    );
    manager.apply();
    settle(manager, 5);
    let near_limit = frames();
    dispatch_mosaic(manager, middle, QUICK_RESIZE);
    settle(manager, 5);
    assert_eq!(frame(middle).x, min_x);
    dispatch_mosaic(manager, middle, QUICK_REVERSE);
    settle(manager, 5);
    assert_eq!(frames(), near_limit);
    assert!(
        manager
            .workspace
            .layout
            .resize_edge_to(middle, Direction::Left, center.x, area)
    );
    manager.apply();
    settle(manager, 5);
    assert!(initial.iter().all(|&(id, rect)| frame(id) == rect));

    move_and_settle(manager, active, Direction::Right, windows);
    assert_split(join_frames(left, center).unwrap(), middle, active, false);
    for &(id, rect) in &initial {
        if id != active && id != middle {
            assert_eq!(frame(id), rect);
        }
    }

    move_and_settle(manager, active, Direction::Right, windows);
    for &(id, rect) in &initial {
        let expected = if id == active {
            center
        } else if id == middle {
            left
        } else {
            rect
        };
        assert_eq!(frame(id), expected);
    }
    // Two quick presses may both be delivered before Windows paints the preview.
    manager.move_window(active, Direction::Right);
    manager.move_window(active, Direction::Right);
    settle(manager, 5);
    assert_eq!(
        (frame(active), frame(end), frame(middle)),
        (right, center, left)
    );
    rows(windows, area, &[3, 2]);
}

fn tall_fixture_minimum(id: Id) -> (i32, i32) {
    let minimum: i32 = env::var("RUSTILE_SMOKE_MIN_HEIGHT")
        .unwrap()
        .parse()
        .unwrap();
    let mut outer = RECT::default();
    assert_ne!(unsafe { GetWindowRect(id as _, &mut outer) }, 0);
    (minimum, minimum - (outer.bottom - outer.top - frame(id).h))
}

fn six_windows(manager: &mut Manager, windows: [Id; 6], area: Rect) {
    let tall = windows[5];
    let (minimum, visible_min) = tall_fixture_minimum(tall);
    assert!(
        visible_min > (area.h - 8) / 2,
        "fixture must require a taller row"
    );
    assert!(manager.admit(tall, None));
    manager.apply();
    settle(manager, 6);
    let grid = rows(&windows, area, &[3, 3]);
    assert!(frame(tall).h >= visible_min);
    let (active, neighbor) = grid[0]
        .iter()
        .copied()
        .zip(grid[1].iter().copied())
        .find(|&(a, b)| a != tall && b != tall)
        .unwrap();
    let (top, bottom) = (frame(active), frame(neighbor));
    let original = windows.map(frame);
    move_and_settle(manager, active, Direction::Down, &windows);
    if top.w >= 328 {
        let (a, b) = (frame(active), frame(neighbor));
        let (left, right) = if a.x < b.x { (a, b) } else { (b, a) };
        assert_eq!((left.x, right.x + right.w), (top.x, top.x + top.w));
        assert_eq!(left.x + left.w + 8, right.x);
        for half in [left, right] {
            assert!(half.w >= 160);
            assert_eq!((half.y, half.y + half.h), (top.y, bottom.y + bottom.h));
        }
        for (id, rect) in windows.into_iter().zip(original) {
            if id != active && id != neighbor {
                assert_eq!(frame(id), rect);
            }
        }
        move_and_settle(manager, active, Direction::Down, &windows);
    }
    let mut expected = original;
    expected.swap(
        windows.iter().position(|&i| i == active).unwrap(),
        windows.iter().position(|&i| i == neighbor).unwrap(),
    );
    assert_eq!(windows.map(frame), expected);

    // Exit immediately after the final step, before Windows paints it.
    assert!(manager.begin_resize(active));
    for _ in 0..3 {
        manager.resize(active, Direction::Up, false);
    }
    settle(manager, 6);
    rows(&windows, area, &[3, 3]);
    assert!(frame(active).h > bottom.h);
    let live = manager.workspace.layout.placements(area);
    manager.finish_mode();
    settle(manager, 6);
    assert_eq!(manager.mode(), Mode::Normal);
    assert!(live.iter().all(|&(id, rect)| frame(id) == rect));

    let current = rows(&windows, area, &[3, 3]);
    let tall_on_top = current[0].contains(&tall);
    let shrink = if tall_on_top {
        Direction::Up
    } else {
        Direction::Down
    };
    assert!(manager.begin_resize(tall));
    for _ in 0..24 {
        manager.resize(tall, shrink, false);
    }
    manager.finish_mode();
    settle(manager, 6);
    assert_eq!(
        frame(tall).h,
        visible_min,
        "resize should stop at the application minimum"
    );
    let row = &current[usize::from(!tall_on_top)];
    let (peer, direction) = if row[0] == tall {
        (row[1], Direction::Right)
    } else {
        (
            row[row.iter().position(|&id| id == tall).unwrap() - 1],
            Direction::Left,
        )
    };
    let prior = (frame(tall), frame(peer));
    // The tall window already needs the entire height of this row. A single
    // move must take the peer's slot, including on a row of three columns.
    move_and_settle(manager, tall, direction, &windows);
    assert_eq!((frame(tall), frame(peer)), (prior.1, prior.0));
    let mut outer = RECT::default();
    assert_ne!(unsafe { GetWindowRect(tall as _, &mut outer) }, 0);
    assert_eq!(outer.bottom - outer.top, minimum);
    rows(&windows, area, &[3, 3]);
}

fn assert_split(before: Rect, existing: Id, new: Id, side_by_side: bool) {
    let (a, b) = (frame(existing), frame(new));
    assert_eq!((a.x, a.y), (before.x, before.y));
    if side_by_side {
        assert_eq!((a.h, b.y, b.h), (before.h, before.y, before.h));
        assert_eq!((b.x, b.x + b.w), (a.x + a.w + 8, before.x + before.w));
    } else {
        assert_eq!((a.w, b.x, b.w), (before.w, before.x, before.w));
        assert_eq!((b.y, b.y + b.h), (a.y + a.h + 8, before.y + before.h));
    }
}

fn set_visible(id: Id, visible: bool) {
    let command = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    assert_ne!(unsafe { ShowWindowAsync(id as _, command) }, 0);
    until("fixture visibility", || {
        (unsafe { IsWindowVisible(id as _) } != 0) == visible
    });
}

fn focused_arrivals(manager: &mut Manager, windows: [Id; 6], area: Rect) {
    let [a, b, c, d, e, _] = windows;
    manager.event(EVENT_SYSTEM_FOREGROUND, a);
    settle(manager, 1);
    move_and_settle(manager, a, Direction::Down, &[a]);
    assert_eq!(frame(a), area);

    // Windows can announce the new foreground before its SHOW. A remains
    // the anchor for B, and B becomes the anchor for the next background SHOW.
    set_visible(b, false);
    manager.event(EVENT_SYSTEM_FOREGROUND, b);
    settle(manager, 1);
    set_visible(b, true);
    manager.event(EVENT_OBJECT_SHOW, b);
    settle(manager, 2);
    assert_split(area, a, b, true);
    let (left, right) = (frame(a), frame(b));
    manager.event(EVENT_OBJECT_SHOW, c);
    settle(manager, 3);
    assert_eq!(frame(a), left);
    assert_split(right, b, c, false);
    let right_tiles = [b, c].map(frame);

    // Switch to the other branch and open two background windows from A.
    manager.event(EVENT_SYSTEM_FOREGROUND, a);
    manager.event(EVENT_OBJECT_SHOW, d);
    settle(manager, 4);
    assert_split(left, a, d, false);
    assert_eq!([b, c].map(frame), right_tiles);
    let (upper_left, lower_left) = (frame(a), frame(d));
    manager.event(EVENT_OBJECT_SHOW, e);
    settle(manager, 5);
    assert_split(upper_left, a, e, true);
    assert_eq!(
        [b, c, d].map(frame),
        [right_tiles[0], right_tiles[1], lower_left]
    );

    let (first, second) = (frame(a), frame(e));
    assert!(manager.begin_resize(a));
    manager.resize(a, Direction::Right, false);
    settle(manager, 5);
    assert!(frame(a).w > first.w && frame(e).w < second.w);
    assert_split(upper_left, a, e, true);
    assert_eq!(
        [b, c, d].map(frame),
        [right_tiles[0], right_tiles[1], lower_left]
    );
    // Start and other shell surfaces are ineligible for tiling. Their foreground
    // event must finish resize immediately, preserving the divider just set.
    let (first, second) = (frame(a), frame(e));
    manager.event(EVENT_SYSTEM_FOREGROUND, unsafe { GetDesktopWindow() } as Id);
    settle(manager, 5);
    assert_eq!(manager.mode(), Mode::Normal);
    assert_eq!(resize_hint(), None);
    assert_eq!((frame(a), frame(e)), (first, second));

    manager.move_window(a, Direction::Right);
    settle(manager, 5);
    assert_split(upper_left, e, a, false);
    manager.move_window(a, Direction::Right);
    settle(manager, 5);
    assert_eq!((frame(a), frame(e)), (second, first));
    assert_eq!(
        [b, c, d].map(frame),
        [right_tiles[0], right_tiles[1], lower_left]
    );

    set_visible(e, false);
    manager.event(EVENT_OBJECT_HIDE, e);
    manager.settle_hidden(true);
    manager.apply();
    settle(manager, 4);
    assert_eq!(
        [a, b, c, d].map(frame),
        [upper_left, right_tiles[0], right_tiles[1], lower_left]
    );
    set_visible(e, true);
}

fn move_left_past_tall_window(manager: &mut Manager, windows: [Id; 6], area: Rect) {
    let (tall, upper, moving) = (windows[5], windows[0], windows[1]);
    let (_, minimum) = tall_fixture_minimum(tall);
    assert!(manager.admit(tall, None));
    assert!(manager.admit(upper, Some(tall)));
    assert!(manager.admit(moving, Some(upper)));
    manager.apply();
    settle(manager, 3);

    // The tall app needs more than either half of the right column. Sharing
    // the left column therefore has to move its new seam to the app minimum.
    let (left, top, bottom) = (frame(tall), frame(upper), frame(moving));
    let right = Rect {
        x: top.x,
        w: top.w,
        ..area
    };
    assert_split(right, upper, moving, false);
    assert_eq!(
        left,
        Rect {
            w: right.x - 8 - area.x,
            ..area
        }
    );
    assert!(bottom.h < minimum && top.h < minimum);

    move_and_settle(manager, moving, Direction::Left, &[tall, upper, moving]);
    assert_split(left, tall, moving, false);
    assert_eq!(frame(tall).h, minimum);
    assert_eq!(frame(upper), right);
    move_and_settle(manager, moving, Direction::Left, &[tall, upper, moving]);
    assert_eq!(frame(moving), left);
    assert_split(right, upper, tall, false);
    assert_eq!(frame(tall).h, minimum, "borrow only the missing height");
}

// A layout of three tiles is full if two can be joined along a gutter
// and their union joins the third to recover the entire original column.
fn join_frames(a: Rect, b: Rect) -> Option<Rect> {
    join_frames_with_gap(a, b, 8)
}

fn join_frames_with_gap(a: Rect, b: Rect, gap: i32) -> Option<Rect> {
    if a.y == b.y && a.h == b.h {
        let (left, right) = if a.x < b.x { (a, b) } else { (b, a) };
        if left.x + left.w + gap == right.x {
            return Some(Rect {
                w: left.w + gap + right.w,
                ..left
            });
        }
    }
    if a.x == b.x && a.w == b.w {
        let (top, bottom) = if a.y < b.y { (a, b) } else { (b, a) };
        if top.y + top.h + gap == bottom.y {
            return Some(Rect {
                h: top.h + gap + bottom.h,
                ..top
            });
        }
    }
    None
}

fn move_down_beside_tall_window(manager: &mut Manager, windows: [Id; 6], area: Rect) {
    let (browser, right, explorer, telegram) = (windows[0], windows[1], windows[2], windows[5]);
    let (_, tall_min) = tall_fixture_minimum(telegram);
    // Three stacked windows (including gutters) would exceed this column by
    // 23 pixels, just as with the application minimums on the reported desktop.
    let area = Rect {
        h: area.h.min(tall_min + 2 * (100 + 8) - 23),
        ..area
    };
    manager.area = Some(area);
    for (id, anchor) in [
        (browser, None),
        (right, Some(browser)),
        (explorer, Some(browser)),
        (telegram, Some(explorer)),
    ] {
        assert!(manager.admit(id, anchor));
    }
    manager.apply();
    settle(manager, 4);
    let right_before = frame(right);
    let left = Rect {
        w: right_before.x - 8 - area.x,
        ..area
    };
    assert_eq!(join_frames(left, right_before), Some(area));
    let row = join_frames(frame(explorer), frame(telegram)).unwrap();
    assert_split(row, explorer, telegram, true);
    assert_eq!(join_frames(frame(browser), row), Some(left));
    assert!(frame(browser).y < row.y);

    manager.event(EVENT_SYSTEM_FOREGROUND, explorer);
    move_and_settle(
        manager,
        explorer,
        Direction::Down,
        &[browser, right, explorer, telegram],
    );
    let [b, e, t] = [browser, explorer, telegram].map(frame);
    assert_eq!(frame(right), right_before);
    assert_eq!(
        e.y,
        t.y + t.h + 8,
        "Down should put Explorer below Telegram: {e:?}, {t:?}"
    );
    assert!(e.x.max(t.x) < (e.x + e.w).min(t.x + t.w));
    assert_eq!(e.y + e.h, left.y + left.h);
    assert!(t.h >= tall_min && b.h >= 100 && e.h >= 100);
    assert!(
        [(b, e, t), (b, t, e), (e, t, b)]
            .into_iter()
            .any(|(a, b, c)| join_frames(a, b).and_then(|pair| join_frames(pair, c)) == Some(left))
    );
}

fn move_right_in_a_tall_pair(manager: &mut Manager, windows: [Id; 6], area: Rect) {
    let [chat, explorer, telegram, browser] = [windows[5], windows[0], windows[1], windows[2]];
    let area = Rect {
        w: area.w.min(area.h * 3 / 2),
        ..area
    };
    manager.area = Some(area);
    let tiled = [chat, explorer, telegram, browser];
    for id in tiled {
        assert!(manager.admit(id, None));
    }
    manager.apply();
    settle(manager, tiled.len());
    let column = join_frames(frame(chat), frame(telegram)).unwrap();
    let right_column = [explorer, browser].map(frame);
    assert!(column.h > column.w);
    assert_split(column, chat, telegram, false);

    // The first Down shows two columns while their underlying slots are rows.
    move_and_settle(manager, chat, Direction::Down, &tiled);
    assert_split(column, chat, telegram, true);
    let (left, right) = (frame(chat), frame(telegram));

    move_and_settle(manager, chat, Direction::Right, &tiled);
    assert_split(column, telegram, chat, false);
    let (top, bottom) = (frame(telegram), frame(chat));
    assert_eq!([explorer, browser].map(frame), right_column);
    assert!(manager.begin_resize(chat));
    manager.resize(chat, Direction::Up, false);
    settle(manager, tiled.len());
    assert!(frame(chat).y < bottom.y && frame(telegram).h < top.h);
    manager.resize(chat, Direction::Up, true);
    manager.finish_mode();
    settle(manager, tiled.len());
    assert_eq!((frame(telegram), frame(chat)), (top, bottom));

    move_and_settle(manager, chat, Direction::Right, &tiled);
    assert_eq!((frame(telegram), frame(chat)), (left, right));
    assert_eq!([explorer, browser].map(frame), right_column);
}

fn move_left_in_three_columns(
    manager: &mut Manager,
    windows: [Id; 6],
    area: Rect,
    can_stack: bool,
) {
    let [browser, chat, explorer] = [windows[0], windows[5], windows[1]];
    let tiled = [browser, chat, explorer];
    let (_, chat_min) = tall_fixture_minimum(chat);
    // Admit three columns in a shallow work area, then use the same set of
    // columns at either height. The chat fixture publishes a native minimum.
    let shallow = Rect {
        h: chat_min + 90,
        ..area
    };
    manager.area = Some(shallow);
    for id in tiled {
        assert!(manager.admit(id, None));
    }
    let area = if can_stack { area } else { shallow };
    manager.area = Some(area);
    manager.apply();
    settle(manager, 3);
    // Match the reported layout: the browser and Explorer are both wider
    // than the chat between them, and the browser/chat union is landscape.
    for (id, direction, reverse) in [
        (chat, Direction::Right, true),
        (browser, Direction::Right, false),
    ] {
        assert!(manager.begin_resize(id));
        manager.resize(id, direction, reverse);
        manager.finish_mode();
        settle(manager, 3);
    }
    let [left, middle, right] = tiled.map(frame);
    let pair = join_frames(left, middle).unwrap();
    assert_eq!(join_frames(pair, right), Some(area));
    assert!(left.x < middle.x && middle.x < right.x);
    assert!(left.w > middle.w && right.w > middle.w && pair.w > pair.h);

    move_and_settle(manager, chat, Direction::Left, &tiled);
    if can_stack {
        assert_split(pair, chat, browser, false);
        assert!(frame(chat).h >= chat_min);
        assert_eq!(frame(explorer), right);
        move_and_settle(manager, chat, Direction::Left, &tiled);
    }
    assert_eq!(tiled.map(frame), [middle, left, right]);
}

fn share_upper_tile_in_six_windows(manager: &mut Manager, windows: [Id; 6], area: Rect) {
    let [host, lower_left, active, lower_middle, filler, top_left] = windows;
    // Splitting an odd width exercises the one-pixel remainder on the second Up.
    let area = Rect {
        w: area.w - 1,
        ..area
    };
    manager.area = Some(area);
    for (id, anchor) in [
        (top_left, None),
        (host, None),
        (lower_left, None),
        (active, None),
        (lower_middle, Some(lower_left)),
        (filler, Some(active)),
    ] {
        assert!(manager.admit(id, anchor));
    }
    manager.apply();
    settle(manager, windows.len());
    assert_eq!(
        rows(&windows, area, &[2, 4]),
        [
            vec![top_left, host],
            vec![lower_left, lower_middle, active, filler]
        ]
    );

    let untouched = [top_left, lower_left, lower_middle];
    let outside = untouched.map(frame);
    let foreground = native::foreground();
    let (upper, lower, side) = (frame(host), frame(active), frame(filler));
    let filled = join_frames(lower, side).unwrap();
    assert_eq!((filled.x, filled.w), (upper.x, upper.w));

    move_and_settle(manager, active, Direction::Up, &windows);
    assert_split(upper, active, host, true);
    assert_eq!(frame(active).w, lower.w);
    assert_eq!(frame(filler), filled);
    assert_eq!(untouched.map(frame), outside);
    assert_eq!(native::foreground(), foreground);

    move_and_settle(manager, active, Direction::Up, &windows);
    assert_eq!([active, host, filler].map(frame), [upper, lower, side]);
    assert_eq!(untouched.map(frame), outside);
    assert_eq!(native::foreground(), foreground);
}

#[test]
#[ignore = "run with tests/desktop-smoke.ps1 on its isolated Win32 desktop"]
fn share_offset_upper_tile() {
    native::init_dpi();
    let windows = fixtures();
    let [
        top_right,
        host,
        bottom_left,
        bottom_middle,
        source,
        top_left,
        bottom_right,
    ] = windows;
    let failure_file = env::var_os("RUSTILE_SMOKE_FAILURE").unwrap();
    std::panic::set_hook(Box::new(move |failure| {
        let _ = std::fs::write(&failure_file, failure.to_string());
    }));
    let originals = windows.map(frame);
    let foreground = native::foreground();
    let area = native::work_area().unwrap().inset_for_tiles(6);
    let (min_height, _) = tall_fixture_minimum(top_left);
    // Start with a compact 2x2. Its tall top-left fixture makes the top-right
    // cell taller than wide, so its focused split makes two stacked tiles.
    let side = (min_height + 200).min(area.w).min(area.h);
    let mut manager = Manager::new();
    manager.area = Some(Rect {
        w: side,
        h: side,
        ..area
    });
    for id in [top_left, top_right, bottom_left, source] {
        assert!(manager.admit(id, None));
    }
    assert!(manager.admit(host, Some(top_right)));
    manager.area = Some(area);
    for (id, anchor) in [(bottom_middle, bottom_left), (bottom_right, source)] {
        assert!(manager.admit(id, Some(anchor)));
    }
    manager.apply();
    settle(&manager, 7);
    assert!(manager.begin_resize(top_left));
    for _ in 0..3 {
        manager.resize(top_left, Direction::Left, false);
    }
    manager.finish_mode();
    settle(&manager, 7);

    let [fixed_right, target, first, second, from, fixed_left, last] = windows.map(frame);
    let upper_region = join_frames(fixed_right, target).unwrap();
    assert_eq!(fixed_right.y, area.y);
    let upper_region = join_frames(fixed_left, upper_region).unwrap();
    let lower_region = join_frames(
        join_frames(first, second).unwrap(),
        join_frames(from, last).unwrap(),
    )
    .unwrap();
    assert_eq!(join_frames(upper_region, lower_region), Some(area));
    assert_eq!(target.y + target.h + 8, from.y);
    assert!(target.x < from.x && from.x + from.w < target.x + target.w);
    assert!(2 * from.x + from.w < 2 * target.x + target.w);

    move_and_settle(&mut manager, source, Direction::Up, &windows);
    assert_split(target, source, host, true);
    assert!(frame(source).w.abs_diff(frame(host).w) <= 1);
    assert_eq!([top_left, top_right].map(frame), [fixed_left, fixed_right]);
    let remaining = [bottom_left, bottom_middle, bottom_right].map(frame);
    assert!(remaining[0].x < remaining[1].x && remaining[1].x < remaining[2].x);
    assert_eq!(
        join_frames(
            join_frames(remaining[0], remaining[1]).unwrap(),
            remaining[2]
        ),
        Some(lower_region)
    );
    for (expanded, old) in remaining.into_iter().zip([first, second, last]) {
        assert!(
            expanded.w > old.w,
            "all three windows should share the released space"
        );
    }

    move_and_settle(&mut manager, source, Direction::Up, &windows);
    assert_eq!(
        windows.map(frame),
        [fixed_right, from, first, second, target, fixed_left, last]
    );
    assert_eq!(native::foreground(), foreground);
    manager.shutdown();
    until("restored all seven native frames", || {
        windows.map(frame) == originals
    });
    unsafe {
        assert_ne!(
            SetPropW(
                source as _,
                w!("Rustile.Smoke.OffsetSevenPassed"),
                1usize as _
            ),
            0
        );
    }
}

fn check_normal_placement(id: Id) {
    let outer = || {
        let mut raw = RECT::default();
        assert_ne!(unsafe { GetWindowRect(id as _, &mut raw) }, 0);
        Rect {
            x: raw.left,
            y: raw.top,
            w: raw.right - raw.left,
            h: raw.bottom - raw.top,
        }
    };
    let original = outer();
    let foreground = native::foreground();
    let corner = native::corner_radius_dip(id);
    let target = Rect {
        x: original.x + 12,
        w: original.w + 17,
        ..original
    };
    for rect in [target, original] {
        assert!(native::place_unsnapped(id as _, rect));
        until(
            "exact outer bounds after non-activating normal placement",
            || outer() == rect,
        );
        assert_eq!(native::foreground(), foreground);
        assert_eq!(native::corner_radius_dip(id), corner);
    }
}

fn paused_window_controls(active: Id, peer: Id) {
    use super::{DesktopId, DesktopMembership};
    use crate::settings::{Command, Settings};

    let paused = Settings::default().changed(Command::Toggle).unwrap();
    assert!(!paused.enabled);
    let mut manager = Manager::with_settings(paused);
    let home = DesktopMembership::Current(DesktopId(1));
    manager
        .desktops
        .set_memberships([(active, home), (peer, home)]);
    let original = frame(active);
    let untouched = frame(peer);
    manager.sync();
    assert!(manager.windows.is_empty());

    for command in [START_RESIZE, QUICK_RESIZE, QUICK_REVERSE] {
        dispatch_with_foreground(&mut manager, active, command, || active);
    }
    assert_eq!(manager.mode(), Mode::Normal);
    assert!(resize_hint().is_none());
    assert_eq!(frame(active), original);

    let veto = w!("Rustile.Smoke.VetoNextClose");
    assert_ne!(unsafe { SetPropW(active as _, veto, 1usize as _) }, 0);
    // A queued close whose window lost focus must never reach the app. The
    // following maximize serves as a barrier in the fixture's message queue.
    let foreground = std::cell::Cell::new(active);
    dispatch_with_foreground(&mut manager, active, CLOSE_WINDOW, || {
        foreground.replace(peer)
    });
    dispatch_with_foreground(&mut manager, active, TOGGLE_MAXIMIZE, || active);
    until("maximize an untiled window while paused", || {
        native::maximized(active)
    });
    assert!(!unsafe { GetPropW(active as _, veto) }.is_null());
    manager.event(EVENT_OBJECT_LOCATIONCHANGE, active);
    manager.reconcile_desktop();
    assert_eq!(frame(peer), untouched);

    dispatch_with_foreground(&mut manager, active, TOGGLE_MAXIMIZE, || active);
    until("restore the original floating frame", || {
        !native::maximized(active) && frame(active) == original
    });
    // Fixed-size app windows can still be closed on a paused desktop.
    let style = unsafe { GetWindowLongW(active as _, GWL_STYLE) };
    assert_eq!(
        unsafe {
            SetWindowLongW(
                active as _,
                GWL_STYLE,
                style & !((WS_THICKFRAME | WS_MAXIMIZEBOX) as i32),
            )
        },
        style
    );
    dispatch_with_foreground(&mut manager, active, TOGGLE_MAXIMIZE, || active);
    dispatch_with_foreground(&mut manager, active, CLOSE_WINDOW, || active);
    until("app vetoes close while paused", || unsafe {
        GetPropW(active as _, veto).is_null()
    });
    assert_ne!(unsafe { IsWindow(active as _) }, 0);
    assert!(
        !native::maximized(active),
        "the maximize button is disabled"
    );
    dispatch_with_foreground(&mut manager, active, CLOSE_WINDOW, || active);
    until("app closes while paused", || unsafe {
        IsWindow(active as _) == 0
    });
    manager.event(EVENT_OBJECT_DESTROY, active);
    manager.reconcile_desktop();
    assert_eq!(frame(peer), untouched);
    assert!(manager.windows.is_empty());
    manager.shutdown();
}

#[test]
#[ignore = "run with tests/desktop-smoke.ps1 on its isolated Win32 desktop"]
fn window_controls() {
    use super::{DesktopId, DesktopMembership};

    native::init_dpi();
    let [first, second, active, floating] = fixtures();
    let failure_file = env::var_os("RUSTILE_SMOKE_FAILURE").unwrap();
    std::panic::set_hook(Box::new(move |failure| {
        let _ = std::fs::write(&failure_file, failure.to_string());
    }));
    paused_window_controls(floating, first);
    let windows = [first, second, active];
    let zoomed = || unsafe { IsZoomed(active as _) != 0 };
    assert_ne!(unsafe { ShowWindowAsync(active as _, SW_MAXIMIZE) }, 0);
    let work = native::work_area().unwrap();
    until("native maximize before Rustile started", || {
        let large = frame(active);
        zoomed()
            && large.x <= work.x + 1
            && large.y <= work.y + 1
            && large.x + large.w >= work.x + work.w - 1
            && large.y + large.h >= work.y + work.h - 1
    });
    // The reference is this same native HWND, including any DWM frame overhang.
    let maximized_frame = frame(active);
    let mut manager = Manager::new();
    let destination = DesktopId(2);
    manager.desktops.set_memberships(windows.map(|id| {
        let membership = if id == active {
            DesktopMembership::Current(destination)
        } else {
            DesktopMembership::Other(DesktopId(1))
        };
        (id, membership)
    }));
    manager.reconcile_desktop();
    assert_eq!(
        manager.layout_desktop(),
        None,
        "all destination windows are maximized"
    );
    let foreground = std::cell::Cell::new(windows[0]);
    assert!(manager.restore_desktop_focus_with(
        destination,
        || foreground.get(),
        |id, previous| {
            assert_eq!(
                previous, windows[0],
                "the outgoing app still appears visible"
            );
            assert_eq!(id, active);
            foreground.set(id);
            true
        }
    ));
    assert_eq!(foreground.get(), active);
    assert_eq!(frame(active), maximized_frame);
    manager
        .desktops
        .set_memberships(windows.map(|id| (id, DesktopMembership::Current(DesktopId(1)))));
    manager.reconcile_desktop();
    settle(&manager, 2);
    assert!(
        manager.transferable_on(active, DesktopId(1)),
        "an already maximized window can move desktops"
    );
    dispatch_with_foreground(&mut manager, active, TOGGLE_MAXIMIZE, || active);
    until(
        "window already maximized at startup can return to the mosaic",
        || !zoomed(),
    );
    manager.reconcile_desktop();
    settle(&manager, 3);
    let initial = windows.map(frame);
    for direction in [Direction::Left, Direction::Up] {
        manager.quick_resize(active, direction, false);
        settle(&manager, 3);
        if windows.map(frame) != initial {
            break;
        }
    }
    let custom = windows.map(frame);
    assert_ne!(
        custom, initial,
        "the round trip must exercise a custom divider"
    );

    dispatch_with_foreground(&mut manager, active, TOGGLE_MAXIMIZE, || active);
    until("fixture maximized", zoomed);
    manager.event(EVENT_OBJECT_LOCATIONCHANGE, active);
    manager.reconcile_desktop();
    until(
        "Win+F matches all four edges of native Windows maximize",
        || frame(active) == maximized_frame,
    );
    dispatch_with_foreground(&mut manager, active, TOGGLE_MAXIMIZE, || active);
    until("fixture restored from maximize", || !zoomed());
    manager.event(EVENT_OBJECT_LOCATIONCHANGE, active);
    manager.reconcile_desktop();
    settle(&manager, 3);
    assert_eq!(windows.map(frame), custom);

    // A resizable tile can disable its maximize button while still accepting close.
    let style = unsafe { GetWindowLongW(active as _, GWL_STYLE) };
    assert_ne!(style as u32 & WS_MAXIMIZEBOX, 0);
    assert_eq!(
        unsafe { SetWindowLongW(active as _, GWL_STYLE, style & !(WS_MAXIMIZEBOX as i32)) },
        style
    );
    assert_eq!(
        manager.shortcut_target(active),
        Some(super::ShortcutTarget::Tile(active))
    );
    dispatch_with_foreground(&mut manager, active, TOGGLE_MAXIMIZE, || active);
    let veto = w!("Rustile.Smoke.VetoNextClose");
    assert_ne!(unsafe { SetPropW(active as _, veto, 1usize as _) }, 0);
    dispatch_with_foreground(&mut manager, active, CLOSE_WINDOW, || active);
    until("fixture processed its close veto", || unsafe {
        GetPropW(active as _, veto).is_null()
    });
    assert_ne!(unsafe { IsWindow(active as _) }, 0);
    assert!(!zoomed());
    manager.reconcile_desktop();
    settle(&manager, 3);
    assert_eq!(windows.map(frame), custom);
    dispatch_with_foreground(&mut manager, active, CLOSE_WINDOW, || active);
    until("fixture accepted the next close request", || unsafe {
        IsWindow(active as _) == 0
    });
    manager.event(EVENT_OBJECT_DESTROY, active);
    manager.reconcile_desktop();
    settle(&manager, 2);
    let [a, b] = [windows[0], windows[1]].map(frame);
    let gap = native::spacing_px(manager.settings.gap);
    assert_eq!(join_frames_with_gap(a, b, gap), manager.area);
    manager.shutdown();
}

#[test]
#[ignore = "run with tests/desktop-smoke.ps1 on its isolated Win32 desktop"]
fn move_windows() {
    native::init_dpi();
    let windows = fixtures();
    let failure_file = env::var_os("RUSTILE_SMOKE_FAILURE").unwrap();
    std::panic::set_hook(Box::new(move |failure| {
        let _ = std::fs::write(&failure_file, failure.to_string());
    }));
    let originals = windows.map(frame);
    check_normal_placement(windows[0]);
    crate::tray::check_work_area_notifications();
    let foreground = native::foreground();
    let work = native::work_area().unwrap();
    let width = work.w.min(1160);
    let area = Rect {
        w: width,
        h: work.h.min(width / 2).min(624),
        ..work
    };
    let run = |scenario: fn(&mut Manager, [Id; 6], Rect)| {
        let mut manager = Manager::new();
        manager.area = Some(area);
        scenario(&mut manager, windows, area);
        assert_eq!(native::foreground(), foreground);
        manager.shutdown();
        until("restored all original DWM frames", || {
            windows.map(frame) == originals
        });
    };
    run(|manager, fixtures, area| {
        five_windows(manager, &fixtures[..5], area);
        six_windows(manager, fixtures, area);
    });
    run(focused_arrivals);
    run(move_left_past_tall_window);
    run(move_down_beside_tall_window);
    run(move_right_in_a_tall_pair);
    run(share_upper_tile_in_six_windows);
    run(virtual_desktops::round_trip);
    run(virtual_desktops::transfer_and_follow);
    run(panel_reservations);
    run(|m, w, a| move_left_in_three_columns(m, w, a, true));
    run(|m, w, a| move_left_in_three_columns(m, w, a, false));
    run(|manager, windows, _| {
        let settings = manager.settings;
        let area = super::WorkArea::read().unwrap().tiles(settings);
        manager.area = Some(area);
        let id = windows[0];
        assert!(manager.admit(id, None));
        // Give the fixture a known minimum that just fits the area. Real HWND
        // placement and persistence must both stay untouched if padding fails.
        manager.workspace.layout = super::Layout::with_gap(native::spacing_px(settings.gap));
        assert!(
            manager
                .workspace
                .layout
                .insert_with_minimum(id, area, (area.w, area.h))
        );
        manager.apply();
        settle(manager, 1);
        let mut tighter = settings;
        tighter.padding += 1;
        assert!(
            manager
                .apply_settings_with(tighter, || panic!("invalid spacing reached disk"))
                .is_err()
        );
        assert_eq!(manager.settings, settings);
        assert_eq!(frame(id), area);

        let mut paused = settings;
        paused.enabled = false;
        assert!(
            manager
                .apply_settings_with(paused, || Err("read-only configuration".into()))
                .is_err()
        );
        assert_eq!(manager.settings, settings);
        manager.apply_settings(paused).unwrap();
        manager.event(EVENT_OBJECT_SHOW, windows[1]);
        assert!(!manager.begin_resize(id));
        assert!(manager.refresh_focus().is_none());
        assert_eq!(manager.workspace.layout.placements(area), [(id, area)]);
        assert_eq!(frame(id), area);
        manager.shutdown();
    });
    // A stale --exact test name would otherwise let libtest exit successfully.
    unsafe {
        assert_ne!(
            SetPropW(windows[0] as _, w!("Rustile.Smoke.MovePassed"), 1usize as _),
            0
        )
    };
}

// Inject only the monitor reading. Use the ordinary inventory, reconciliation
// and placement paths, and measure the resulting HWNDs on the private desktop.
fn panel_reservations(manager: &mut Manager, windows: [Id; 6], area: Rect) {
    use super::{DesktopId, DesktopMembership, WorkArea};

    let [first, second, parked, ..] = windows;
    let homes = [
        (first, DesktopId(1)),
        (second, DesktopId(1)),
        (parked, DesktopId(2)),
    ];
    let select = |manager: &mut Manager, active: DesktopId| {
        manager.desktops.set_memberships(homes.map(|(id, home)| {
            (
                id,
                if active == home {
                    DesktopMembership::Current(home)
                } else {
                    DesktopMembership::Other(home)
                },
            )
        }));
    };
    let base = WorkArea {
        bounds: Rect {
            x: area.x + 10,
            w: area.w - 10,
            ..area
        },
        dpi: native::system_dpi(),
    };
    select(manager, DesktopId(2));
    manager.reconcile_in(Some(base));
    settle(manager, 1);
    let parked_before = frame(parked);
    select(manager, DesktopId(1));
    manager.reconcile_in(Some(base));
    settle(manager, 2);
    let initial = [frame(first), frame(second)];
    assert!(manager.begin_resize(first));

    for (left, top, right, bottom) in [(0, 32, 0, 0), (63, 32, 0, 0), (0, 0, 75, 24), (0, 0, 0, 0)]
    {
        let work = WorkArea {
            bounds: Rect {
                x: base.bounds.x + left,
                y: base.bounds.y + top,
                w: base.bounds.w - left - right,
                h: base.bounds.h - top - bottom,
            },
            ..base
        };
        manager.reconcile_in(Some(work));
        settle(manager, 2);
        let (a, b) = (frame(first), frame(second));
        let expected = work.tiles(manager.settings);
        assert_eq!((a.x.min(b.x), a.y.min(b.y)), (expected.x, expected.y));
        assert_eq!(
            ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h)),
            (expected.x + expected.w, expected.y + expected.h)
        );
        assert_eq!(manager.mode(), super::Mode::Normal);
        assert_eq!(frame(parked), parked_before);
    }
    assert_eq!([frame(first), frame(second)], initial);
    // A space too small for a pair still accommodates either fixture alone.
    // The released window must move out from under the panel and rejoin on growth.
    let small = WorkArea {
        bounds: Rect {
            x: base.bounds.x + 110,
            y: base.bounds.y + 50,
            w: 240,
            h: 185,
        },
        ..base
    };
    manager.reconcile_in(Some(small));
    settle(manager, 1);
    until("former tile inside the available work area", || {
        [first, second].map(frame).iter().all(|r| {
            r.x >= small.bounds.x
                && r.y >= small.bounds.y
                && r.x + r.w <= small.bounds.x + small.bounds.w
                && r.y + r.h <= small.bounds.y + small.bounds.h
        })
    });
    assert_eq!(frame(parked), parked_before);
    manager.reconcile_in(Some(base));
    settle(manager, 2);
    let work = WorkArea {
        bounds: Rect {
            y: base.bounds.y + 32,
            h: base.bounds.h - 32,
            ..base.bounds
        },
        ..base
    };
    manager.reconcile_in(Some(work));
    select(manager, DesktopId(2));
    manager.reconcile_in(Some(work));
    settle(manager, 1);
    assert_eq!(frame(parked), work.tiles(manager.settings));
    // Let the harness restore all three fixtures on shutdown.
    manager
        .desktops
        .set_memberships(homes.map(|(id, _)| (id, DesktopMembership::Current(DesktopId(2)))));
}
