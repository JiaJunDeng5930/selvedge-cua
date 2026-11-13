use crate::corelib::{
    init::datatypes::{Bool, List},
    numbers::bin_nums::{N, Positive},
};

pub fn boolean(value: bool) -> Bool {
    if value { Bool::True } else { Bool::False }
}

pub fn list<T: 'static>(items: impl DoubleEndedIterator<Item = T>) -> List<T> {
    items.rfold(List::Nil, |tail, item| List::Cons(item, Box::new(tail)))
}

pub fn items<T: 'static>(list: List<T>) -> impl Iterator<Item = T> {
    let mut next = list;
    std::iter::from_fn(move || match std::mem::replace(&mut next, List::Nil) {
        List::Nil => None,
        List::Cons(item, tail) => {
            next = *tail;
            Some(item)
        }
    })
}

pub fn n(value: u64) -> N {
    if value == 0 {
        N::N0
    } else {
        N::Npos(positive(value))
    }
}

fn positive(value: u64) -> Positive {
    match value {
        1 => Positive::XH,
        even if even & 1 == 0 => Positive::XO(Box::new(positive(even >> 1))),
        odd => Positive::XI(Box::new(positive(odd >> 1))),
    }
}

pub fn number(value: &N) -> u64 {
    match value {
        N::N0 => 0,
        N::Npos(value) => positive_number(value),
    }
}

fn positive_number(value: &Positive) -> u64 {
    match value {
        Positive::XH => 1,
        Positive::XO(high) => positive_number(high) << 1,
        Positive::XI(high) => (positive_number(high) << 1) | 1,
    }
}
