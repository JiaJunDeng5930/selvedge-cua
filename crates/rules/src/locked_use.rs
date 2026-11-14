use crate::{corelib::init::datatypes::Bool, locked_use_requirement as extracted};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Ordinary,
    Protected,
}

pub enum FinishReply {
    Ok,
    Locked,
}

pub fn confirmation_matches(mode: RunMode, reply: FinishReply) -> bool {
    let mode = match mode {
        RunMode::Ordinary => extracted::RunMode::Ordinary,
        RunMode::Protected => extracted::RunMode::Protected,
    };
    let reply = match reply {
        FinishReply::Ok => extracted::FinishReply::Ok,
        FinishReply::Locked => extracted::FinishReply::Locked,
    };
    matches!(extracted::confirmation_matches(mode, reply), Bool::True)
}
