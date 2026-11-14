#![deny(unsafe_code)]

mod desktop;
mod input;
mod locked_use;

use std::{io, path::PathBuf, sync::mpsc, thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let helper = locked_use_helper()?;
    let (finish, finished) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(Into::into)
            .and_then(|runtime| {
                runtime.block_on(selvedge_script_server::serve(desktop::Desktop::new(helper)))
            });
        let _ = finish.send(result);
    });

    // ScreenCaptureKit can schedule WindowServer callbacks on the main run
    // loop while the MCP server and native calls wait on worker threads.
    loop {
        selvedge_macos_screen::pump_main_run_loop();
        match finished.recv_timeout(Duration::from_millis(5)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                server.join().expect("MCP server thread panicked");
                unreachable!("MCP server sends its result before exiting");
            }
        }
    }
}

fn locked_use_helper() -> io::Result<Option<PathBuf>> {
    let mut args = std::env::args_os().skip(1);
    let Some(option) = args.next() else {
        return Ok(None);
    };
    if option == "--locked-use-helper"
        && let Some(path) = args.next().map(PathBuf::from)
        && path.is_absolute()
        && args.next().is_none()
    {
        return Ok(Some(path));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Usage: selvedge-cua-mcp [--locked-use-helper /absolute/path]",
    ))
}
