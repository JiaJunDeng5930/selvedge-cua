#![deny(unsafe_code)]

pub mod accessibility;
mod convert;
pub mod input;
pub mod observation;
pub mod trajectory;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
