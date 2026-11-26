use std::ptr::{null, null_mut};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
};
use windows_sys::core::{PCWSTR, w};

use crate::layout::{Direction, Id, Rect};
use crate::native;
use crate::settings::Settings;
use crate::tray::Colors;

mod guide;
mod outline;
mod panel;
#[cfg(test)]
mod panel_tests;
use guide::Guide;
pub use outline::{DropPreview, FocusOutline};
use panel::Panel;

#[derive(Default)]
pub struct ResizeHud {
    label: HWND,
    edge: HWND,
    hint: Option<Hint>,
}

// Windows retains the uploaded bitmap. Repeated arrows only move the popup.
struct Hint {
    settings: Settings,
    dpi: u32,
    work: Rect,
    tile: Rect,
    width: i32,
    height: i32,
}

impl Hint {
    fn bounds(&self) -> Rect {
        let center = |start: i32, span: i32, size: i32, low: i32, available: i32| {
            (start + (span - size) / 2).clamp(low, low + (available - size).max(0))
        };
        Rect {
            x: center(
                self.tile.x,
                self.tile.w,
                self.width,
                self.work.x,
                self.work.w,
            ),
            y: center(
                self.tile.y,
                self.tile.h,
                self.height,
                self.work.y,
                self.work.h,
            ),
            w: self.width,
            h: self.height,
        }
    }
}

impl ResizeHud {
    fn show_guide(&mut self, tile: Rect, settings: &Settings) {
        if self.label.is_null() {
            self.label = popup(w!("Rustile.ResizeHint"), Some(guide_proc), tile);
            if self.label.is_null() {
                return;
            }
        }
        let dpi = unsafe { GetDpiForWindow(self.label) }.max(96);
        if let Some(hint) = &mut self.hint
            && hint.dpi == dpi
            && hint.settings == *settings
        {
            if hint.tile != tile {
                hint.tile = tile;
                position(self.label, hint.bounds());
            }
            return;
        }

        let work = native::work_area().unwrap_or(tile);
        let guide = Guide::new(settings);
        let colors = Colors::current();
        let Some(panel) = Panel::render(&guide, dpi, work, colors, colors.allows_transparency())
        else {
            return;
        };
        let hint = Hint {
            settings: settings.clone(),
            dpi,
            work,
            tile,
            width: panel.width,
            height: panel.height,
        };
        let bounds = hint.bounds();
        if panel.present(self.label, bounds.x, bounds.y) {
            let title: Vec<_> = guide.accessible_text().encode_utf16().chain([0]).collect();
            unsafe { SetWindowTextW(self.label, title.as_ptr()) };
            self.hint = Some(hint);
            position(self.label, bounds);
        }
    }

    pub(super) fn refresh_appearance(&mut self, window: isize) {
        if window == self.label as isize
            && let Some(hint) = self.hint.take()
        {
            self.show_guide(hint.tile, &hint.settings);
        }
    }

    pub fn show(&mut self, target: Id, tile: Rect, edge: Option<Direction>, settings: &Settings) {
        self.show_guide(tile, settings);
        let Some(edge) = edge else {
            if !self.edge.is_null() {
                unsafe { ShowWindow(self.edge, SW_HIDE) };
            }
            return;
        };
        if self.edge.is_null() {
            self.edge = popup(w!("Rustile.ResizeEdge"), Some(edge_proc), tile);
            if self.edge.is_null() {
                return;
            }
            unsafe { SetLayeredWindowAttributes(self.edge, 0, 255, LWA_ALPHA) };
        }
        let dpi = unsafe { GetDpiForWindow(self.edge) }.max(96) as i32;
        let scale = |n: i32| (n * dpi + 48) / 96;
        let line = scale(2);
        // The movable edge meets the curved focus stroke at its tangents.
        let radius = scale(native::corner_radius_dip(target)).min(tile.w.min(tile.h) / 2);
        let (x, y, w, h) = match edge {
            Direction::Left => (tile.x, tile.y + radius, line, tile.h - 2 * radius),
            Direction::Right => (
                tile.x + tile.w - line,
                tile.y + radius,
                line,
                tile.h - 2 * radius,
            ),
            Direction::Up => (tile.x + radius, tile.y, tile.w - 2 * radius, line),
            Direction::Down => (
                tile.x + radius,
                tile.y + tile.h - line,
                tile.w - 2 * radius,
                line,
            ),
        };
        position(self.edge, Rect { x, y, w, h });
    }

    pub fn hide(&mut self) {
        self.hint = None;
        for window in [&mut self.label, &mut self.edge] {
            if !window.is_null() {
                unsafe { DestroyWindow(*window) };
                *window = null_mut();
            }
        }
    }
}

fn popup(name: PCWSTR, proc: WNDPROC, tile: Rect) -> HWND {
    unsafe {
        let class = WNDCLASSW {
            hInstance: GetModuleHandleW(null()),
            lpszClassName: name,
            lpfnWndProc: proc,
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            name,
            w!(""),
            WS_POPUP | WS_DISABLED,
            tile.x,
            tile.y,
            0,
            0,
            null_mut(),
            null_mut(),
            class.hInstance,
            null(),
        )
    }
}

fn position(hwnd: HWND, rect: Rect) {
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

unsafe extern "system" fn guide_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_THEMECHANGED | WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_DPICHANGED => {
            unsafe {
                PostThreadMessageW(
                    GetCurrentThreadId(),
                    crate::runtime::RESIZE_HINT_CHANGED,
                    0,
                    hwnd as isize,
                );
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe extern "system" fn edge_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            SetDCBrushColor(dc, outline::resize_edge_color());
            FillRect(dc, &paint.rcPaint, GetStockObject(DC_BRUSH));
            EndPaint(hwnd, &paint);
            0
        },
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            unsafe { InvalidateRect(hwnd, null(), 0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
