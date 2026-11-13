use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use image::RgbaImage;

use crate::PixelRect;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Shift {
    pub axis: Axis,
    pub area: PixelRect,
    // Content at a viewport position came from this many pixels further
    // along the preceding viewport. Scrolling down therefore has delta > 0.
    pub delta: i32,
}

impl Axis {
    pub fn longitudinal(self, width: u32, height: u32) -> u32 {
        match self {
            Self::Vertical => height,
            Self::Horizontal => width,
        }
    }

    pub fn lateral(self, width: u32, height: u32) -> u32 {
        self.longitudinal(height, width)
    }

    pub fn xy<T>(self, along: T, across: T) -> (T, T) {
        match self {
            Self::Vertical => (across, along),
            Self::Horizontal => (along, across),
        }
    }

    pub fn rect(self, along: u32, across: u32, length: u32, breadth: u32) -> PixelRect {
        let (x, y) = self.xy(along, across);
        let (width, height) = self.xy(length, breadth);
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }
}

impl Shift {
    pub fn residual(self, before: &RgbaImage, after: &RgbaImage) -> Option<PixelRect> {
        let first = self.axis.longitudinal(self.area.x, self.area.y);
        let end = first + self.axis.longitudinal(self.area.width, self.area.height);
        let side = self.axis.lateral(self.area.x, self.area.y);
        let side_end = side + self.axis.lateral(self.area.width, self.area.height);
        super::observation::difference_bounds(after, |x, y| {
            let along = self.axis.longitudinal(x, y);
            let across = self.axis.lateral(x, y);
            if (first..end).contains(&along) && (side..side_end).contains(&across) {
                let source = along as i32 + self.delta;
                if (first as i32..end as i32).contains(&source) {
                    let (x, y) = self.axis.xy(source as u32, across);
                    Some(before.get_pixel(x, y))
                } else {
                    // This strip has just entered the viewport and belongs in the mosaic.
                    None
                }
            } else {
                Some(before.get_pixel(x, y))
            }
        })
    }
}

pub(super) fn detect(before: &RgbaImage, after: &RgbaImage, changed: PixelRect) -> Option<Shift> {
    [Axis::Vertical, Axis::Horizontal]
        .into_iter()
        .filter_map(|axis| detect_axis(axis, before, after, changed))
        .max_by_key(|(matched, shift)| (*matched, shift.area.area()))
        .map(|(_, shift)| shift)
}

fn detect_axis(
    axis: Axis,
    before: &RgbaImage,
    after: &RgbaImage,
    changed: PixelRect,
) -> Option<(u32, Shift)> {
    let length = axis.longitudinal(before.width(), before.height());
    let breadth = axis.lateral(before.width(), before.height());
    let (start, end) = {
        let start = axis.lateral(changed.x, changed.y);
        (start, start + axis.lateral(changed.width, changed.height))
    };
    if length < 16 || end - start < 12 {
        return None;
    }
    let from = fingerprints(axis, before, length, start, end);
    let to = fingerprints(axis, after, length, start, end);

    // Repeated flat rows convey no displacement. Unique lines vote for a
    // translation, which must also explain the overlapping viewport below.
    let mut previous = HashMap::new();
    for (row, fingerprint) in from.iter().enumerate() {
        previous
            .entry(fingerprint)
            .and_modify(|entry| *entry = None)
            .or_insert(Some(row as i32));
    }
    let mut votes = HashMap::<i32, u32>::new();
    for (row, fingerprint) in to.iter().enumerate() {
        if let Some(Some(source)) = previous.get(fingerprint) {
            let delta = source - row as i32;
            if (2..=length * 2 / 3).contains(&delta.unsigned_abs()) {
                *votes.entry(delta).or_default() += 1;
            }
        }
    }
    let (start, end) = {
        let start = axis.longitudinal(changed.x, changed.y);
        (
            start,
            start + axis.longitudinal(changed.width, changed.height),
        )
    };

    // Compositor antialiasing and fractional scrolling can change pixel
    // values along translated edges. Vote with exact lines, then verify
    // candidate displacements against sparsely sampled RGB differences.
    let mut candidates: Vec<_> = votes.into_iter().collect();
    candidates.sort_unstable_by_key(|&(delta, votes)| {
        (std::cmp::Reverse(votes), delta.unsigned_abs(), delta)
    });
    let mut refined = Vec::new();
    for (delta, _) in candidates
        .into_iter()
        .take(12)
        .take_while(|&(_, votes)| votes >= 5)
    {
        for adjustment in [0, -1, 1, -2, 2] {
            let candidate = delta + adjustment;
            if candidate.unsigned_abs() >= 2 && !refined.contains(&candidate) {
                refined.push(candidate);
            }
        }
    }
    let mut best = None;
    for delta in refined {
        let displacement = delta.unsigned_abs();
        if displacement >= end - start || end - start - displacement < 12 {
            continue;
        }
        let overlap = end - start - displacement;
        let (new_start, old_start) = if delta > 0 {
            (start, start + displacement)
        } else {
            (start + displacement, start)
        };
        let exact_matches = (0..overlap)
            .filter(|row| to[(new_start + row) as usize] == from[(old_start + row) as usize])
            .count() as u32;
        let matched = if exact_matches * 4 >= overlap * 3 {
            exact_matches * 100 / overlap
        } else {
            let translated = alignment(axis, before, after, changed, new_start, old_start, overlap);
            let stationary = alignment(axis, before, after, changed, new_start, new_start, overlap);
            // Require broad agreement and improvement over zero displacement.
            if translated < 70 || translated < stationary + 12 {
                continue;
            }
            translated
        };
        let evidence = matched * overlap;
        if best.as_ref().is_none_or(|(score, _)| evidence > *score) {
            best = Some((
                evidence,
                Shift {
                    axis,
                    area: axis.rect(start, 0, end - start, breadth),
                    delta,
                },
            ));
        }
    }
    best
}

fn alignment(
    axis: Axis,
    before: &RgbaImage,
    after: &RgbaImage,
    changed: PixelRect,
    new_start: u32,
    old_start: u32,
    overlap: u32,
) -> u32 {
    let first = axis.lateral(changed.x, changed.y);
    let breadth = axis.lateral(changed.width, changed.height);
    let stride = (breadth / 48).max(1);
    let mut matched = 0;
    let mut sampled = 0;
    for row in (0..overlap).step_by(3) {
        for across in (first..first + breadth).step_by(stride as usize) {
            let (old_x, old_y) = axis.xy(old_start + row, across);
            let (new_x, new_y) = axis.xy(new_start + row, across);
            let old = before.get_pixel(old_x, old_y).0;
            let new = after.get_pixel(new_x, new_y).0;
            let difference: u32 = old[..3]
                .iter()
                .zip(&new[..3])
                .map(|(a, b)| u32::from(a.abs_diff(*b)))
                .sum();
            matched += u32::from(difference <= 32);
            sampled += 1;
        }
    }
    matched * 100 / sampled
}

fn fingerprints(axis: Axis, image: &RgbaImage, length: u32, start: u32, end: u32) -> Vec<u64> {
    let samples = (end - start).min(48);
    (0..length)
        .map(|along| {
            let mut hash = DefaultHasher::new();
            for index in 0..samples {
                let across = start + index * (end - start) / samples;
                let (x, y) = axis.xy(along, across);
                image.get_pixel(x, y).0.hash(&mut hash);
            }
            hash.finish()
        })
        .collect()
}
