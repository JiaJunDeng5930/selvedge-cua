use image::codecs::png::PngEncoder;
use image::{GenericImage, ImageEncoder, Rgba, RgbaImage, imageops};
use selvedge_rules::observation as rules;
use serde::Serialize;
use serde_json::{Value, json};

use crate::scroll::{self, Axis, Shift};
use crate::{DesktopRect, Frame, PixelRect};

pub struct TimedFrame {
    pub frame: Frame,
    pub at_ms: f64,
    /// Stable dispatch epoch, or none if a dispatch overlaps this capture.
    pub input_epoch: Option<u64>,
}

pub struct Observation {
    pub metadata: Value,
    pub images: Vec<Vec<u8>>,
}

struct FrameRun {
    latest: TimedFrame,
    source_index: usize,
    first_at_ms: f64,
    samples: usize,
}

#[derive(Clone, Copy)]
struct Slice<'a> {
    run: &'a FrameRun,
    area: PixelRect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Keyframe,
    Scroll,
    Viewport,
}

struct Asset<'a> {
    kind: Kind,
    axis: Axis,
    slices: Vec<Slice<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Location {
    image_index: usize,
    image_rect: PixelRect,
    desktop_rect: DesktopRect,
    at_ms: f64,
    first_at_ms: f64,
    last_at_ms: f64,
}

#[derive(Serialize)]
struct Item {
    kind: &'static str,
    tiles: Vec<Location>,
}

/// Build the full-frame, animation or scroll evidence described in
/// Requirement.v. The capture adapter supplies the observed pixels and
/// geometry. The extracted packing rule operates on measured layouts.
pub fn summarize(frames: Vec<TimedFrame>, image_budget: usize) -> Result<Observation, String> {
    if image_budget == 0 || frames.is_empty() {
        return Err(
            "Screen observation requires at least one frame and a positive image budget".into(),
        );
    }
    let runs = merge_identical_frames(frames)?;
    let latest = runs
        .last()
        .expect("a nonempty observation has a final frame");
    let mut evidence = collect_evidence(&runs);
    evidence.push(Asset::single(Kind::Viewport, latest));
    let mut images = Vec::new();
    let mut descriptions = Vec::new();
    let (selected, candidates, plan) = select_layout(&evidence, image_budget)?;
    let mut start = 0;
    for len in plan {
        let layout = candidates[start]
            .iter()
            .find(|candidate| candidate.len == len)
            .expect("the checked plan uses an available candidate");
        let (png, items) = encode_image(&selected[start..start + len], layout, images.len())?;
        descriptions.push(items);
        images.push(png);
        start += len;
    }
    let viewport_location = descriptions
        .last()
        .and_then(|items| items.last())
        .and_then(|item| item.tiles.first())
        .expect("the final tile is the actual viewport");

    let frames: Vec<_> = runs
        .iter()
        .map(|run| {
            let (width, height) = run.latest.frame.dimensions();
            json!({
                "firstAtMs": run.first_at_ms,
                "lastAtMs": run.latest.at_ms,
                "sampleCount": run.samples,
                "desktopRect": run.latest.frame.desktop,
                "pixelWidth": width,
                "pixelHeight": height,
            })
        })
        .collect();
    Ok(Observation {
        metadata: json!({
            "viewport": viewport_location,
            "frames": frames,
            "images": descriptions,
        }),
        images,
    })
}

impl<'a> Asset<'a> {
    fn single(kind: Kind, run: &'a FrameRun) -> Self {
        Self {
            kind,
            axis: Axis::Vertical,
            slices: vec![Slice {
                run,
                area: run.latest.frame.full_area(),
            }],
        }
    }

    fn dimensions(&self) -> (u32, u32) {
        let mut length = 0;
        let mut breadth = 0;
        for slice in &self.slices {
            let PixelRect { width, height, .. } = slice.area;
            length += self.axis.longitudinal(width, height);
            breadth = breadth.max(self.axis.lateral(width, height));
        }
        self.axis.xy(length, breadth)
    }

    fn impact(&self, previous: &Self) -> u64 {
        let current = self.slices[0].run;
        let previous = previous.slices[0].run;
        difference(previous, current)
            .map_or_else(|| current.latest.frame.full_area().area(), PixelRect::area)
    }
}

fn collect_evidence(runs: &[FrameRun]) -> Vec<Asset<'_>> {
    let shifts: Vec<_> = runs
        .windows(2)
        .map(|pair| scroll_between(&pair[0], &pair[1]))
        .collect();
    let shifts = stabilize_scroll_areas(runs, &shifts);
    let mut evidence = Vec::new();
    let mut current = 0;
    while current + 1 < runs.len() {
        if let Some(shift) = shifts[current] {
            current = scroll_run(runs, &shifts, current + 1, shift, &mut evidence) - 1;
        } else {
            if !evidence.last().is_some_and(|asset: &Asset<'_>| {
                asset.kind == Kind::Keyframe
                    && asset.slices[0].run.source_index == runs[current].source_index
            }) {
                evidence.push(Asset::single(Kind::Keyframe, &runs[current]));
            }
            current += 1;
        }
    }
    evidence
}

fn same_input_phase(a: &TimedFrame, b: &TimedFrame) -> bool {
    a.input_epoch.is_some() && a.input_epoch == b.input_epoch
}

fn equivalent_pixels(a: &Frame, b: &Frame) -> bool {
    a.same_viewport(b)
        && (a.pixels == b.pixels
            || a.pixels
                .pixels()
                .zip(b.pixels.pixels())
                .all(|(a, b)| a == b || (a.0[3] == 0 && b.0[3] == 0)))
}

fn merge_identical_frames(frames: Vec<TimedFrame>) -> Result<Vec<FrameRun>, String> {
    if frames.iter().any(|frame| !frame.at_ms.is_finite())
        || frames.windows(2).any(|pair| pair[1].at_ms < pair[0].at_ms)
    {
        return Err("Screen frame times must be finite and chronological".into());
    }
    let matches = frames
        .windows(2)
        .map(|pair| {
            same_input_phase(&pair[1], &pair[0])
                && equivalent_pixels(&pair[1].frame, &pair[0].frame)
        })
        .collect::<Vec<_>>();
    let times = frames.iter().map(|frame| frame.at_ms).collect::<Vec<_>>();
    let mut frames = frames.into_iter();
    Ok(rules::group_frames(&matches)
        .into_iter()
        .map(|span| {
            let samples = span.clone().count();
            FrameRun {
                latest: frames
                    .nth(samples - 1)
                    .expect("the span indexes captured frames"),
                source_index: *span.end(),
                first_at_ms: times[*span.start()],
                samples,
            }
        })
        .collect())
}

fn difference(previous: &FrameRun, current: &FrameRun) -> Option<PixelRect> {
    let before = &previous.latest.frame;
    let after = &current.latest.frame;
    if !before.same_viewport(after) {
        return None;
    }
    let width = after.pixels.width();
    let height = after.pixels.height();
    let row_bytes = width as usize * 4;
    let mut left = width;
    let mut top = height;
    let mut right = 0;
    let mut bottom = 0;
    for y in 0..height {
        let offset = y as usize * row_bytes;
        let before = &before.pixels.as_raw()[offset..offset + row_bytes];
        let after = &after.pixels.as_raw()[offset..offset + row_bytes];
        let pixels = before.chunks_exact(4).zip(after.chunks_exact(4));
        if let Some(first) = pixels.clone().position(|(a, b)| a != b) {
            let last = pixels
                .rev()
                .position(|(a, b)| a != b)
                .expect("a pixel differs");
            left = left.min(first as u32);
            right = right.max(width - last as u32);
            top = top.min(y);
            bottom = y + 1;
        }
    }
    (left < right).then(|| PixelRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn is_local(run: &FrameRun, changed: PixelRect) -> bool {
    rules::local_change(changed.area(), run.latest.frame.full_area().area())
}

#[derive(Clone, Copy)]
struct ScrollStep {
    shift: Shift,
    has_residual: bool,
}

fn scroll_between(previous: &FrameRun, current: &FrameRun) -> Option<Shift> {
    let changed = difference(previous, current)?;
    if is_local(current, changed) {
        return None;
    }
    scroll::detect(
        &previous.latest.frame.pixels,
        &current.latest.frame.pixels,
        changed,
    )
}

fn stabilize_scroll_areas(runs: &[FrameRun], shifts: &[Option<Shift>]) -> Vec<Option<ScrollStep>> {
    let mut steps = Vec::with_capacity(shifts.len());
    let mut start = 0;
    while start < shifts.len() {
        let Some(first) = shifts[start] else {
            steps.push(None);
            start += 1;
            continue;
        };
        let mut end = start + 1;
        let mut area = first.area;
        while let Some(Some(next)) = shifts.get(end) {
            if next.axis != first.axis || next.delta.signum() != first.delta.signum() {
                break;
            }
            area = area.union(next.area);
            end += 1;
        }

        // Equal edge pixels make a difference rectangle narrower than the
        // scrolling region. Use the common bounds only if the captured pixels
        // confirm that the same translation explains each frame pair there.
        for (index, shift) in shifts.iter().enumerate().take(end).skip(start) {
            let shift = shift.expect("the run consists of detected scrolls");
            let before = &runs[index].latest.frame.pixels;
            let after = &runs[index + 1].latest.frame.pixels;
            let expanded = Shift { area, ..shift };
            let step = if !expanded.has_residual(before, after) {
                ScrollStep {
                    shift: expanded,
                    has_residual: false,
                }
            } else {
                ScrollStep {
                    shift,
                    has_residual: area == shift.area || shift.has_residual(before, after),
                }
            };
            steps.push(Some(step));
        }
        start = end;
    }
    steps
}

fn scroll_run<'a>(
    runs: &'a [FrameRun],
    shifts: &[Option<ScrollStep>],
    start: usize,
    first_step: ScrollStep,
    history: &mut Vec<Asset<'a>>,
) -> usize {
    let first_shift = first_step.shift;
    let axis = first_shift.axis;
    let area = first_shift.area;
    let mut slices = vec![Slice {
        run: &runs[start - 1],
        area,
    }];
    let mut changes = Vec::new();
    let mut index = start;
    let mut step = first_step;
    loop {
        let shift = step.shift;
        let previous = &runs[index - 1];
        let current = &runs[index];
        let length = shift.delta.unsigned_abs();
        let area_length = axis.longitudinal(area.width, area.height);
        let area_start = axis.longitudinal(area.x, area.y);
        let along = if shift.delta > 0 {
            area_start + area_length - length
        } else {
            area_start
        };
        slices.push(Slice {
            run: current,
            area: axis.rect(
                along,
                axis.lateral(area.x, area.y),
                length,
                axis.lateral(area.width, area.height),
            ),
        });
        if (step.has_residual || !same_input_phase(&previous.latest, &current.latest))
            && index + 1 < runs.len()
        {
            changes.push(Asset::single(Kind::Keyframe, current));
        }

        index += 1;
        let Some(next) = shifts.get(index - 1).copied().flatten() else {
            break;
        };
        if !rules::same_scroll(shift_signature(shift), shift_signature(next.shift)) {
            break;
        }
        step = next;
    }
    if first_shift.delta < 0 {
        slices.reverse();
    }
    // A long scroll remains readable as consecutive, abutting parts.
    // Stop each part before the consumer would shrink its source details.
    let mut part = Asset {
        kind: Kind::Scroll,
        axis,
        slices: Vec::new(),
    };
    for slice in slices {
        part.slices.push(slice);
        let (width, height) = part.dimensions();
        if !preserves_source_scale(width, height, &part) && part.slices.len() > 1 {
            let last = part.slices.pop().expect("one strip was just added");
            history.push(part);
            part = Asset {
                kind: Kind::Scroll,
                axis,
                slices: vec![last],
            };
        }
    }
    history.push(part);
    history.extend(changes);
    index
}

fn shift_signature(shift: Shift) -> [u32; 6] {
    let area = shift.area;
    [
        u32::from(shift.axis == Axis::Horizontal),
        u32::from(shift.delta > 0),
        area.x,
        area.y,
        area.width,
        area.height,
    ]
}

/// A conservative consumer profile for model clients using 512-pixel
/// tiles. The upper bound matches the documented high-detail preprocessing
/// for GPT-4.1. No additional layout resampling is applied to source pixels.
/// KNOWN LIMITATION: MCP cannot identify the client's vision preprocessing,
/// so measured detail and token estimates are relative to this profile.
fn consumer_scale(width: u32, height: u32) -> f64 {
    1.0_f64
        .min(2048.0 / f64::from(width.max(height)))
        .min(768.0 / f64::from(width.min(height)))
}

fn preserves_source_scale(width: u32, height: u32, asset: &Asset<'_>) -> bool {
    let scale = consumer_scale(width, height);
    asset.slices.iter().all(|slice| {
        let (width, height) = slice.run.latest.frame.dimensions();
        scale >= consumer_scale(width, height)
    })
}

struct Layout {
    len: usize,
    axis: Axis,
    width: u32,
    height: u32,
    tokens: u64,
}

impl Layout {
    fn measure(assets: &[&Asset<'_>], axis: Axis) -> Option<Self> {
        let mut length = 0;
        let mut breadth = 0;
        for asset in assets {
            let (width, height) = asset.dimensions();
            length += axis.longitudinal(width, height);
            breadth = breadth.max(axis.lateral(width, height));
        }
        let (width, height) = axis.xy(length, breadth);
        if !assets
            .iter()
            .all(|asset| preserves_source_scale(width, height, asset))
        {
            return None;
        }
        let scale = consumer_scale(width, height);
        let tiles = |size: u32| (f64::from(size) * scale).floor().max(1.0) as u64;
        // The manifest has one mapping per source tile in every layout.
        // Image tokens are estimated, since the MCP client chooses its model.
        let tokens = 85 + 170 * tiles(width).div_ceil(512) * tiles(height).div_ceil(512);
        Some(Self {
            len: assets.len(),
            axis,
            width,
            height,
            tokens,
        })
    }
}

type LayoutCandidates = Vec<Vec<Layout>>;

fn candidates_for(assets: &[&Asset<'_>]) -> LayoutCandidates {
    (0..assets.len())
        .map(|start| {
            (start + 1..=assets.len())
                .filter_map(|end| {
                    let group = &assets[start..end];
                    [Axis::Horizontal, Axis::Vertical]
                        .into_iter()
                        .filter_map(|axis| Layout::measure(group, axis))
                        .min_by_key(|layout| {
                            (
                                layout.tokens,
                                u64::from(layout.width) * u64::from(layout.height),
                            )
                        })
                })
                .collect()
        })
        .collect()
}

fn animation_candidates(assets: &[Asset<'_>]) -> Vec<(usize, u64)> {
    // A brief A-B-A is evidence, even if B differs by one pixel. Only
    // sustained sequences of fresh captures can give up intermediate frames.
    let mut optional = Vec::new();
    let mut start = 0;
    while start < assets.len() {
        if assets[start].kind != Kind::Keyframe || assets[start].slices[0].run.samples != 1 {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < assets.len()
            && assets[end].kind != Kind::Scroll
            && assets[end].slices[0].run.samples == 1
            && assets[end].slices[0].run.latest.at_ms - assets[end - 1].slices[0].run.latest.at_ms
                <= 250.0
        {
            end += 1;
        }
        if end - start >= 5 {
            for index in start + 1..end - 1 {
                // Protect a local excursion bracketed by the same full
                // state, and preserve explicitly requested input boundaries.
                let before = assets[index - 1].slices[0].run;
                let current = assets[index].slices[0].run;
                let after = assets[index + 1].slices[0].run;
                if !same_input_phase(&current.latest, &before.latest)
                    || !same_input_phase(&current.latest, &after.latest)
                    || equivalent_pixels(&before.latest.frame, &after.latest.frame)
                {
                    continue;
                }
                let impact = assets[index]
                    .impact(&assets[index - 1])
                    .max(assets[index + 1].impact(&assets[index]));
                optional.push((index, impact));
            }
        }
        start = end;
    }
    optional.sort_by_key(|&(index, impact)| (impact, index % 2 == 0, index));
    optional
}

fn select_layout<'a, 'frame>(
    evidence: &'a [Asset<'frame>],
    budget: usize,
) -> Result<(Vec<&'a Asset<'frame>>, LayoutCandidates, Vec<usize>), String> {
    let mut discard = vec![false; evidence.len()];
    let mut optional = animation_candidates(evidence).into_iter();
    loop {
        let selected: Vec<_> = evidence
            .iter()
            .zip(&discard)
            .filter_map(|(asset, &omit)| (!omit).then_some(asset))
            .collect();
        let candidates = candidates_for(&selected);
        let costs: Vec<Vec<_>> = candidates
            .iter()
            .map(|row| {
                row.iter()
                    .map(|layout| (layout.len, layout.tokens))
                    .collect()
            })
            .collect();
        if let Some(plan) = rules::pack_images(budget, &costs) {
            return Ok((selected, candidates, plan));
        }
        let Some((index, _)) = optional.next() else {
            return Err("The observation cannot fit in the image budget without losing required detail; increase imageBudget or observe a smaller target".into());
        };
        discard[index] = true;
    }
}

fn encode_image(
    assets: &[&Asset<'_>],
    layout: &Layout,
    image_index: usize,
) -> Result<(Vec<u8>, Vec<Item>), String> {
    let mut items = Vec::with_capacity(assets.len());
    let mut canvas = RgbaImage::from_pixel(layout.width, layout.height, Rgba([35, 35, 35, 255]));
    let mut origin = 0;
    for asset in assets {
        let (x, y) = layout.axis.xy(origin, 0);
        let mut along = 0;
        let mut tiles = Vec::with_capacity(asset.slices.len());
        for slice in &asset.slices {
            let input = slice.area;
            let mut image_rect = asset.axis.rect(
                along,
                0,
                asset.axis.longitudinal(input.width, input.height),
                asset.axis.lateral(input.width, input.height),
            );
            image_rect.x += x;
            image_rect.y += y;
            along += asset.axis.longitudinal(input.width, input.height);
            let frame = &slice.run.latest.frame;
            let crop =
                imageops::crop_imm(&frame.pixels, input.x, input.y, input.width, input.height);
            canvas
                .copy_from(&*crop, image_rect.x, image_rect.y)
                .map_err(|error| error.to_string())?;
            tiles.push(Location {
                image_index,
                image_rect,
                desktop_rect: frame.desktop_area(input),
                at_ms: slice.run.latest.at_ms,
                first_at_ms: slice.run.first_at_ms,
                last_at_ms: slice.run.latest.at_ms,
            });
        }
        let (width, height) = asset.dimensions();
        origin += layout.axis.longitudinal(width, height);
        items.push(Item {
            kind: match asset.kind {
                Kind::Keyframe => "keyframe",
                Kind::Scroll => "scroll",
                Kind::Viewport => "viewport",
            },
            tiles,
        });
    }
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| error.to_string())?;
    Ok((bytes, items))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn scrolling_past_blank_rows_keeps_one_mosaic_and_the_final_viewport() {
        let viewport = DesktopRect {
            x: 0.0,
            y: 0.0,
            width: 160.0,
            height: 96.0,
        };
        for offsets in [[0, 10, 20, 30], [30, 20, 10, 0]] {
            let frames = offsets.into_iter().enumerate().map(|(index, offset)| {
                let pixels = RgbaImage::from_fn(160, 96, |x, y| {
                    let row = y + offset;
                    if row % 13 < 5 {
                        Rgba([240, 240, 240, 255])
                    } else {
                        Rgba([row as u8, x as u8, 40, 255])
                    }
                });
                TimedFrame {
                    frame: Frame::new(pixels, viewport, Instant::now()).unwrap(),
                    at_ms: index as f64 * 100.0,
                    input_epoch: Some(0),
                }
            });
            let result = summarize(frames.collect(), 6).unwrap();
            let items: Vec<_> = result.metadata["images"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|attachment| attachment.as_array().unwrap())
                .collect();
            assert_eq!(items.len(), 2, "{items:?}");
            assert_eq!(items[0]["kind"], "scroll");
            assert_eq!(items[1]["kind"], "viewport");
            let strips = items[0]["tiles"].as_array().unwrap();
            assert_eq!(strips.len(), offsets.len());
            let first_row = strips[0]["desktopRect"]["y"].as_f64().unwrap() as u32;
            let last_row = strips.last().unwrap()["desktopRect"]["y"].as_f64().unwrap() as u32
                + strips.last().unwrap()["imageRect"]["height"]
                    .as_u64()
                    .unwrap() as u32;
            assert!(first_row <= 5 && last_row >= 91, "{strips:?}");
            let decoded = image::load_from_memory(
                &result.images[strips[0]["imageIndex"].as_u64().unwrap() as usize],
            )
            .unwrap()
            .into_rgba8();
            let packed_x = strips[0]["imageRect"]["x"].as_u64().unwrap() as u32;
            let packed_y = strips[0]["imageRect"]["y"].as_u64().unwrap() as u32;
            let height: u32 = strips
                .iter()
                .map(|tile| tile["imageRect"]["height"].as_u64().unwrap() as u32)
                .sum();
            assert!(height >= 116);
            for y in 0..height {
                let row = y + first_row;
                let expected = if row % 13 < 5 {
                    Rgba([240, 240, 240, 255])
                } else {
                    Rgba([row as u8, 79, 40, 255])
                };
                assert_eq!(
                    *decoded.get_pixel(packed_x + 79, packed_y + y),
                    expected,
                    "row {row}"
                );
            }
            assert_eq!(result.metadata["viewport"]["atMs"], 300.0);
        }
    }
}
