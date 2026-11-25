use super::{Direction, Id, Layout, Manager, Rect, native};

pub(super) struct MouseDrag {
    pub target: Id,
    tile: Option<Rect>,
    edges: [Option<(Direction, i32)>; 2],
}

impl MouseDrag {
    fn new(target: Id, tile: Option<Rect>) -> Self {
        Self {
            target,
            tile,
            edges: [None, None],
        }
    }

    fn sample(&mut self, frame: Option<Rect>) -> bool {
        let edges = self
            .tile
            .zip(frame)
            .map_or([None, None], |(tile, frame)| resized_edges(tile, frame));
        std::mem::replace(&mut self.edges, edges) != edges
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
        self.finish_mode(true);
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
        self.dragging = Some(MouseDrag::new(id, tile));
    }

    fn drag_frame(&self, id: Id) -> Option<Rect> {
        if native::is_snapped(id) {
            return None;
        }
        self.windows.get(&id)?.original.visible_frame()
    }

    pub(super) fn preview_mouse_drag(&mut self, id: Id) {
        let frame = self.drag_frame(id);
        if let Some(drag) = self.dragging.as_mut()
            && drag.sample(frame)
        {
            self.apply();
        }
    }

    pub(super) fn finish_mouse_drag(&mut self, mut drag: MouseDrag) {
        let frame = self.drag_frame(drag.target);
        drag.sample(frame);
        if let Some(area) = self.area
            && let Some(layout) = drag.resized_layout(&self.workspace.layout, area)
        {
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
