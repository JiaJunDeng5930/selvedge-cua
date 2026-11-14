//! macOS Accessibility access for UI reads, actions and insertion focus checks.
//!
//! The application owns its tree and can change it during a read. An AX read
//! returns the window and focus that were observed and indicates when limits
//! or a changing tree prevented traversal of all descendants.
//! References retain native element identities because tree paths can point
//! to different controls after a reordering. The platform owns the live
//! element and may invalidate the identity at any time.

mod native;
mod window_id;

use std::collections::{HashMap, VecDeque};
use std::sync::{OnceLock, mpsc};
use std::thread;

use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{CFBoolean, CFNumber, CFString, CFType};
use selvedge_rules::accessibility;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use native::{Attributes, Element};
pub use window_id::{InputProcess, InputWindow, input_process, input_window};

type Job = Box<dyn FnOnce(&mut Accessibility) + Send>;

/// Return the app receiving keyboard events even if AX access is disabled.
pub fn frontmost_pid() -> Result<i32, String> {
    autoreleasepool(|_| {
        let application = frontmost_application()?;
        Ok(application.processIdentifier())
    })
}

/// Read the current window and focus. Bounds are per call. The root is at
/// depth zero, and the focused element is also returned independently of the
/// window traversal limits.
pub fn read_ui(options: &Value) -> Result<Value, String> {
    let options: ReadOptions = decode(options, "readUI")?;
    let limits = ReadLimits::from_options(&options)?;
    let fields = ReadFields::new(options.fields)?;
    submit(move |accessibility| accessibility.read(options.pid, limits, fields))
}

/// Apply a native action or set a writable AXValue attribute. The target
/// must be a live reference returned by read_ui.
pub fn perform_ui_action(options: &Value) -> Result<Value, String> {
    let options: ActionOptions = decode(options, "performUIAction")?;
    let target = Target::parse(&options.target)?;
    submit(move |accessibility| {
        let retained = accessibility.target(target)?;
        let pid = retained.application.processIdentifier();
        let element = &retained.element;
        if options.action == "AXSetValue" {
            let value = options
                .value
                .as_ref()
                .ok_or("AXSetValue requires a string, number, or boolean value")?;
            let value = SetValue::parse(value)?;
            if !native::settable(element, "AXValue").map_err(|error| error.to_string())? {
                return Err("the target does not expose a writable AXValue".into());
            }
            native::set_value(element, value.as_cf()).map_err(|error| error.to_string())?;
        } else {
            // DEFENSE: An action advertised during readUI can disappear as
            // the application changes, so check the live list before invoking it.
            if !native::actions(element)
                .map_err(|error| error.to_string())?
                .contains(&options.action)
            {
                return Err(format!("the target does not expose {}", options.action));
            }
            native::perform_action(element, &options.action).map_err(|error| error.to_string())?;
        }
        Ok(json!({ "pid": pid, "target": options.target, "action": options.action }))
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TextDelivery {
    AccessibilitySelection,
    FocusedProcess,
    NoOp,
}

/// Verify that the requested editable control owns the application's keyboard
/// focus. Send the text to that element if it exposes a writable selection.
/// Standard AppKit fields can expose a writable value and no selection setter.
/// For those controls, the sender posts text to the checked process.
/// KNOWN LIMITATION: Focus is sampled in a separate native call. An
/// element-directed write retains its destination if focus changes, while a
/// process-directed event is routed by the application's focus on delivery.
pub fn insert_text(
    options: &Value,
    send: impl FnOnce(i32, &str) -> Result<(), String> + Send + 'static,
) -> Result<(i32, TextDelivery), String> {
    let options: TextOptions = decode(options, "insertText")?;
    let target = Target::parse(&options.target)?;
    submit(move |accessibility| {
        let retained = accessibility.target(target)?;
        let pid = retained.application.processIdentifier();
        let element = &retained.element;
        let role = native::role(element).map_err(|error| error.to_string())?;
        let text_role = matches!(
            role.as_deref(),
            Some("AXTextField" | "AXTextArea" | "AXComboBox")
        );
        let selection_writable =
            native::settable(element, native::SELECTED_TEXT).map_err(|error| error.to_string())?;
        let writable = selection_writable
            || native::settable(element, "AXValue").map_err(|error| error.to_string())?;
        let selected_text = selection_writable.then(|| CFString::from_str(&options.text));
        let enabled = native::enabled(element).map_err(|error| error.to_string())?;
        let application = native::application(pid);
        let focused = native::element_attribute(&application, "AXFocusedUIElement")
            .map_err(|error| error.to_string())?;
        accessibility::authorize_background_insertion(
            text_role,
            writable,
            enabled == Some(false),
            focused.as_deref() == Some(&**element),
        )?;
        let delivery = if options.text.is_empty() {
            TextDelivery::NoOp
        } else if let Some(text) = selected_text {
            native::replace_selected_text(element, &text).map_err(|error| error.to_string())?;
            TextDelivery::AccessibilitySelection
        } else {
            send(pid, &options.text)?;
            TextDelivery::FocusedProcess
        };
        Ok((pid, delivery))
    })
}

fn decode<T: serde::de::DeserializeOwned>(options: &Value, function: &str) -> Result<T, String> {
    serde_json::from_value(options.clone()).map_err(|error| format!("{function}: {error}"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadOptions {
    pid: Option<i32>,
    max_depth: Option<usize>,
    max_nodes: Option<usize>,
    fields: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ActionOptions {
    target: String,
    action: String,
    value: Option<Value>,
}

#[derive(Deserialize)]
struct TextOptions {
    target: String,
    text: String,
}

struct ReadLimits {
    depth: usize,
    nodes: usize,
}

struct ReadFields(Option<Vec<String>>);

impl ReadFields {
    fn new(fields: Option<Vec<String>>) -> Result<Self, String> {
        if let Some(fields) = &fields {
            for field in fields {
                if !matches!(field.as_str(), "actions" | "settable") && !native::is_attribute(field)
                {
                    return Err(format!("Unknown readUI field: {field}"));
                }
            }
        }
        Ok(Self(fields))
    }

    fn includes(&self, field: &str) -> bool {
        self.0
            .as_ref()
            .is_none_or(|fields| fields.iter().any(|selected| selected == field))
    }
}

impl ReadLimits {
    fn from_options(options: &ReadOptions) -> Result<Self, String> {
        let depth = options.max_depth.unwrap_or(6);
        let nodes = options.max_nodes.unwrap_or(120);
        // DEFENSE: Callers set traversal limits through JSON. A depth bound
        // keeps recursion finite; a node bound limits IPC and response size.
        if depth > 32 || !(1..=500).contains(&nodes) {
            return Err("readUI requires maxDepth from 0 to 32 and maxNodes from 1 to 500".into());
        }
        Ok(Self { depth, nodes })
    }
}

enum SetValue {
    String(objc2_core_foundation::CFRetained<CFString>),
    Number(objc2_core_foundation::CFRetained<CFNumber>),
    Bool(&'static CFBoolean),
}

impl SetValue {
    fn parse(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(value) => Ok(Self::String(CFString::from_str(value))),
            Value::Bool(value) => Ok(Self::Bool(CFBoolean::new(*value))),
            Value::Number(value) => {
                let number = if let Some(integer) = value.as_i64() {
                    CFNumber::new_i64(integer)
                } else if value.is_f64() {
                    CFNumber::new_f64(value.as_f64().expect("finite JSON float"))
                } else {
                    // DEFENSE: JSON permits unsigned integers outside the
                    // signed Core Foundation range. Rounding would set a different value.
                    return Err("AXSetValue integer exceeds the signed 64-bit range".into());
                };
                Ok(Self::Number(number))
            }
            _ => Err("AXSetValue requires a string, number, or boolean value".into()),
        }
    }

    fn as_cf(&self) -> &CFType {
        match self {
            Self::String(value) => value.as_ref(),
            Self::Number(value) => value.as_ref(),
            Self::Bool(value) => (*value).as_ref(),
        }
    }
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Target(u64);

impl Target {
    fn parse(value: &str) -> Result<Self, String> {
        value
            .strip_prefix("ax-")
            .and_then(|value| value.parse().ok())
            .map(Self)
            .ok_or_else(|| "target must be a reference returned by readUI".into())
    }

    fn name(self) -> String {
        format!("ax-{}", self.0)
    }
}

fn submit<T: Send + 'static>(
    operation: impl FnOnce(&mut Accessibility) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    static WORKER: OnceLock<Result<mpsc::Sender<Job>, String>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (jobs, pending) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("macos-accessibility".into())
            .spawn(move || {
                let mut accessibility = Accessibility::new();
                for job in pending {
                    autoreleasepool(|_| job(&mut accessibility));
                }
            })
            .map_err(|error| format!("could not start macOS Accessibility worker: {error}"))?;
        Ok(jobs)
    });
    let worker = worker.as_ref().map_err(Clone::clone)?;
    let (reply, result) = mpsc::sync_channel(1);
    worker
        .send(Box::new(move |accessibility| {
            let result = accessibility
                .require_access()
                .and_then(|()| operation(accessibility));
            // A disconnected caller no longer needs this result.
            let _ = reply.send(result);
        }))
        .map_err(|_| "macOS Accessibility worker stopped")?;
    result
        .recv()
        .map_err(|_| "macOS Accessibility worker stopped")?
}

struct RetainedTarget {
    // Retain the process identity along with its element so that a PID
    // reused after app termination does not validate a stale target.
    application: Retained<NSRunningApplication>,
    element: Element,
}

struct Accessibility {
    next_target: u64,
    reads: VecDeque<HashMap<Target, RetainedTarget>>,
    attributes: Attributes,
    timeout_configured: bool,
}

impl Accessibility {
    fn new() -> Self {
        Self {
            next_target: 0,
            reads: VecDeque::new(),
            attributes: Attributes::new(|_| true),
            timeout_configured: false,
        }
    }

    fn require_access(&mut self) -> Result<(), String> {
        native::require_permission()?;
        if !self.timeout_configured {
            native::configure_timeout().map_err(|error| error.to_string())?;
            self.timeout_configured = true;
        }
        Ok(())
    }

    fn target(&self, target: Target) -> Result<&RetainedTarget, String> {
        let (age, target) = self
            .reads
            .iter()
            .rev()
            .enumerate()
            .find_map(|(age, read)| read.get(&target).map(|target| (age, target)))
            .ok_or("the accessibility reference expired or was never returned by readUI")?;
        // DEFENSE: A retained AX handle does not keep the application running.
        if !accessibility::reference_live(age, target.application.isTerminated()) {
            return Err("the accessibility reference is no longer live".into());
        }
        Ok(target)
    }

    fn read(
        &mut self,
        pid: Option<i32>,
        limits: ReadLimits,
        fields: ReadFields,
    ) -> Result<Value, String> {
        let application = match pid {
            Some(pid) => NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                .filter(|application| pid > 0 && !application.isTerminated())
                .ok_or_else(|| format!("no running application with PID {pid}"))?,
            None => frontmost_application()?,
        };
        let pid = application.processIdentifier();
        let ax = native::application(pid);
        let window = current_window(&ax).map_err(|error| error.to_string())?;
        let focused = native::element_attribute(&ax, "AXFocusedUIElement")
            .map_err(|error| error.to_string())?;
        let attributes = fields
            .0
            .as_ref()
            .map(|_| Attributes::new(|name| name == "role" || fields.includes(name)));
        let window_attributes = fields.0.as_ref().map(|_| {
            Attributes::new(|name| {
                matches!(name, "role" | "position" | "size" | "title") || fields.includes(name)
            })
        });
        let mut read = WindowRead {
            application,
            next_target: &mut self.next_target,
            attributes: attributes.as_ref().unwrap_or(&self.attributes),
            window_attributes: window_attributes.as_ref().unwrap_or(&self.attributes),
            fields,
            targets: HashMap::new(),
            remaining: limits.nodes,
            max_depth: limits.depth,
            truncated: false,
        };
        let mut window = window
            .map(|window| read.tree(window, 0))
            .transpose()
            .map_err(|error| error.to_string())?
            .flatten();
        let focused = focused
            .map(|focused| read.describe(focused, false))
            .transpose()
            .map_err(|error| error.to_string())?
            .flatten();
        if let Some(Value::Object(window)) = &mut window {
            if let Some(window_id) = window_id::find(pid, window) {
                window.insert("windowId".into(), json!(window_id));
            }
            for field in ["position", "size", "title"] {
                if !read.fields.includes(field) {
                    window.remove(field);
                }
            }
        }
        let result = json!({
            "pid": pid,
            "window": window,
            "focused": focused,
            "truncated": read.truncated,
        });
        self.reads.push_back(read.targets);
        if !accessibility::retained_read(self.reads.len() - 1) {
            self.reads.pop_front();
        }
        Ok(result)
    }
}

fn current_window(application: &Element) -> Result<Option<Element>, native::Error> {
    // Inactive applications can lack a focused window. The main window
    // identifies their primary document without changing OS focus.
    for name in ["AXFocusedWindow", "AXMainWindow"] {
        if let Some(window) = native::element_attribute(application, name)? {
            return Ok(Some(window));
        }
    }
    // An application with no primary window can still expose one concrete
    // candidate, for example a single window belonging to a background app.
    native::only_window(application)
}

fn frontmost_application() -> Result<Retained<NSRunningApplication>, String> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .filter(|application| !application.isTerminated() && application.processIdentifier() > 0)
        .ok_or_else(|| "no application currently receives keyboard events".into())
}

struct WindowRead<'a> {
    application: Retained<NSRunningApplication>,
    next_target: &'a mut u64,
    attributes: &'a Attributes,
    window_attributes: &'a Attributes,
    fields: ReadFields,
    targets: HashMap<Target, RetainedTarget>,
    remaining: usize,
    max_depth: usize,
    truncated: bool,
}

impl WindowRead<'_> {
    fn describe(
        &mut self,
        element: Element,
        window_root: bool,
    ) -> Result<Option<serde_json::Map<String, Value>>, native::Error> {
        match self.describe_live(element, window_root) {
            // DEFENSE: Applications can delete an element after its parent
            // was observed. Mark the result incomplete when that occurs.
            Err(error) if error.is_invalid_element() => {
                self.truncated = true;
                Ok(None)
            }
            result => result.map(Some),
        }
    }

    fn describe_live(
        &mut self,
        element: Element,
        window_root: bool,
    ) -> Result<serde_json::Map<String, Value>, native::Error> {
        let attributes = if window_root {
            self.window_attributes
        } else {
            self.attributes
        };
        let mut node = attributes.read(&element)?;
        let include_actions = self.fields.includes("actions");
        let include_settable = self.fields.includes("settable");
        if include_actions || include_settable {
            let settable = native::settable(&element, "AXValue")?;
            if include_actions {
                let mut actions = native::actions(&element)?;
                if settable {
                    actions.push("AXSetValue".into());
                }
                node.insert("actions".into(), json!(actions));
            }
            if include_settable {
                node.insert("settable".into(), json!(settable));
            }
        }
        let target = Target(*self.next_target);
        *self.next_target += 1;
        self.targets.insert(
            target,
            RetainedTarget {
                application: self.application.clone(),
                element,
            },
        );
        node.insert("target".into(), json!(target.name()));
        Ok(node)
    }

    fn tree(&mut self, element: Element, depth: usize) -> Result<Option<Value>, native::Error> {
        let Some(mut node) = self.describe(element.clone(), depth == 0)? else {
            return Ok(None);
        };
        self.remaining -= 1;
        match self.descendants(&element, depth) {
            Ok(children) => node.insert("children".into(), Value::Array(children)),
            // DEFENSE: A node can vanish after its attributes were read and
            // before the descendant query reaches the application.
            Err(error) if error.is_invalid_element() => {
                self.truncated = true;
                node.insert("children".into(), json!([]))
            }
            Err(error) => return Err(error),
        };
        Ok(Some(Value::Object(node)))
    }

    fn descendants(
        &mut self,
        element: &Element,
        depth: usize,
    ) -> Result<Vec<Value>, native::Error> {
        let count = native::child_count(element)?;
        if count == 0 {
            return Ok(Vec::new());
        }
        if depth == self.max_depth || self.remaining == 0 {
            self.truncated = true;
            return Ok(Vec::new());
        }
        let Some(elements) = native::children(element, self.remaining.min(count))? else {
            self.truncated = true;
            return Ok(Vec::new());
        };
        self.truncated |= elements.len() < count;
        let mut children = Vec::new();
        for child in elements {
            if self.remaining == 0 {
                self.truncated = true;
                break;
            }
            if let Some(child) = self.tree(child, depth + 1)? {
                children.push(child);
            }
        }
        Ok(children)
    }
}
