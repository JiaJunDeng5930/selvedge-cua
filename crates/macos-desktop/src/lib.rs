//! macOS services that operate outside a single application window.

mod applications;

pub use applications::{LaunchApp, launch_app, list_apps};
