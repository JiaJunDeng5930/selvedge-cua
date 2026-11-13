use crate::{
    convert::{boolean, items, list, n, number},
    corelib::init::datatypes::{Bool, Prod},
    observation_rules::{self, Delivery, ImageKind},
};
use std::ops::RangeInclusive;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyframe,
    Change,
    Scroll,
    Viewport,
}

pub fn group_frames(same_as_previous: &[bool]) -> Vec<RangeInclusive<usize>> {
    items(observation_rules::group_frames(list(
        same_as_previous.iter().copied().map(boolean),
    )))
    .map(|Prod::Pair(first, last)| number(&first) as usize..=number(&last) as usize)
    .collect()
}

pub fn local_change(changed_area: u64, viewport_area: u64) -> bool {
    matches!(
        observation_rules::local_change(n(changed_area), n(viewport_area)),
        Bool::True
    )
}

pub fn same_scroll(first: [u32; 6], next: [u32; 6]) -> bool {
    let dimensions = |signature: [u32; 6]| list(signature.into_iter().map(|value| n(value.into())));
    matches!(
        observation_rules::same_scroll(dimensions(first), dimensions(next)),
        Bool::True
    )
}

pub enum ImagePlan {
    Combined(Vec<usize>),
    Separate(Vec<Vec<usize>>),
}

pub fn delivery_plan(budget: usize, history: &[Kind]) -> ImagePlan {
    let kinds = list(history.iter().copied().enumerate().map(|(index, kind)| {
        Prod::Pair(
            n(index as u64),
            match kind {
                Kind::Keyframe => ImageKind::Keyframe,
                Kind::Change => ImageKind::Change,
                Kind::Scroll => ImageKind::Scroll,
                Kind::Viewport => ImageKind::Viewport,
            },
        )
    }));
    let indices = |group| items(group).map(|id| number(&id) as usize).collect();
    match observation_rules::delivery_plan(n(budget as u64), kinds) {
        Delivery::Combined(history) => ImagePlan::Combined(indices(history)),
        Delivery::Separate(history) => ImagePlan::Separate(items(history).map(indices).collect()),
    }
}
