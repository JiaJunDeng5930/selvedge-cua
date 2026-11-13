#![deny(unsafe_code)]

mod desktop;
mod input;

use std::{sync::mpsc, thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (finish, finished) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(Into::into)
            .and_then(|runtime| {
                runtime.block_on(selvedge_script_server::serve(desktop::Desktop::new()))
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
