use image::codecs::png::PngEncoder;
use image::{GenericImage, ImageEncoder, Rgba, RgbaImage, imageops};
use selvedge_rules::observation::{self as rules, ImagePlan, Kind};
use serde::Serialize;
use serde_json::{Value, json};

use crate::scroll::{self, Axis, Shift};
use crate::{DesktopRect, Frame, PixelRect};

pub struct TimedFrame {
    pub frame: Frame,
    pub at_ms: f64,
}

pub struct Observation {
    pub metadata: Value,
    pub images: Vec<Vec<u8>>,
}

struct FrameRun {
    latest: TimedFrame,
    first_at_ms: f64,
    samples: usize,
}

#[derive(Clone, Copy)]
struct Slice<'a> {
    run: &'a FrameRun,
    area: PixelRect,
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

/// Keep the actual final viewport and spend the remaining budget on views of
/// earlier states, changed regions and newly exposed scrolling content.
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
    let viewport = Asset::single(Kind::Viewport, latest);
    let mut history = Vec::new();

    if runs.len() > 1 {
        let mut after = 1;
        if scroll_between(&runs[0], &runs[1]).is_none() {
            history.push(Asset::single(Kind::Keyframe, &runs[0]));
        }
        while after < runs.len() {
            let previous = &runs[after - 1];
            let current = &runs[after];
            if let Some(shift) = scroll_between(previous, current) {
                after = scroll_run(&runs, after, shift, &mut history);
            } else {
                match difference(previous, current) {
                    Some(rect) if is_local(current, rect) => {
                        history.push(Asset::change(previous, current, rect));
                    }
                    _ if after + 1 < runs.len() => {
                        history.push(Asset::single(Kind::Keyframe, current));
                    }
                    _ => {}
                }
                after += 1;
            }
        }
    }

    let kinds: Vec<_> = history.iter().map(|asset| asset.kind).collect();
    let plan = rules::delivery_plan(image_budget, &kinds);
    let mut images = Vec::new();
    let mut descriptions = Vec::new();
    let mut encode = |assets: &[&Asset<'_>]| {
        let (png, items) = encode_image(assets, images.len())?;
        descriptions.push(items);
        images.push(png);
        Ok::<(), String>(())
    };
    // Both constructors require the actual final viewport. Every history
    // index resolves to an asset that owns a borrow of its source frame.
    match plan {
        ImagePlan::Combined(selected) => {
            let mut assets = resolve_assets(&history, selected);
            assets.push(&viewport);
            encode(&assets)?;
        }
        ImagePlan::Separate(groups) => {
            for group in groups {
                encode(&resolve_assets(&history, group))?;
            }
            encode(&[&viewport])?;
        }
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

    fn change(previous: &'a FrameRun, current: &'a FrameRun, rect: PixelRect) -> Self {
        Self {
            kind: Kind::Change,
            axis: Axis::Horizontal,
            slices: vec![
                Slice {
                    run: previous,
                    area: rect,
                },
                Slice {
                    run: current,
                    area: rect,
                },
            ],
        }
    }
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
            pair[1].frame.same_viewport(&pair[0].frame)
                && pair[1].frame.pixels == pair[0].frame.pixels
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
    difference_bounds(&after.pixels, |x, y| Some(before.pixels.get_pixel(x, y)))
}

pub(super) fn difference_bounds<'a>(
    image: &RgbaImage,
    mut reference: impl FnMut(u32, u32) -> Option<&'a Rgba<u8>>,
) -> Option<PixelRect> {
    let mut left = image.width();
    let mut top = image.height();
    let mut right = 0;
    let mut bottom = 0;
    for (x, y, pixel) in image.enumerate_pixels() {
        if let Some(other) = reference(x, y)
            && pixel != other
        {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
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

fn scroll_run<'a>(
    runs: &'a [FrameRun],
    start: usize,
    first_shift: Shift,
    history: &mut Vec<Asset<'a>>,
) -> usize {
    let axis = first_shift.axis;
    let area = first_shift.area;
    let mut slices = vec![Slice {
        run: &runs[start - 1],
        area,
    }];
    let mut changes = Vec::new();
    let mut index = start;
    let mut shift = first_shift;
    loop {
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
        if let Some(rect) =
            shift.residual(&previous.latest.frame.pixels, &current.latest.frame.pixels)
        {
            if is_local(current, rect) {
                changes.push(Asset::change(previous, current, rect));
            } else {
                changes.push(Asset::single(Kind::Keyframe, current));
            }
        }

        index += 1;
        let Some(next) = runs
            .get(index)
            .and_then(|next| scroll_between(current, next))
        else {
            break;
        };
        if !rules::same_scroll(shift_signature(shift), shift_signature(next)) {
            break;
        }
        shift = next;
    }
    if shift.delta < 0 {
        slices.reverse();
    }
    history.push(Asset {
        kind: Kind::Scroll,
        axis,
        slices,
    });
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

fn resolve_assets<'a>(history: &'a [Asset<'a>], indices: Vec<usize>) -> Vec<&'a Asset<'a>> {
    indices.into_iter().map(|index| &history[index]).collect()
}

fn encode_image(assets: &[&Asset<'_>], image_index: usize) -> Result<(Vec<u8>, Vec<Item>), String> {
    const GAP: u32 = 4;
    let mut top = 0;
    let mut width = 0;
    let mut items = Vec::new();
    let mut placements = Vec::new();
    for asset in assets {
        let axis = asset.axis;
        let between = if asset.kind == Kind::Change { GAP } else { 0 };
        let length = asset
            .slices
            .iter()
            .map(|slice| axis.longitudinal(slice.area.width, slice.area.height))
            .sum::<u32>()
            + between * (asset.slices.len() as u32 - 1);
        let breadth = asset
            .slices
            .iter()
            .map(|slice| axis.lateral(slice.area.width, slice.area.height))
            .max()
            .expect("each asset has a visible slice");
        let (asset_width, asset_height) = axis.xy(length, breadth);
        // A dedicated scroll image can stay long enough to preserve text.
        // Composite tiles use a smaller footprint to preserve the final viewport.
        let max_height = if assets.len() == 1 { 10_240 } else { 1_920 };
        let scale = if asset.kind == Kind::Viewport {
            1.0
        } else {
            1.0_f64
                .min(2560.0 / f64::from(asset_width))
                .min(f64::from(max_height) / f64::from(asset_height))
                .min((12_000_000.0 / (f64::from(asset_width) * f64::from(asset_height))).sqrt())
        };
        let scaled = |value: u32| (f64::from(value) * scale).round() as u32;
        let mut origin = 0;
        let mut tiles = Vec::new();
        for slice in &asset.slices {
            let edge = origin + axis.longitudinal(slice.area.width, slice.area.height);
            let slice_length = scaled(edge) - scaled(origin);
            let mut image_rect = axis.rect(
                scaled(origin),
                0,
                slice_length,
                scaled(axis.lateral(slice.area.width, slice.area.height)).max(1),
            );
            image_rect.y += top;
            origin = edge + between;
            if slice_length == 0 {
                // A heavily scaled mosaic can hide a subpixel strip. Its sample
                // time remains in the observation's frame list.
                continue;
            }
            placements.push((*slice, image_rect));
            let frame = &slice.run.latest.frame;
            tiles.push(Location {
                image_index,
                image_rect,
                desktop_rect: frame.desktop_area(slice.area),
                at_ms: slice.run.latest.at_ms,
                first_at_ms: slice.run.first_at_ms,
                last_at_ms: slice.run.latest.at_ms,
            });
        }
        items.push(Item {
            kind: match asset.kind {
                Kind::Keyframe => "keyframe",
                Kind::Change => "change",
                Kind::Scroll => "scroll",
                Kind::Viewport => "viewport",
            },
            tiles,
        });
        width = width.max(scaled(asset_width).max(1));
        top += scaled(asset_height).max(1) + GAP;
    }
    let mut canvas = RgbaImage::from_pixel(width, top - GAP, Rgba([35, 35, 35, 255]));
    for (slice, output) in placements {
        let input = slice.area;
        let pixels = &slice.run.latest.frame.pixels;
        let crop = imageops::crop_imm(pixels, input.x, input.y, input.width, input.height);
        if (input.width, input.height) == (output.width, output.height) {
            canvas.copy_from(&*crop, output.x, output.y)
        } else {
            let tile = imageops::resize(
                &*crop,
                output.width,
                output.height,
                imageops::FilterType::Triangle,
            );
            canvas.copy_from(&tile, output.x, output.y)
        }
        .map_err(|error| error.to_string())?;
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
