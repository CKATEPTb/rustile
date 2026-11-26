use super::*;

/// The back buffer lives only for an interactive resize. Growing in blocks
/// avoids reallocating a screen-sized bitmap for each pixel of a border drag.
#[derive(Default)]
pub(in super::super) struct ResizeCanvas {
    dc: HDC,
    bitmap: HBITMAP,
    original: HGDIOBJ,
    width: i32,
    height: i32,
}

impl ResizeCanvas {
    fn prepare(&mut self, target: HDC, width: i32, height: i32) -> Option<HDC> {
        if self.width >= width && self.height >= height {
            return Some(self.dc);
        }
        let round = |value: i32| value.saturating_add(127) / 128 * 128;
        let w = round(width.max(self.width));
        let h = round(height.max(self.height));
        unsafe {
            if self.dc.is_null() {
                self.dc = CreateCompatibleDC(target);
                if self.dc.is_null() {
                    return None;
                }
            }
            let bitmap = CreateCompatibleBitmap(target, w, h);
            if bitmap.is_null() {
                return None;
            }
            let old = SelectObject(self.dc, bitmap);
            if old.is_null() {
                DeleteObject(bitmap);
                return None;
            }
            if self.bitmap.is_null() {
                self.original = old;
            } else {
                DeleteObject(self.bitmap);
            }
            self.bitmap = bitmap;
            self.width = w;
            self.height = h;
            Some(self.dc)
        }
    }
}

impl Drop for ResizeCanvas {
    fn drop(&mut self) {
        unsafe {
            if !self.dc.is_null() {
                if !self.bitmap.is_null() {
                    SelectObject(self.dc, self.original);
                    DeleteObject(self.bitmap);
                }
                DeleteDC(self.dc);
            }
        }
    }
}

pub(super) fn paint(ui: &ShortcutWindow, target: HDC, placements: &[Placement], bounds: &RECT) {
    if target.is_null() || bounds.right <= 0 || bounds.bottom <= 0 {
        return;
    }
    let buffer = ui
        .resize_canvas
        .borrow_mut()
        .prepare(target, bounds.right, bounds.bottom);
    let dc = buffer.unwrap_or(target);
    paint::background(dc, bounds, ui.colors.get());
    let view = child(ui.hwnd.get(), RESIZE_VIEW);
    let mut cursor = POINT::default();
    let inside = unsafe {
        GetCursorPos(&mut cursor) != 0
            && WindowFromPoint(cursor) == view
            && ScreenToClient(view, &mut cursor) != 0
    };
    let focus = unsafe { GetFocus() };
    let hide_focus =
        unsafe { SendMessageW(ui.hwnd.get(), WM_QUERYUISTATE, 0, 0) as u32 } & UISF_HIDEFOCUS != 0;
    for place in placements {
        let r = RECT {
            left: place.x,
            top: place.y,
            right: place.x + place.width,
            bottom: place.y + place.height,
        };
        let id = unsafe { GetDlgCtrlID(place.hwnd) };
        let disabled = match id {
            PAGE_PREV => ui.page.get() == 0,
            PAGE_NEXT => ui.page.get() + 1 >= ui.pages.get(),
            _ => unsafe { IsWindowEnabled(place.hwnd) == 0 },
        };
        let mut state = 0;
        if disabled {
            state |= ODS_DISABLED;
        }
        if focus == place.hwnd {
            state |= ODS_FOCUS;
            if hide_focus {
                state |= ODS_NOFOCUSRECT;
            }
        }
        let hovered = inside && !disabled && unsafe { PtInRect(&r, cursor) != 0 };
        ui.draw_cached(
            &DRAWITEMSTRUCT {
                CtlID: id as u32,
                hwndItem: place.hwnd,
                hDC: dc,
                rcItem: r,
                itemState: state,
                ..Default::default()
            },
            hovered,
        );
    }
    if buffer.is_some() {
        unsafe { BitBlt(target, 0, 0, bounds.right, bounds.bottom, dc, 0, 0, SRCCOPY) };
    }
}
