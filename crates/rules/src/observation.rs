use crate::{
    convert::{boolean, items, list, n, number},
    corelib::init::datatypes::{Bool, Nat, Option_1, Prod},
    observation_rules::{self, Candidate},
};
use std::ops::RangeInclusive;

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

/// Select readable attachments that cover all evidence in order within the
/// attachment budget. Each row lists the (span, token cost) choices starting
/// at that evidence position.
pub fn pack_images(budget: usize, candidates: &[Vec<(usize, u64)>]) -> Option<Vec<usize>> {
    let rows = list(candidates.iter().enumerate().map(|(index, choices)| {
        let remaining = candidates.len() - index;
        list(choices.iter().filter_map(move |&(span, cost)| {
            // DEFENSE: A public caller can submit an empty span or one
            // extending beyond the evidence. Neither describes an attachment.
            if span == 0 || span > remaining {
                return None;
            }
            let following = (1..span).fold(Nat::O, |count, _| Nat::S(Box::new(count)));
            Some(Candidate::Candidate(following, n(cost)))
        }))
    }));
    let budget = budget.min(candidates.len());
    match observation_rules::pack_images(n(budget as u64), rows) {
        Option_1::Some(spans) => Some(items(spans).map(|span| number(&span) as usize).collect()),
        Option_1::None => None,
    }
}
