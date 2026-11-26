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

    pub(super) fn pages(
        &self,
        sections: &[Section],
        transfer: &Section,
        bottom: i32,
    ) -> Vec<Vec<Placement>> {
        let mut pages = vec![Vec::new()];
        let mut cursors = [(0, self.top); 3];
        for (section, column) in sections.iter().zip(self.section_columns(sections, bottom)) {
            let cursor = &mut cursors[column];
            if cursor.1 > self.top {
                cursor.1 += self.section_gap;
            }
            let x = self.column_x(column);
            self.place_actions(
                &mut pages,
                cursor,
                &section.actions,
                CellPlacement::new(section.heading, x, self.col),
                x,
                bottom,
            );
        }

        let lanes = self.transfer_lanes(&transfer.actions);
        let mut page = pages.len() - 1;
        let mut y = pages[page]
            .iter()
            .map(|p| p.y + p.height)
            .max()
            .map_or(self.top, |last_edge| last_edge + 2 * self.section_gap);
        let rows = lanes
            .iter()
            .map(|actions| {
                actions
                    .iter()
                    .map(|action| action.lines.len())
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        let height = self.heading + rows as i32 * self.row;
        if y > self.top && y + height > bottom && self.top + height <= bottom {
            page += 1;
            y = self.top;
        }
        let width = self.column_x(self.columns - 1) + self.col - self.margin;
        let heading = CellPlacement::new(transfer.heading, self.margin, width);
        for (column, actions) in lanes[..self.columns].iter().enumerate() {
            let mut cursor = (page, y);
            self.place_actions(
                &mut pages,
                &mut cursor,
                actions,
                heading,
                self.column_x(column),
                bottom,
            );
        }
        pages
    }

    fn column_x(&self, column: usize) -> i32 {
        self.margin + column as i32 * (self.gap + self.col)
    }

    fn transfer_lanes<'a>(&self, actions: &'a [ActionLines]) -> [&'a [ActionLines]; 3] {
        match self.columns {
            1 => [actions, &[], &[]],
            2 => {
                let (first, second) = actions.split_at(actions.len().div_ceil(2));
                [first, second, &[]]
            }
            _ => {
                // Previous/next sit together, followed by the two numeric ranges.
                let (neighbors, numbers) = actions.split_at(2.min(actions.len()));
                let (first, second) = numbers.split_at(numbers.len().div_ceil(2));
                [neighbors, first, second]
            }
        }
    }

    fn place_actions(
        &self,
        pages: &mut Vec<Vec<Placement>>,
        (page, y): &mut (usize, i32),
        actions: &[ActionLines],
        heading: CellPlacement,
        x: i32,
        bottom: i32,
    ) {
        let mut heading_needed = true;
        for action in actions {
            let lines = action.lines.len() as i32;
            let fresh_height = bottom - self.top - self.heading;
            // Keep ordinary actions together. Repeat the label on a new page
            // when one action alone has more alternatives than fit on a page.
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
                    if !items.iter().any(|item| item.hwnd == heading.hwnd) {
                        items.push(Placement {
                            hwnd: heading.hwnd,
                            x: heading.x,
                            y: *y,
                            width: heading.width,
                            height: self.heading,
                        });
                    }
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

    fn grid(columns: usize) -> Grid {
        Grid {
            columns,
            margin: 14,
            col: 380,
            gap: 20,
            top: 6,
            row: 24,
            heading: 26,
            section_gap: 8,
        }
    }

    #[test]
    fn an_action_with_many_bindings_preserves_every_binding_and_repeats_its_label() {
        let grid = grid(1);
        let header = [1, 2, 3].map(|id| CellPlacement::new(id as HWND, 0, 40));
        let mut lines = vec![Vec::from(header)];
        lines.extend((5..72).map(|id| vec![CellPlacement::new(id as HWND, 0, 120)]));
        let transfer = Section {
            heading: 4 as HWND,
            topic: 0,
            actions: vec![ActionLines { header, lines }],
        };
        let pages = grid.pages(&[], &transfer, 182);
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

    fn section(heading: usize, topic: usize, actions: usize) -> Section {
        Section {
            heading: heading as HWND,
            topic,
            actions: (1..=actions)
                .map(|n| {
                    let first = (heading + n) * 10;
                    let header = [0, 1, 2]
                        .map(|i| CellPlacement::new((first + i) as HWND, 40 * i as i32, 40));
                    ActionLines {
                        header,
                        lines: vec![Vec::from(header)],
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn transfer_heading_spans_the_grid_below_all_three_topics() {
        let grid = grid(3);
        let upper = [section(100, 0, 2), section(200, 1, 1), section(300, 2, 4)];
        let transfer = section(400, 1, 12);
        let pages = grid.pages(&upper, &transfer, 500);
        assert_eq!(pages.len(), 1);
        let items = &pages[0];
        let heading = items.iter().find(|p| p.hwnd == transfer.heading).unwrap();
        let upper_bottom = grid.top + grid.heading + 4 * grid.row;
        assert_eq!(heading.y, upper_bottom + 2 * grid.section_gap);
        assert_eq!(heading.x, grid.margin);
        assert_eq!(heading.width, 3 * grid.col + 2 * grid.gap);
        for (n, action) in transfer.actions.iter().enumerate() {
            let (column, row) = match n {
                0..2 => (0, n),
                2..7 => (1, n - 2),
                _ => (2, n - 7),
            };
            let label = items
                .iter()
                .find(|p| p.hwnd == action.header[0].hwnd)
                .unwrap();
            assert_eq!(
                (label.x, label.y),
                (
                    grid.column_x(column),
                    heading.y + grid.heading + row as i32 * grid.row
                )
            );
        }
    }

    #[test]
    fn divided_transfer_section_has_one_heading_per_page_and_no_duplicate_actions() {
        let upper = [section(100, 0, 6), section(200, 1, 7), section(300, 2, 3)];
        let transfer = section(400, 0, 12);
        let expected = transfer
            .actions
            .iter()
            .map(|action| action.header[0].hwnd as usize)
            .collect::<Vec<_>>();
        for columns in 1..=3 {
            let grid = grid(columns);
            let bottom = 110;
            let mut placed = Vec::new();
            let mut transfer_started = false;
            for page in grid.pages(&upper, &transfer, bottom) {
                assert!(page.iter().all(|p| p.y + p.height <= bottom));
                let rows: Vec<_> = page
                    .iter()
                    .filter(|p| expected.contains(&(p.hwnd as usize)))
                    .collect();
                let headings: Vec<_> = page.iter().filter(|p| p.hwnd == transfer.heading).collect();
                assert_eq!(headings.len(), usize::from(!rows.is_empty()));
                if let Some(heading) = headings.first() {
                    assert!(rows.iter().all(|p| p.y >= heading.y + heading.height));
                    transfer_started = true;
                } else {
                    assert!(!transfer_started);
                }
                placed.extend(rows.iter().map(|p| p.hwnd as usize));
            }
            placed.sort_unstable();
            assert_eq!(placed, expected);
        }
    }
}
