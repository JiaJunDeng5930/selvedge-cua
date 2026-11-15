use std::{env, fs::File, thread, time::Duration};

use objc2::{
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardTypeHTML,
    NSPasteboardTypeString,
};
use objc2_foundation::{NSArray, NSString};

pub enum PasteContent {
    Text(String),
    Html {
        text: String,
        plain_text: Option<String>,
    },
}

/// Loads a temporary clipboard item, sends paste and restores the saved items
/// only while the transaction still owns the clipboard.
pub fn paste<T>(
    content: PasteContent,
    send: impl FnOnce() -> Result<T, String>,
) -> Result<(T, bool), String> {
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(env::temp_dir().join("dev.selvedge.cua-mcp.clipboard.lock"))
        .map_err(|error| format!("Could not open the Selvedge clipboard lock: {error}"))?;
    // A Codex chat can start its own MCP process using the same pasteboard.
    lock.try_lock()
        .map_err(|error| format!("Could not acquire the Selvedge clipboard lock: {error}"))?;
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        let item = NSPasteboardItem::new();
        // SAFETY: These symbols are AppKit's immutable pasteboard type identifiers.
        let (plain, html) = unsafe { (NSPasteboardTypeString, NSPasteboardTypeHTML) };
        let encoded = match content {
            PasteContent::Text(text) => item.setString_forType(&NSString::from_str(&text), plain),
            PasteContent::Html { text, plain_text } => {
                item.setString_forType(&NSString::from_str(&text), html)
                    && plain_text.is_none_or(|text| {
                        item.setString_forType(&NSString::from_str(&text), plain)
                    })
            }
        };
        if !encoded {
            return Err("Could not encode the pasteboard item".into());
        }
        paste_on(&board, &item, send)
    })
}

fn paste_on<T>(
    board: &NSPasteboard,
    item: &NSPasteboardItem,
    send: impl FnOnce() -> Result<T, String>,
) -> Result<(T, bool), String> {
    let original_count = board.changeCount();
    let original = snapshot(board)?;
    // DEFENSE: A person or other app can copy while the snapshot is read.
    if board.changeCount() != original_count {
        return Err("The clipboard changed while saving its previous contents".into());
    }
    board.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    let written = write_items(board, &[item]);
    let owned_count = board.changeCount();
    let sent = if written {
        let sent = send();
        if sent.is_ok() {
            // KNOWN LIMITATION: Quartz posts asynchronously and cannot acknowledge
            // pasteboard consumption. Give the receiver a bounded delivery window.
            thread::sleep(Duration::from_millis(350));
        }
        sent
    } else {
        Err("Could not write the temporary pasteboard item".into())
    };
    let restored = if board.changeCount() == owned_count {
        board.clearContents();
        let items = original.iter().map(|item| &**item).collect::<Vec<_>>();
        if !write_items(board, &items) {
            return Err("Could not restore the saved clipboard items".into());
        }
        true
    } else {
        false
    };
    sent.map(|result| (result, restored))
}

fn snapshot(board: &NSPasteboard) -> Result<Vec<Retained<NSPasteboardItem>>, String> {
    let Some(items) = board.pasteboardItems() else {
        // DEFENSE: Reading an advertised pasteboard item can fail (for
        // example, a provider has exited). Preserve the board in that case.
        if board.types().is_some_and(|types| types.count() > 0) {
            return Err("Could not read the existing clipboard items".into());
        }
        return Ok(Vec::new());
    };
    items
        .to_vec()
        .into_iter()
        .map(|source| {
            let saved = NSPasteboardItem::new();
            for data_type in source.types().to_vec() {
                let data = source
                    .dataForType(&data_type)
                    .ok_or_else(|| format!("Could not save clipboard type {data_type}"))?;
                if !saved.setData_forType(&data, &data_type) {
                    return Err(format!("Could not save clipboard type {data_type}"));
                }
            }
            Ok(saved)
        })
        .collect()
}

fn write_items(board: &NSPasteboard, items: &[&NSPasteboardItem]) -> bool {
    items.is_empty()
        || board.writeObjects(&NSArray::from_slice(
            &items
                .iter()
                .map(|&item| ProtocolObject::from_ref(item))
                .collect::<Vec<_>>(),
        ))
}
