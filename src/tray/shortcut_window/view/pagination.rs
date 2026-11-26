use super::HWND;

#[derive(Clone, Copy)]
pub(super) struct CellPlacement {
    hwnd: HWND,
    x: i32,
    width: i32,
}

impl CellPlacement {
    pub(super) fn new(hwnd: HWND, x: i32, width: i32) -> Self {
        Self { hwnd, x, width }
    }
}

pub(super) struct ActionLines {
    pub(super) header: [CellPlacement; 3],
    pub(super) lines: Vec<Vec<CellPlacement>>,
}

pub(super) struct Section {
    pub(super) heading: HWND,
    pub(super) topic: usize,
    pub(super) actions: Vec<ActionLines>,
}

pub(super) struct Placement {
    pub(super) hwnd: HWND,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: i32,
    pub(super) height: i32,
}

pub(super) struct Grid {
    pub(super) columns: usize,
    pub(super) margin: i32,
    pub(super) col: i32,
    pub(super) gap: i32,
    pub(super) top: i32,
    pub(super) row: i32,
    pub(super) heading: i32,
    pub(super) section_gap: i32,
}

impl Grid {
    fn section_columns(&self, sections: &[Section], bottom: i32) -> Vec<usize> {
        if self.columns == 1 {
            return vec![0; sections.len()];
        }
        let heights: Vec<_> = sections
            .iter()
            .map(|s| {
                self.section_gap
                    + self.heading
                    + s.actions
                        .iter()
                        .map(|a| a.lines.len() as i32 * self.row)
                        .sum::<i32>()
            })
            .collect();
        if self.columns == 2 {
            let total: i32 = heights.iter().sum();
            let split = heights
                .into_iter()
                .scan(0, |left, height| {
                    *left += height;
                    Some((total - 2 * *left).abs())
                })
                .enumerate()
                .min_by_key(|(_, difference)| *difference)
                .map_or(0, |(i, _)| i + 1);
            return (0..sections.len())
                .map(|i| usize::from(i >= split))
                .collect();
        }
        let topics: Vec<_> = sections.iter().map(|s| s.topic).collect();
        let mut totals = [0; 3];
        for (&column, &height) in topics.iter().zip(&heights) {
            totals[column] += height;
        }
        let capacity = bottom - self.top + self.section_gap;
        if totals.iter().all(|&h| h <= capacity) {
            return topics;
        }
        // Before adding a page on a short, wide display, try moving complete
        // sections to the next column in reading order.
        let (mut column, mut height) = (0, 0);
        let mut fitted = Vec::with_capacity(sections.len());
        for h in heights {
            if height + h > capacity {
                column += 1;
                height = 0;
            }
            if h > capacity || column >= self.columns {
                return topics;
            }
            fitted.push(column);
            height += h;
        }
        fitted
    }

    pub(super) fn pages(&self, sections: &[Section], bottom: i32) -> Vec<Vec<Placement>> {
        let mut pages = vec![Vec::new()];
        let mut cursors = [(0, self.top); 3];
        for (section, column) in sections.iter().zip(self.section_columns(sections, bottom)) {
            let (page, y) = &mut cursors[column];
            let x = self.margin + column as i32 * (self.gap + self.col);
            if *y > self.top {
                *y += self.section_gap;
            }
            let mut heading_needed = true;
            for action in &section.actions {
                let lines = action.lines.len() as i32;
                let fresh_height = bottom - self.top - self.heading;
                // Keep a normal action on one page; an unusually large list of
                // alternatives can continue on as many pages as it needs.
                let keep = if lines * self.row <= fresh_height {
                    lines
                } else {
                    lines.min(2)
                };
                if *y > self.top
                    && *y + i32::from(heading_needed) * self.heading + keep * self.row > bottom
                {
                    *page += 1;
                    *y = self.top;
                    heading_needed = true;
                }
                for (line_no, line) in action.lines.iter().enumerate() {
                    if line_no > 0 && *y + self.row > bottom {
                        *page += 1;
                        *y = self.top;
                        heading_needed = true;
                    }
                    if pages.len() <= *page {
                        pages.resize_with(*page + 1, Vec::new);
                    }
                    let items = &mut pages[*page];
                    if heading_needed {
                        items.push(Placement {
                            hwnd: section.heading,
                            x,
                            y: *y,
                            width: self.col,
                            height: self.heading,
                        });
                        *y += self.heading;
                        if line_no > 0 {
                            self.line(items, &action.header, x, *y);
                            *y += self.row;
                        }
                        heading_needed = false;
                    }
                    self.line(items, line, x, *y);
                    *y += self.row;
                }
            }
        }
        pages
    }

    fn line(&self, items: &mut Vec<Placement>, cells: &[CellPlacement], x: i32, y: i32) {
        items.extend(cells.iter().map(|c| Placement {
            hwnd: c.hwnd,
            x: x + c.x,
            y,
            width: c.width,
            height: self.row,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_with_many_bindings_preserves_every_binding_and_repeats_its_label() {
        let grid = Grid {
            columns: 1,
            margin: 14,
            col: 380,
            gap: 20,
            top: 6,
            row: 24,
            heading: 26,
            section_gap: 8,
        };
        let header = [1, 2, 3].map(|id| CellPlacement::new(id as HWND, 0, 40));
        let mut lines = vec![Vec::from(header)];
        lines.extend((5..72).map(|id| vec![CellPlacement::new(id as HWND, 0, 120)]));
        let sections = [Section {
            heading: 4 as HWND,
            topic: 0,
            actions: vec![ActionLines { header, lines }],
        }];
        let pages = grid.pages(&sections, 182);
        assert!(pages.len() > 2);
        let mut bindings = Vec::new();
        for page in pages {
            assert!(page.iter().all(|p| p.y + p.height <= 182));
            for id in 1..=4 {
                assert_eq!(page.iter().filter(|p| p.hwnd == id as HWND).count(), 1);
            }
            bindings.extend(page.iter().map(|p| p.hwnd as usize).filter(|&id| id >= 5));
        }
        assert_eq!(bindings, (5..72).collect::<Vec<_>>());
    }
}
