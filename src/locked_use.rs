//! Desktop access for one MCP invocation. The external broker owns the
//! system authorization, display protection and relock watchdog.
use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use selvedge_rules::locked_use::{FinishReply, RunMode, confirmation_matches};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const MAX_REPLY: usize = 8 * 1024;
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
const ACTION_TIMEOUT: Duration = Duration::from_secs(5);
const RELOCK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct LockedUse {
    helper: Option<PathBuf>,
    runs: Mutex<Runs>,
}

#[derive(Default)]
struct Runs {
    current: Run,
    recovery_unconfirmed: bool,
}

#[derive(Default)]
enum Run {
    #[default]
    Idle,
    Active(Arc<Session>),
    Failed(String),
}

#[derive(Clone, Default)]
pub struct DesktopPermission(Option<Arc<Session>>);

struct Session {
    stream: Mutex<Helper>,
    mode: RunMode,
    revoked: AtomicBool,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ReplyState {
    Ordinary,
    Protected,
    Ok,
    Locked,
}

#[derive(Deserialize)]
struct Reply {
    state: Option<ReplyState>,
    error: Option<String>,
}

struct Helper {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl LockedUse {
    pub fn new(helper: Option<PathBuf>) -> Self {
        Self {
            helper,
            ..Self::default()
        }
    }

    pub async fn acquire(&self, cancel: &CancellationToken) -> Result<DesktopPermission, String> {
        let Some(program) = &self.helper else {
            return Ok(DesktopPermission::default());
        };
        let mut runs = self.runs.lock().await;
        if runs.recovery_unconfirmed {
            let error = "Locked Use did not confirm recovery; restart after verifying the desktop is locked".to_string();
            runs.current = Run::Failed(error.clone());
            return Err(error);
        }
        if cancel.is_cancelled() {
            return Err("Script was cancelled before acquiring desktop access".into());
        }
        match &runs.current {
            Run::Active(session) => return Ok(DesktopPermission(Some(Arc::clone(session)))),
            Run::Failed(error) => return Err(error.clone()),
            Run::Idle => {}
        }
        let mut helper = match Helper::spawn(program) {
            Ok(helper) => helper,
            Err(error) => {
                let error = format!("Starting Locked Use helper: {error}");
                runs.current = Run::Failed(error.clone());
                return Err(error);
            }
        };
        // Once acquisition has been requested, let the protected helper settle.
        // The native task is drained on cancellation before finish is called.
        let session = match helper.request("begin", ACQUIRE_TIMEOUT).await {
            Ok(ReplyState::Ordinary) => Ok(RunMode::Ordinary),
            Ok(ReplyState::Protected) => Ok(RunMode::Protected),
            Ok(_) => Err("Locked Use helper did not identify the desktop session".into()),
            Err(error) => Err(error),
        };
        match session {
            Ok(mode) => {
                let session = Arc::new(Session {
                    stream: Mutex::new(helper),
                    mode,
                    revoked: AtomicBool::new(false),
                });
                runs.current = Run::Active(Arc::clone(&session));
                if cancel.is_cancelled() {
                    return Err("Script was cancelled after acquiring desktop access".into());
                }
                Ok(DesktopPermission(Some(session)))
            }
            Err(error) => {
                // DEFENSE: The helper may have lost its reply after starting
                // acquisition. Require recovery before any later run can use it.
                let settled = helper.request("finish", RELOCK_TIMEOUT).await;
                let error = match settled {
                    Ok(ReplyState::Ok | ReplyState::Locked) => error,
                    _ => {
                        runs.recovery_unconfirmed = true;
                        format!("{error}; Locked Use recovery was not confirmed")
                    }
                };
                runs.current = Run::Failed(error.clone());
                Err(error)
            }
        }
    }

    pub async fn permission(&self) -> DesktopPermission {
        match &self.runs.lock().await.current {
            Run::Active(session) => DesktopPermission(Some(Arc::clone(session))),
            _ => DesktopPermission::default(),
        }
    }

    pub async fn finish(&self) -> Result<(), String> {
        let mut runs = self.runs.lock().await;
        let session = match std::mem::take(&mut runs.current) {
            Run::Idle => return Ok(()),
            Run::Failed(error) => return Err(error),
            Run::Active(session) => session,
        };
        let mut helper = session.stream.lock().await;
        let reply = match helper.receive("finish", RELOCK_TIMEOUT).await {
            Ok(reply) => reply,
            Err(error) => {
                runs.recovery_unconfirmed = true;
                return Err(error);
            }
        };
        let confirmation = match reply.state {
            Some(ReplyState::Ok) => Some(FinishReply::Ok),
            Some(ReplyState::Locked) => Some(FinishReply::Locked),
            _ => None,
        };
        if !confirmation.is_some_and(|reply| confirmation_matches(session.mode, reply)) {
            runs.recovery_unconfirmed = true;
            return Err(reply.error.unwrap_or_else(|| {
                "Locked Use helper did not confirm the expected final desktop state".into()
            }));
        }
        if let Some(error) = reply.error {
            return Err(format!("Locked Use finish: {error}"));
        }
        if session.revoked.load(Ordering::Acquire) {
            return Err("Locked Use authorization ended before the script completed".into());
        }
        Ok(())
    }
}

impl DesktopPermission {
    pub fn managed(&self) -> bool {
        self.0.is_some()
    }

    pub fn protected(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|session| session.mode == RunMode::Protected)
    }

    pub async fn check(&self) -> Result<(), String> {
        let Some(session) = &self.0 else {
            return Ok(());
        };
        let mut helper = session.stream.lock().await;
        if session.revoked.load(Ordering::Acquire) {
            return Err("Locked Use authorization has ended".into());
        }
        match helper.request("check", ACTION_TIMEOUT).await {
            Ok(ReplyState::Ok) => Ok(()),
            response => {
                session.revoked.store(true, Ordering::Release);
                Err(match response {
                    Ok(_) => "Locked Use helper did not confirm desktop authorization".into(),
                    Err(error) => error,
                })
            }
        }
    }
}

impl Helper {
    fn spawn(program: &Path) -> io::Result<Self> {
        let mut child = Command::new(program)
            .arg("--selvedge-locked-use-bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let input = child.stdin.take().expect("piped helper stdin");
        let output = BufReader::new(child.stdout.take().expect("piped helper stdout"));
        Ok(Self {
            child,
            input,
            output,
        })
    }

    async fn request(&mut self, action: &str, deadline: Duration) -> Result<ReplyState, String> {
        match self.receive(action, deadline).await? {
            Reply {
                error: Some(error), ..
            } => Err(format!("Locked Use {action}: {error}")),
            Reply {
                state: Some(state), ..
            } => Ok(state),
            _ => Err(format!("Locked Use {action}: missing helper reply state")),
        }
    }

    async fn receive(&mut self, action: &str, deadline: Duration) -> Result<Reply, String> {
        // DEFENSE: A stalled helper may be inside a native authorization call.
        // Drop the connection on timeout so the external watchdog can recover.
        match timeout(deadline, self.exchange(action)).await {
            Ok(result) => result.map_err(|error| format!("Locked Use {action}: {error}")),
            Err(_) => {
                let _ = self.child.start_kill();
                Err(format!("Locked Use {action} timed out"))
            }
        }
    }

    async fn exchange(&mut self, action: &str) -> Result<Reply, String> {
        self.input
            .write_all(action.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        self.input
            .write_all(b"\n")
            .await
            .map_err(|error| error.to_string())?;
        self.input
            .flush()
            .await
            .map_err(|error| error.to_string())?;
        let mut line = Vec::new();
        loop {
            let available = self
                .output
                .fill_buf()
                .await
                .map_err(|error| error.to_string())?;
            let end = available.iter().position(|byte| *byte == b'\n');
            let count = end.map_or(available.len(), |index| index + 1);
            // DEFENSE: The helper could exit mid-message or emit unbounded
            // output. Neither can establish authorization for a native effect.
            if count == 0 || count > MAX_REPLY - line.len() {
                return Err("Incomplete or oversized helper reply".into());
            }
            line.extend_from_slice(&available[..count]);
            self.output.consume(count);
            if end.is_some() {
                break;
            }
        }
        serde_json::from_slice(&line).map_err(|error| format!("Invalid helper reply: {error}"))
    }
}
