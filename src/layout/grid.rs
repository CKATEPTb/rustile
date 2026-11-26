//! Pack the root tiles into bands before applying local BSP cuts. A band can
//! have a different number of occupants from its neighbor, so an incomplete
//! final band can still cover its full share of the work area.

use super::{Axis, Direction, GAP, Rect};

#[derive(Clone, Copy, Debug)]
pub(super) struct Shape {
    flow: Axis,
    bands: usize,
    count: usize,
}

impl Shape {
    pub fn candidates(count: usize, area: Rect, gap: i32) -> Vec<Self> {
        let flow = if area.w >= area.h { Axis::X } else { Axis::Y };
        if count == 0 {
            return vec![Self {
                flow,
                bands: 0,
                count,
            }];
        }
        let capacity = |axis: Axis| {
            ((i64::from(axis.length(area).max(0)) + i64::from(gap))
                / i64::from(axis.minimum() + gap)) as usize
        };
        let along = capacity(flow);
        let across = capacity(flow.perpendicular());
        if count > along.saturating_mul(across) {
            return Vec::new();
        }
        let side = count.isqrt();
        let ideal = if side * side == count { side } else { side + 1 };
        let preferred = count.div_ceil(ideal);
        let mut shapes: Vec<_> = (count.div_ceil(along)..=across.min(count))
            .map(|bands| Self { flow, bands, count })
            .collect();
        shapes.sort_by_key(|shape| shape.bands.abs_diff(preferred));
        shapes
    }

    fn band_len(self, band: usize) -> usize {
        self.count / self.bands + usize::from(band < self.count % self.bands)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Grid {
    pub(super) gap: i32,
    flow: Axis,
    bands: Vec<i32>,
    segments: Vec<Vec<i32>>,
}

impl Default for Grid {
    fn default() -> Self {
        Self::new(GAP)
    }
}

impl Grid {
    pub fn new(gap: i32) -> Self {
        Self {
            gap,
            flow: Axis::X,
            bands: Vec::new(),
            segments: Vec::new(),
        }
    }

    pub fn set_shape(&mut self, shape: Shape) {
        let rotated = self.flow != shape.flow;
        self.flow = shape.flow;
        if rotated || self.bands.len() != shape.bands {
            self.bands = vec![1; shape.bands];
        }
        self.segments.resize_with(shape.bands, Vec::new);
        for (i, weights) in self.segments.iter_mut().enumerate() {
            if rotated || weights.len() != shape.band_len(i) {
                *weights = vec![1; shape.band_len(i)];
            }
        }
    }

    fn tracks(&self, slot: usize) -> Option<(usize, usize)> {
        let mut offset = slot;
        for (band, segments) in self.segments.iter().enumerate() {
            if offset < segments.len() {
                return Some((band, offset));
            }
            offset -= segments.len();
        }
        None
    }

    pub fn remove_slot(&mut self, slot: usize) {
        let (band, segment) = self.tracks(slot).expect("occupied track");
        self.segments[band].remove(segment);
        if self.segments[band].is_empty() {
            self.segments.remove(band);
            self.bands.remove(band);
        }
    }

    fn band_minima(&self, minima: &[(i32, i32)]) -> Vec<i32> {
        let mut offset = 0;
        self.segments
            .iter()
            .map(|row| {
                let next = offset + row.len();
                let minimum = minima[offset..next]
                    .iter()
                    .map(|&m| self.flow.perpendicular().min_dimension(m))
                    .max()
                    .unwrap_or(0);
                offset = next;
                minimum
            })
            .collect()
    }

    pub fn fit(&mut self, area: Rect, minima: &[(i32, i32)]) -> bool {
        let gap = self.gap;
        self.fit_with(minima, |weights, axis, mins| {
            lengths(weights, axis.length(area), mins, gap)
        })
    }

    pub fn refit(&mut self, area: Rect, minima: &[(i32, i32)]) -> bool {
        let gap = self.gap;
        self.fit_with(minima, |weights, axis, mins| {
            keep_dividers(weights, axis.length(area), mins, gap)
        })
    }

    // During a display or panel change, rounding a projected size to pixels
    // must not turn it into a new ratio. Keep each band's and row's weights
    // unless a native minimum actually requires displacing a divider.
    pub fn fit_spacing(&mut self, area: Rect, minima: &[(i32, i32)]) -> bool {
        let gap = self.gap;
        self.fit_with(minima, |weights, axis, mins| {
            let length = axis.length(area);
            let floors = vec![axis.minimum(); weights.len()];
            let projected = lengths(weights, length, &floors, gap)?;
            if projected.iter().zip(mins).all(|(size, min)| size >= min) {
                Some(weights.to_vec())
            } else {
                lengths(weights, length, mins, gap)
            }
        })
    }

    fn fit_with(
        &mut self,
        minima: &[(i32, i32)],
        distribute: impl Fn(&[i32], Axis, &[i32]) -> Option<Vec<i32>>,
    ) -> bool {
        if self.bands.is_empty() {
            return true;
        }
        let across = self.flow.perpendicular();
        let Some(bands) = distribute(&self.bands, across, &self.band_minima(minima)) else {
            return false;
        };
        let mut offset = 0;
        for weights in &mut self.segments {
            let next = offset + weights.len();
            let min: Vec<_> = minima[offset..next]
                .iter()
                .map(|&m| self.flow.min_dimension(m))
                .collect();
            let Some(sizes) = distribute(weights, self.flow, &min) else {
                return false;
            };
            *weights = sizes;
            offset = next;
        }
        self.bands = bands;
        true
    }

    pub fn rects(&self, area: Rect) -> Vec<Rect> {
        let across = self.flow.perpendicular();
        let bands = spans(&self.bands, across.length(area), across.minimum(), self.gap);
        let mut result = Vec::new();
        for ((band_at, band_length), weights) in bands.into_iter().zip(&self.segments) {
            for (segment_at, segment_length) in spans(
                weights,
                self.flow.length(area),
                self.flow.minimum(),
                self.gap,
            ) {
                let (dx, dy, w, h) = match self.flow {
                    Axis::X => (segment_at, band_at, segment_length, band_length),
                    Axis::Y => (band_at, segment_at, band_length, segment_length),
                };
                result.push(Rect {
                    x: area.x.saturating_add(dx),
                    y: area.y.saturating_add(dy),
                    w,
                    h,
                });
            }
        }
        result
    }

    pub fn divider_scope(
        &self,
        slot: usize,
        edge: Direction,
        area: Rect,
        roots: &[Rect],
    ) -> Option<Rect> {
        let (band, segment) = self.tracks(slot)?;
        let (track, count, scope) = if edge.axis() == self.flow {
            (segment, self.segments[band].len(), roots[slot])
        } else {
            (band, self.bands.len(), area)
        };
        let adjacent = if edge.towards_start() {
            track > 0
        } else {
            track + 1 < count
        };
        adjacent.then_some(scope)
    }

    // Pixel lengths are exact weights at the current work area. Other bands
    // keep their original ratios, even if their current projection is rounded.
    pub fn update_dividers(&mut self, old: &[Rect], new: &[Rect]) {
        let mut offset = 0;
        let across = self.flow.perpendicular();
        let mut heights = Vec::with_capacity(self.bands.len());
        let mut changed = false;
        for row in &mut self.segments {
            let end = offset + row.len();
            let before = &old[offset..end];
            let after = &new[offset..end];
            if before
                .iter()
                .zip(after)
                .any(|(&a, &b)| self.flow.length(a) != self.flow.length(b))
            {
                for (weight, &rect) in row.iter_mut().zip(after) {
                    *weight = self.flow.length(rect);
                }
            }
            let thickness = across.length(after[0]);
            changed |= across.length(before[0]) != thickness;
            heights.push(thickness);
            offset = end;
        }
        if changed {
            self.bands = heights;
        }
    }
}

// Clamp existing divider positions to the space needed on either side. A
// change propagates past a neighboring track only when it runs out of room.
fn keep_dividers(weights: &[i32], length: i32, minima: &[i32], gap: i32) -> Option<Vec<i32>> {
    let mut sizes = lengths(weights, length, &vec![0; weights.len()], gap)?;
    let usable: i64 = sizes.iter().map(|&size| i64::from(size)).sum();
    let mut reserved: i64 = minima.iter().map(|&min| i64::from(min)).sum();
    if reserved > usable {
        return None;
    }
    let (mut old_end, mut start) = (0, 0);
    for (size, &min) in sizes.iter_mut().zip(minima) {
        old_end += i64::from(*size);
        reserved -= i64::from(min);
        let end = old_end.clamp(start + i64::from(min), usable - reserved);
        *size = (end - start) as i32;
        start = end;
    }
    Some(sizes)
}

fn lengths(weights: &[i32], length: i32, minima: &[i32], gap: i32) -> Option<Vec<i32>> {
    if weights.is_empty() {
        return Some(Vec::new());
    }
    let mut remaining = i64::from(length) - i64::from(gap) * (weights.len() - 1) as i64;
    let mut reserved: i64 = minima.iter().map(|&m| i64::from(m)).sum();
    if remaining < reserved {
        return None;
    }
    let mut total: i64 = weights.iter().map(|&w| i64::from(w)).sum();
    let mut sizes = Vec::with_capacity(weights.len());
    for (&w, &m) in weights.iter().zip(minima) {
        reserved -= i64::from(m);
        let ideal = (remaining * i64::from(w) + total / 2) / total;
        let size = ideal.clamp(i64::from(m), remaining - reserved);
        sizes.push(size as i32);
        remaining -= size;
        total -= i64::from(w);
    }
    Some(sizes)
}

fn spans(weights: &[i32], length: i32, min: i32, gap: i32) -> Vec<(i32, i32)> {
    if weights.is_empty() {
        return Vec::new();
    }
    let min = vec![min; weights.len()];
    if let Some(sizes) = lengths(weights, length, &min, gap) {
        let mut pos = 0i32;
        return sizes
            .into_iter()
            .map(|size| {
                let start = pos;
                pos = pos.saturating_add(size).saturating_add(gap);
                (start, size)
            })
            .collect();
    }
    // A display can become smaller before the next admission/reflow. Keep all
    // frames inside it, shrinking the gutters too if necessary.
    let gap = gap.min(length.max(0) / (weights.len().saturating_sub(1).max(1) as i32));
    let usable = (length.max(0) - gap * (weights.len() - 1) as i32) as i64;
    let count = weights.len() as i64;
    (0..count)
        .map(|i| {
            (
                (usable * i / count + i * i64::from(gap)) as i32,
                (usable * (i + 1) / count - usable * i / count) as i32,
            )
        })
        .collect()
}
