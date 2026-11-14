//! Ownership and type checks at the Core Foundation and AX C API boundary.

use std::ptr::{self, NonNull};

use objc2_application_services::{
    AXCopyMultipleAttributeOptions, AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFNumber, CFRetained, CFString, CFType, CGPoint, CGSize, Type,
};
use serde_json::{Value, json};

pub type Element = CFRetained<AXUIElement>;
pub const SELECTED_TEXT: &str = "AXSelectedText";

#[derive(Debug)]
pub enum Error {
    System(AXError),
    UnexpectedResponse(&'static str),
}

impl Error {
    pub fn is_invalid_element(&self) -> bool {
        matches!(self, Self::System(AXError::InvalidUIElement))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System(AXError::InvalidUIElement) => {
                formatter.write_str("the accessibility element no longer exists")
            }
            Self::System(AXError::CannotComplete) => {
                formatter.write_str("the application did not complete the accessibility request")
            }
            Self::System(AXError::APIDisabled) => formatter.write_str(permission_message()),
            Self::System(AXError::ActionUnsupported) => {
                formatter.write_str("the accessibility action is unsupported")
            }
            Self::System(error) => write!(formatter, "macOS Accessibility error {}", error.0),
            Self::UnexpectedResponse(operation) => {
                write!(formatter, "{operation} returned an unexpected response")
            }
        }
    }
}

pub fn require_permission() -> Result<(), String> {
    // SAFETY: This is a parameterless process permission query.
    if unsafe { AXIsProcessTrusted() } {
        Ok(())
    } else {
        Err(permission_message().into())
    }
}

fn permission_message() -> &'static str {
    "macOS Accessibility permission is required"
}

pub fn application(pid: i32) -> Element {
    // SAFETY: The PID is supplied by NSRunningApplication.
    unsafe { AXUIElement::new_application(pid) }
}

pub fn configure_timeout() -> Result<(), Error> {
    // SAFETY: Creating the system-wide object takes no arguments. AX accepts
    // a positive timeout on this object and applies it to this process.
    unsafe { check(AXUIElement::new_system_wide().set_messaging_timeout(1.0)) }
}

fn check(error: AXError) -> Result<(), Error> {
    if error == AXError::Success {
        Ok(())
    } else {
        Err(Error::System(error))
    }
}

fn optional(error: AXError) -> Result<bool, Error> {
    match error {
        AXError::Success => Ok(true),
        // DEFENSE: AX attributes vary by application and role. Unsupported
        // attributes and attributes with no current value are omitted.
        AXError::NoValue | AXError::AttributeUnsupported | AXError::NotImplemented => Ok(false),
        _ => Err(Error::System(error)),
    }
}

fn copied<T: Type>(pointer: *const T, operation: &'static str) -> Result<CFRetained<T>, Error> {
    let pointer = NonNull::new(pointer.cast_mut()).ok_or(Error::UnexpectedResponse(operation))?;
    // SAFETY: The AX copy calls transfer one retained object on success.
    Ok(unsafe { CFRetained::from_raw(pointer) })
}

fn attribute(element: &AXUIElement, name: &str) -> Result<Option<CFRetained<CFType>>, Error> {
    let mut value = ptr::null();
    let name = CFString::from_str(name);
    // SAFETY: The pointer points to initialized, writable space for the result.
    let error = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut value)) };
    optional(error)?
        .then(|| copied(value, "AXUIElementCopyAttributeValue"))
        .transpose()
}

pub fn element_attribute(element: &AXUIElement, name: &str) -> Result<Option<Element>, Error> {
    attribute(element, name)?
        .map(|value| {
            value
                .downcast::<AXUIElement>()
                .map_err(|_| Error::UnexpectedResponse("AXUIElementCopyAttributeValue"))
        })
        .transpose()
}

pub fn only_window(application: &AXUIElement) -> Result<Option<Element>, Error> {
    let mut windows = windows(application)?;
    if windows.len() != 1 {
        return Ok(None);
    }
    let window = windows.remove(0);
    let minimized = attribute(&window, "AXMinimized")?.and_then(|value| boolean(&value));
    let is_window = role(&window)?.as_deref() == Some("AXWindow");
    Ok((is_window && minimized != Some(true)).then_some(window))
}

pub fn windows(application: &AXUIElement) -> Result<Vec<Element>, Error> {
    let Some(windows) = attribute(application, "AXWindows")? else {
        return Ok(Vec::new());
    };
    let windows = windows
        .downcast::<CFArray>()
        .map_err(|_| Error::UnexpectedResponse("AXWindows"))?;
    array(windows)
        .iter()
        .map(|element| {
            element
                .downcast::<AXUIElement>()
                .map_err(|_| Error::UnexpectedResponse("AXWindows"))
        })
        .collect()
}

pub fn containing_window(element: &AXUIElement) -> Result<Option<Element>, Error> {
    element_attribute(element, "AXWindow")
}

pub fn role(element: &AXUIElement) -> Result<Option<String>, Error> {
    Ok(attribute(element, "AXRole")?.and_then(|role| string(&role)))
}

pub fn enabled(element: &AXUIElement) -> Result<Option<bool>, Error> {
    Ok(attribute(element, "AXEnabled")?.and_then(|value| boolean(&value)))
}

pub fn settable(element: &AXUIElement, name: &str) -> Result<bool, Error> {
    let name = CFString::from_str(name);
    let mut value = 0;
    // SAFETY: The pointer points to writable space for a Core Foundation Boolean.
    let error = unsafe { element.is_attribute_settable(&name, NonNull::from(&mut value)) };
    Ok(optional(error)? && value != 0)
}

pub fn actions(element: &AXUIElement) -> Result<Vec<String>, Error> {
    let mut actions = ptr::null();
    // SAFETY: The pointer points to initialized, writable space for the result.
    let error = unsafe { element.copy_action_names(NonNull::from(&mut actions)) };
    match error {
        // DEFENSE: Some applications report ActionUnsupported for elements
        // that do not expose any actions.
        AXError::ActionUnsupported => Ok(Vec::new()),
        _ if optional(error)? => Ok(array(copied(actions, "AXUIElementCopyActionNames")?)
            .iter()
            .filter_map(|value| string(&value))
            .collect()),
        _ => Ok(Vec::new()),
    }
}

fn array(array: CFRetained<CFArray>) -> CFRetained<CFArray<CFType>> {
    // SAFETY: The AX copy calls used here return immutable arrays of CF objects.
    unsafe { CFRetained::cast_unchecked(array) }
}

pub fn child_count(element: &AXUIElement) -> Result<usize, Error> {
    let name = CFString::from_str("AXChildren");
    let mut count = 0;
    // SAFETY: The pointer points to writable space for the CFIndex result.
    let error = unsafe { element.attribute_value_count(&name, NonNull::from(&mut count)) };
    if optional(error)? {
        usize::try_from(count)
            .map_err(|_| Error::UnexpectedResponse("AXUIElementGetAttributeValueCount"))
    } else {
        Ok(0)
    }
}

pub fn children(element: &AXUIElement, limit: usize) -> Result<Option<Vec<Element>>, Error> {
    let name = CFString::from_str("AXChildren");
    let mut children = ptr::null();
    // SAFETY: The pointer points to initialized, writable space for the result.
    let error = unsafe {
        element.copy_attribute_values(&name, 0, limit as isize, NonNull::from(&mut children))
    };
    match error {
        // DEFENSE: The application can remove children between the count and
        // slice requests. AX reports IllegalArgument when the range shrinks.
        AXError::IllegalArgument | AXError::NoValue | AXError::AttributeUnsupported => Ok(None),
        _ => {
            check(error)?;
            array(copied(children, "AXUIElementCopyAttributeValues")?)
                .iter()
                .map(|value| {
                    value
                        .downcast::<AXUIElement>()
                        .map_err(|_| Error::UnexpectedResponse("AXUIElementCopyAttributeValues"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
    }
}

pub fn perform_action(element: &AXUIElement, action: &str) -> Result<(), Error> {
    // SAFETY: The native element and action string remain live for this call.
    check(unsafe { element.perform_action(&CFString::from_str(action)) })
}

pub fn set_value(element: &AXUIElement, value: &CFType) -> Result<(), Error> {
    // SAFETY: The argument is a live CF scalar. AX validates the value's class
    // against the requirements of the element's AXValue attribute.
    check(unsafe { element.set_attribute_value(&CFString::from_str("AXValue"), value) })
}

pub fn replace_selected_text(element: &AXUIElement, text: &CFString) -> Result<(), Error> {
    // SAFETY: The element and text remain live for this call. The application
    // handles the selection replacement through the specified element.
    check(unsafe { element.set_attribute_value(&CFString::from_str(SELECTED_TEXT), text.as_ref()) })
}

type Field = (&'static str, &'static str, fn(&CFType) -> Option<Value>);

const ATTRIBUTES: [Field; 11] = [
    ("AXRole", "role", text),
    ("AXSubrole", "subrole", text),
    ("AXTitle", "title", text),
    ("AXDescription", "description", text),
    ("AXValue", "value", scalar),
    ("AXPosition", "position", point),
    ("AXSize", "size", size),
    ("AXEnabled", "enabled", |value| {
        boolean(value).map(Value::Bool)
    }),
    ("AXIdentifier", "identifier", text),
    ("AXHelp", "help", text),
    ("AXPlaceholderValue", "placeholder", text),
];

pub struct Attributes {
    names: CFRetained<CFArray<CFString>>,
    fields: Vec<Field>,
}

pub fn is_attribute(field: &str) -> bool {
    ATTRIBUTES.iter().any(|&(_, name, _)| name == field)
}

impl Attributes {
    pub fn new(includes: impl Fn(&str) -> bool) -> Self {
        let fields: Vec<_> = ATTRIBUTES
            .into_iter()
            .filter(|&(_, name, _)| includes(name))
            .collect();
        let names: Vec<_> = fields
            .iter()
            .map(|&(name, _, _)| CFString::from_str(name))
            .collect();
        let refs: Vec<_> = names.iter().map(|name| &**name).collect();
        Self {
            names: CFArray::from_objects(&refs),
            fields,
        }
    }

    pub fn read(&self, element: &AXUIElement) -> Result<serde_json::Map<String, Value>, Error> {
        let mut values = ptr::null();
        // SAFETY: The input array contains CFStrings and the result pointer is writable.
        let error = unsafe {
            element.copy_multiple_attribute_values(
                self.names.as_opaque(),
                AXCopyMultipleAttributeOptions::empty(),
                NonNull::from(&mut values),
            )
        };
        check(error)?;
        let values = array(copied(values, "AXUIElementCopyMultipleAttributeValues")?);
        if values.len() != self.fields.len() {
            return Err(Error::UnexpectedResponse(
                "AXUIElementCopyMultipleAttributeValues",
            ));
        }
        let mut fields = serde_json::Map::new();
        for (&(_, name, decode), value) in self.fields.iter().zip(values.iter()) {
            // AX represents missing attributes as CFNull or an AXValue error.
            if let Some(value) = decode(&value) {
                fields.insert(name.into(), value);
            }
        }
        Ok(fields)
    }
}

fn string(value: &CFType) -> Option<String> {
    value.downcast_ref::<CFString>().map(ToString::to_string)
}

fn text(value: &CFType) -> Option<Value> {
    string(value).map(Value::String)
}

fn boolean(value: &CFType) -> Option<bool> {
    value.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool)
}

fn scalar(value: &CFType) -> Option<Value> {
    if let Some(string) = string(value) {
        return Some(Value::String(string));
    }
    if let Some(boolean) = boolean(value) {
        return Some(Value::Bool(boolean));
    }
    let number = value.downcast_ref::<CFNumber>()?;
    if number.is_float_type() {
        number
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
    } else {
        number.as_i64().map(Value::from)
    }
}

fn point(value: &CFType) -> Option<Value> {
    let value = value.downcast_ref::<AXValue>()?;
    let mut point = CGPoint::default();
    // SAFETY: The requested payload type matches the pointed-to storage.
    let success = unsafe { value.value(AXValueType::CGPoint, NonNull::from(&mut point).cast()) };
    success.then(|| json!({ "x": point.x, "y": point.y }))
}

fn size(value: &CFType) -> Option<Value> {
    let value = value.downcast_ref::<AXValue>()?;
    let mut size = CGSize::default();
    // SAFETY: The requested payload type matches the pointed-to storage.
    let success = unsafe { value.value(AXValueType::CGSize, NonNull::from(&mut size).cast()) };
    success.then(|| json!({ "width": size.width, "height": size.height }))
}
