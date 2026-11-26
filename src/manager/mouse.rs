use super::{Direction, Id, Layout, Manager, Rect, native};
use crate::{hud::DropPreview, layout::DropZone};

#[derive(PartialEq, Eq)]
enum Gesture {
    Pending,
    Move,
    Resize,
}

/// An in-progress native move/size gesture. Preview frames are evaluated
/// against the committed layout so dragging a divider back to its origin can
/// restore the initial proportions even after reaching a size limit.
pub(super) struct MouseDrag {
    pub target: Id,
    tile: Option<Rect>,
    edges: [Option<(Direction, i32)>; 2],
    gesture: Gesture,
    outline_only: bool,
    pointer: Option<(i32, i32)>,
    preview_frame: Option<Rect>,
    hover: Option<(Id, DropZone)>,
    ghost: DropPreview,
}

impl MouseDrag {
    fn new(target: Id, tile: Option<Rect>) -> Self {
        Self {
            target,
            tile,
            edges: [None, None],
            gesture: Gesture::Pending,
            outline_only: false,
            pointer: None,
            preview_frame: None,
            hover: None,
            ghost: DropPreview::default(),
        }
    }

    fn sample(&mut self, frame: Option<Rect>) -> bool {
        let mut edges = [None, None];
        if let Some((tile, frame)) = self.tile.zip(frame) {
            if self.gesture == Gesture::Pending
                && tile != frame
                && (tile.w, tile.h) == (frame.w, frame.h)
            {
                self.gesture = Gesture::Move;
            }
            if self.gesture != Gesture::Move {
                edges = resized_edges(tile, frame);
                if edges != [None, None] {
                    self.gesture = Gesture::Resize;
                }
            }
        }
        std::mem::replace(&mut self.edges, edges) != edges
    }

    fn drop_point(
        &self,
        frame: Option<Rect>,
        cursor: Option<(i32, i32)>,
        finished: bool,
    ) -> Option<(i32, i32)> {
        if self.gesture != Gesture::Move || (self.tile == frame && (finished || !self.outline_only))
        {
            return None;
        }
        // A private desktop can deliver window events but deny cursor access.
        cursor.or_else(|| frame.map(|r| (r.x.saturating_add(r.w / 2), r.y.saturating_add(r.h / 2))))
    }

    pub fn resized_layout(&self, layout: &Layout, area: Rect) -> Option<Layout> {
        if self.edges == [None, None] {
            return None;
        }
        // Always start from the committed topology. Moving back to the starting
        // frame (including a native Esc) must undo even a clamped corner drag.
        let mut preview = layout.clone();
        let mut changed = false;
        for (edge, coordinate) in self.edges.into_iter().flatten() {
            changed |= preview.resize_edge_to(self.target, edge, coordinate, area);
        }
        changed.then_some(preview)
    }
}

impl Manager {
    pub(super) fn start_mouse_drag(&mut self, id: Id) {
        if self.drag_target() == Some(id) {
            return;
        }
        self.finish_mode();
        // Out-of-context START can arrive after the first native move. Read
        // the tile Rustile placed, even if the HWND has reached its new size.
        let tile = self.area.and_then(|area| {
            self.workspace
                .layout
                .placements(area)
                .into_iter()
                .find(|&(other, _)| other == id)
                .map(|(_, rect)| rect)
        });
        let mut drag = MouseDrag::new(id, tile);
        if tile.is_some() && native::outline_caption_drag(id) {
            drag.gesture = Gesture::Move;
            drag.outline_only = true;
            drag.pointer = native::cursor_position();
        }
        self.dragging = Some(drag);
    }

    fn drag_frame(&self, id: Id) -> Option<Rect> {
        if native::is_snapped(id) {
            return None;
        }
        self.windows.get(&id)?.original.visible_frame()
    }

    pub(super) fn preview_mouse_drag(&mut self, id: Id) {
        let frame = self.drag_frame(id);
        let Some(mut drag) = self.dragging.take() else {
            return;
        };
        let resized = drag.sample(frame);
        if drag.gesture == Gesture::Move {
            self.show_drop(&mut drag, frame, native::cursor_position(), false);
        }
        self.dragging = Some(drag);
        if resized {
            self.apply();
        }
    }

    fn destination(&self, drag: &MouseDrag, point: Option<(i32, i32)>) -> Option<(Id, DropZone)> {
        self.workspace
            .layout
            .drop_target(drag.target, point?, self.area?)
    }

    fn drop_plan(&self, source: Id, hover: (Id, DropZone)) -> Option<(Layout, Rect)> {
        let (target, zone) = hover;
        if !self.can_act_on(source) || !self.can_act_on(target) {
            return None;
        }
        self.workspace
            .layout
            .plan_drop(source, target, zone, self.area?)
    }

    fn show_drop(
        &self,
        drag: &mut MouseDrag,
        frame: Option<Rect>,
        cursor: Option<(i32, i32)>,
        force: bool,
    ) {
        drag.pointer = cursor;
        drag.preview_frame = frame;
        let hover = self.destination(drag, drag.drop_point(frame, cursor, false));
        if !force && drag.hover == hover {
            return;
        }
        drag.hover = hover;
        if let Some((_, bounds)) = hover.and_then(|h| self.drop_plan(drag.target, h)) {
            drag.ghost.show(drag.target, bounds, self.settings.outline);
        } else {
            drag.ghost.hide();
        }
    }

    pub fn tracking_drag_cursor(&self) -> bool {
        self.dragging
            .as_ref()
            .is_some_and(|d| d.gesture == Gesture::Move && d.pointer.is_some())
    }

    pub fn cancel_mouse_move(&mut self) {
        if self
            .dragging
            .as_ref()
            .is_some_and(|drag| drag.gesture == Gesture::Move)
        {
            self.dragging = None;
            self.needs_desktop_retry = true;
        }
    }

    pub(super) fn hide_drop_for(&mut self, id: Id) {
        if let Some(drag) = self.dragging.as_mut()
            && (drag.target == id || drag.hover.is_some_and(|(target, _)| target == id))
        {
            drag.ghost.hide();
            drag.hover = None;
        }
    }

    // Window geometry can stop changing at a screen edge while the pointer
    // continues moving. Only sample the cursor during the native move loop.
    pub fn refresh_drop_preview(&mut self, layout_changed: bool) {
        if !self
            .dragging
            .as_ref()
            .is_some_and(|d| d.gesture == Gesture::Move)
        {
            return;
        }
        let cursor = native::cursor_position();
        let mut drag = self.dragging.take().unwrap();
        if layout_changed || cursor != drag.pointer {
            let frame = self.drag_frame(drag.target);
            self.show_drop(&mut drag, frame, cursor, layout_changed);
        }
        self.dragging = Some(drag);
    }

    pub(super) fn finish_mouse_drag(&mut self, mut drag: MouseDrag) {
        let frame = self.drag_frame(drag.target);
        drag.sample(frame);
        let next = if drag.gesture == Gesture::Move {
            drag.ghost.hide();
            // Revalidate against the live desktop. A window may have closed
            // or a panel may have changed the work area since the last sample.
            // END is delivered asynchronously. If the HWND is still at the
            // previewed frame, do not chase the pointer after button-up.
            let cursor = if !drag.outline_only && frame.is_some() && frame == drag.preview_frame {
                drag.pointer
            } else {
                native::cursor_position()
            };
            let point = drag.drop_point(frame, cursor, true);
            self.destination(&drag, point)
                .and_then(|h| self.drop_plan(drag.target, h))
                .map(|(layout, _)| layout)
        } else {
            self.area
                .and_then(|area| drag.resized_layout(&self.workspace.layout, area))
        };
        if let Some(layout) = next {
            self.workspace.layout = layout;
        }
        if let Some(window) = self.windows.get_mut(&drag.target) {
            // The HWND has already followed the mouse. Place it again only
            // if a minimum size, the work area or Windows Snap requires it.
            window.requested = frame;
            window.repairs = 0;
        }
        self.apply();
    }
}

fn resized_edges(tile: Rect, frame: Rect) -> [Option<(Direction, i32)>; 2] {
    // Windows uses the same move/size events for a caption drag and a border
    // drag. Only a size change with a stationary opposite edge moves a divider.
    let edge = |old: (i32, i32), new: (i32, i32), leading, trailing| {
        if old.1 == new.1 {
            return None;
        }
        let old_end = old.0.saturating_add(old.1);
        let new_end = new.0.saturating_add(new.1);
        if new.0 == old.0 {
            Some((trailing, new_end))
        } else if new_end == old_end {
            Some((leading, new.0))
        } else {
            None
        }
    };
    [
        edge(
            (tile.x, tile.w),
            (frame.x, frame.w),
            Direction::Left,
            Direction::Right,
        ),
        edge(
            (tile.y, tile.h),
            (frame.y, frame.h),
            Direction::Up,
            Direction::Down,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_drop_follows_the_pointer_and_ignores_a_cancelled_move() {
        let tile = Rect {
            x: -640,
            y: 10,
            w: 600,
            h: 400,
        };
        let moved = Some(Rect { x: 10, ..tile });
        let pointer = Some((25, 22));
        let mut drag = MouseDrag::new(1, Some(tile));
        drag.sample(moved);
        assert_eq!(drag.drop_point(moved, pointer, true), pointer);
        assert_eq!(drag.drop_point(moved, None, true), Some((310, 210)));
        assert_eq!(drag.drop_point(Some(tile), pointer, true), None);

        // When Windows draws only an outline, the HWND stays at the origin
        // while dragging. At END the same frame means the move was cancelled.
        drag.outline_only = true;
        assert_eq!(drag.drop_point(Some(tile), pointer, false), pointer);
        assert_eq!(drag.drop_point(Some(tile), pointer, true), None);
    }

    #[test]
    fn move_size_samples_distinguish_translation_corner_resize_and_cancel() {
        let tile = Rect {
            x: -119,
            y: 37,
            w: 602,
            h: 402,
        };
        let mut drag = MouseDrag::new(1, Some(tile));
        assert!(!drag.sample(Some(Rect {
            x: -124,
            y: 40,
            ..tile
        })));
        assert!(drag.gesture == Gesture::Move);
        let mut drag = MouseDrag::new(1, Some(tile));
        let corner = Rect {
            x: -104,
            y: 37,
            w: 587,
            h: 430,
        };
        assert!(drag.sample(Some(corner)));
        assert_eq!(
            drag.edges,
            [Some((Direction::Left, -104)), Some((Direction::Down, 467))]
        );
        assert!(!drag.sample(Some(corner)));
        assert!(drag.sample(Some(tile)));
        assert_eq!(drag.edges, [None, None]);
    }
}
