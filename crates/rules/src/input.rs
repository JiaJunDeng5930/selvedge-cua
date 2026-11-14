//! The Rust adapter translates platform identities and times to the types used
//! by [InputRules.v](../formal/InputRules.v). It leaves ownership, transition
//! validation and release order to the extracted functions.
use crate::{
    convert::{boolean, items, list, n, number},
    corelib::{
        init::datatypes::{List, Option_1},
        numbers::bin_nums::N,
    },
    input_rules::{self, Control, Outcome, TimedStep, Transition},
};

#[derive(Clone, Copy, Debug)]
pub enum Held {
    Mouse(u8),
    Key { code: u16, pid: i32 },
}

#[derive(Clone, Copy)]
pub enum Change {
    Unchanged,
    Press(Held),
    Release(Held),
}

pub struct State {
    previous: N,
    held: List<Control>,
    destination: Option<input_rules::Destination>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            previous: N::N0,
            held: List::Nil,
            destination: None,
        }
    }
}

impl State {
    pub fn held(&self) -> impl Iterator<Item = Held> {
        held_controls(&self.held)
    }

    pub fn reference(&self) -> Option<u64> {
        match self.held {
            List::Nil => None,
            List::Cons(..) => Some(number(&self.previous)),
        }
    }

    /// Withdraw a reference whose desktop authorization has ended.
    /// The generation remains reserved, even if native release failed.
    pub fn invalidate_held(&mut self) {
        self.held = List::Nil;
        self.destination = None;
    }

    pub fn window(&self) -> Option<u32> {
        match &self.destination {
            Some(input_rules::Destination::Destination(_, Option_1::Some(window))) => {
                Some(number(window) as u32)
            }
            _ => None,
        }
    }

    pub fn event_group(&self) -> i64 {
        number(&self.previous) as i64
    }

    pub fn authorize(&self, requested: Option<u64>) -> Result<(), String> {
        self.reference_for(requested).map(|_| ())
    }

    fn reference_for(&self, requested: Option<u64>) -> Result<N, String> {
        result(input_rules::authorize(
            self.previous.clone(),
            self.held.clone(),
            option_n(requested),
        ))
    }

    pub fn begin(
        &mut self,
        requested: Option<u64>,
        target: Option<(i32, Option<u32>)>,
        plan: impl DoubleEndedIterator<Item = (u64, Change)>,
    ) -> Result<(), String> {
        let reference = self.reference_for(requested)?;
        let destination = target
            .map(|(pid, window)| {
                let active = match &self.destination {
                    Some(destination) if self.reference().is_some() => {
                        Option_1::Some(destination.clone())
                    }
                    _ => Option_1::None,
                };
                result(input_rules::bind_destination(
                    active,
                    input_rules::Destination::Destination(
                        n(pid as u64),
                        option_n(window.map(u64::from)),
                    ),
                ))
            })
            .transpose()?;
        let steps = plan.rfold(List::Nil, |tail, (offset, change)| {
            List::Cons(TimedStep::Step(n(offset), change.into()), Box::new(tail))
        });
        result(input_rules::validate_plan(N::N0, self.held.clone(), steps))?;
        self.previous = reference;
        self.destination = destination.or_else(|| self.reference().and(self.destination.take()));
        Ok(())
    }

    // Preparation and acknowledgement cannot be interleaved with another
    // transition. A failed platform send leaves the previous state in place.
    pub fn dispatch<E: From<String>>(
        &mut self,
        change: Change,
        send: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        let next = result(input_rules::apply_transition(
            self.held.clone(),
            change.into(),
        ))?;
        send()?;
        self.held = next;
        Ok(())
    }

    pub fn releases(&self, hold: bool, failed: bool) -> Vec<Held> {
        held_controls(&input_rules::finish_releases(
            boolean(hold),
            boolean(failed),
            self.held.clone(),
        ))
        .collect()
    }

    pub fn new_pointer_presses(&self, buttons: Vec<u8>) -> impl Iterator<Item = u8> {
        let buttons = list(buttons.into_iter().map(|button| n(button.into())));
        items(input_rules::new_pointer_presses(self.held.clone(), buttons))
            .map(|button| number(&button) as u8)
    }
}

impl From<Held> for Control {
    fn from(held: Held) -> Self {
        match held {
            Held::Mouse(button) => Self::Mouse(n(button.into())),
            Held::Key { code, pid } => Self::Key(n(code.into()), n(pid as u64)),
        }
    }
}

impl From<Change> for Transition {
    fn from(change: Change) -> Self {
        match change {
            Change::Unchanged => Self::Unchanged,
            Change::Press(held) => Self::Press(held.into()),
            Change::Release(held) => Self::Release(held.into()),
        }
    }
}

fn held_controls(list: &List<Control>) -> impl Iterator<Item = Held> {
    let mut next = list;
    std::iter::from_fn(move || match next {
        List::Nil => None,
        List::Cons(control, tail) => {
            next = tail;
            Some(match control {
                Control::Mouse(button) => Held::Mouse(number(button) as u8),
                Control::Key(code, pid) => Held::Key {
                    code: number(code) as u16,
                    pid: number(pid) as i32,
                },
            })
        }
    })
}

fn result<T>(outcome: Outcome<T>) -> Result<T, String> {
    match outcome {
        Outcome::Accepted(value) => Ok(value),
        Outcome::OwnerRequired => Err("The live input operation requires its reference"),
        Outcome::StaleReference => Err("The input operation reference is stale or unknown"),
        Outcome::ReferencesExhausted => Err("The input operation reference space is exhausted"),
        Outcome::DestinationMismatch => {
            Err("The input operation belongs to another process or window")
        }
        Outcome::OutOfOrder => Err("Input event times must be nondecreasing"),
        Outcome::AlreadyPressed => Err("The physical key or button is already held"),
        Outcome::NotPressed => Err("The key or button is not held for that target"),
    }
    .map_err(str::to_owned)
}

fn option_n(value: Option<u64>) -> Option_1<N> {
    match value {
        Some(value) => Option_1::Some(n(value)),
        None => Option_1::None,
    }
}
