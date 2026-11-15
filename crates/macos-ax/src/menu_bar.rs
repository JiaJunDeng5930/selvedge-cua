use serde::Deserialize;
use serde_json::{Value, json};

use super::{TreeOptions, TreeRead, decode, native, running_application, submit};

#[derive(Deserialize)]
struct ReadMenuBar {
    pid: i32,
    #[serde(flatten)]
    tree: TreeOptions,
}

/// Reads the application's AXMenuBar without clicking or opening its menus.
pub fn read_menu_bar(options: &Value) -> Result<Value, String> {
    let options: ReadMenuBar = decode(options, "desktop.readMenuBar")?;
    let (limits, fields) = options.tree.validate("desktop.readMenuBar")?;
    let pid = options.pid;
    submit(move |accessibility| {
        let application = running_application(pid)?;
        let menu = native::element_attribute(&native::application(pid), "AXMenuBar")
            .map_err(|error| error.to_string())?;
        let Some(menu) = menu else {
            return Ok(json!({"pid": pid, "menuBar": null, "truncated": false}));
        };
        let mut read = TreeRead::new(
            application,
            &mut accessibility.next_target,
            &accessibility.attributes,
            limits,
            fields,
            None,
        );
        let menu = read.tree(menu, 0).map_err(|error| error.to_string())?;
        let truncated = read.truncated;
        let targets = read.targets;
        accessibility.retain_targets(targets);
        Ok(json!({"pid": pid, "menuBar": menu, "truncated": truncated}))
    })
}
