use super::*;

const MOVE_FLAGS: SET_WINDOW_POS_FLAGS = SWP_NOACTIVATE | SWP_NOZORDER | SWP_DEFERERASE;

// Keep the viewport's existing child pixels when only its outer edges move.
// A change of origin needs a full reflow, e.g. when the compact header appears.
pub(super) fn place_header(
    parent: HWND,
    hwnd: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    viewport: bool,
) -> bool {
    let (width, height) = (width.max(1), height.max(1));
    unsafe {
        let mut old = RECT::default();
        GetWindowRect(hwnd, &mut old);
        MapWindowPoints(null_mut(), parent, (&raw mut old).cast(), 2);
        let visible = GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0;
        let same_origin = visible && (old.left, old.top) == (x, y);
        if same_origin && (old.right - x, old.bottom - y) == (width, height) {
            return true;
        }
        let flags = if viewport { MOVE_FLAGS } else { LAYOUT_FLAGS };
        SetWindowPos(
            hwnd,
            null_mut(),
            x,
            y,
            width,
            height,
            flags | SWP_SHOWWINDOW,
        );
        if !viewport {
            // The preview aligns its chord to the right edge; resizing affects
            // more than just the newly exposed strip.
            InvalidateRect(hwnd, null(), 0);
            RedrawWindow(
                parent,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_NOCHILDREN,
            );
        }
        same_origin
    }
}

// On a stable page let Windows move sibling surfaces together. Controls with
// new bounds redraw their contents when their size changes. A changed set of
// visible controls (or a failed native batch) uses the full layout/repaint.
pub(super) fn resize_visible(view: HWND, previous: &[Placement], current: &[Placement]) -> bool {
    if previous.len() != current.len()
        || !previous.iter().zip(current).all(|(a, b)| a.hwnd == b.hwnd)
    {
        return false;
    }
    let changes = previous.iter().zip(current).filter(|(a, b)| a != b);
    let count = changes.clone().count();
    if count == 0 {
        return true;
    }
    unsafe {
        let mut batch = BeginDeferWindowPos(count as i32);
        if batch.is_null() {
            return false;
        }
        for (old, new) in changes.clone() {
            let mut flags = LAYOUT_FLAGS;
            if (old.width, old.height) == (new.width, new.height) {
                flags |= SWP_NOSIZE;
            }
            if (old.x, old.y) == (new.x, new.y) {
                flags |= SWP_NOMOVE;
            }
            batch = DeferWindowPos(
                batch,
                new.hwnd,
                null_mut(),
                new.x,
                new.y,
                new.width,
                new.height,
                flags,
            );
            if batch.is_null() {
                return false;
            }
        }
        if EndDeferWindowPos(batch) == 0 {
            return false;
        }
        RedrawWindow(view, null(), null_mut(), RDW_INVALIDATE | RDW_NOCHILDREN);
        for (_, new) in changes {
            InvalidateRect(new.hwnd, null(), 0);
        }
    }
    true
}
