//! macOS services that operate outside a single application window.

mod applications;
mod clipboard;

pub use applications::{LaunchApp, launch_app, list_apps};
pub use clipboard::{PasteContent, paste};
