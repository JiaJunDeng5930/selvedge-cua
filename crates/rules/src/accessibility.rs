use crate::{
    accessibility_rules::{self, Insertion},
    convert::{boolean, n},
    corelib::init::datatypes::Bool,
};

pub enum WindowMatch {
    Unmatched,
    Ambiguous,
    Geometry,
    Title,
}

pub fn choose_window(geometry: usize, title: usize) -> WindowMatch {
    use accessibility_rules::WindowMatch as Rule;
    match accessibility_rules::choose_window(n(geometry as u64), n(title as u64)) {
        Rule::Unmatched => WindowMatch::Unmatched,
        Rule::Ambiguous => WindowMatch::Ambiguous,
        Rule::Geometry => WindowMatch::Geometry,
        Rule::Title => WindowMatch::Title,
    }
}

pub fn retained_read(age: usize) -> bool {
    matches!(
        accessibility_rules::retained_read(n(age as u64)),
        Bool::True
    )
}

pub fn reference_live(age: usize, terminated: bool) -> bool {
    matches!(
        accessibility_rules::reference_live(n(age as u64), boolean(terminated)),
        Bool::True
    )
}

/// An affirmative decision is valid for the supplied native observations.
pub fn authorize_background_insertion(
    text_role: bool,
    writable: bool,
    disabled: bool,
    same_element: bool,
) -> Result<(), &'static str> {
    match accessibility_rules::authorize_insertion(
        boolean(text_role),
        boolean(writable),
        boolean(disabled),
        boolean(same_element),
    ) {
        Insertion::Insert => Ok(()),
        Insertion::NotEditable => Err("the target is not an enabled editable text control"),
        Insertion::NotFocused => Err("the target does not have keyboard focus"),
    }
}
