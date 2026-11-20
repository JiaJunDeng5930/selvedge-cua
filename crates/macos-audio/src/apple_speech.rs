use crate::{Event, TranscriptSegment};
use serde::Deserialize;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Message {
    Ready,
    Segment {
        #[serde(flatten)]
        segment: TranscriptSegment,
    },
    Error {
        message: String,
    },
}

/// The bundled helper receives only the selected process's PCM over stdin.
pub(crate) struct AppleSpeech {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl AppleSpeech {
    pub(crate) async fn prepare(
        language: &str,
        cancel: &CancellationToken,
    ) -> Result<Self, String> {
        let program = std::env::current_exe()
            .map_err(|error| format!("Could not locate the Apple speech helper: {error}"))?
            .with_file_name("selvedge-apple-speech");
        if !program.is_file() {
            return Err("The bundled Apple speech helper is unavailable".into());
        }
        let mut child = Command::new(program)
            .args(["--transcribe", language])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("Could not start the Apple speech helper: {error}"))?;
        let input = child.stdin.take().expect("piped Apple speech input");
        let mut output =
            BufReader::new(child.stdout.take().expect("piped Apple speech output")).lines();
        let ready = tokio::select! {
            _ = cancel.cancelled() => return Err("Audio observation was cancelled".into()),
            ready = tokio::time::timeout(Duration::from_secs(10), read_message(&mut output)) => {
                ready.map_err(|_| "Apple speech helper startup timed out")??
            }
        };
        match ready {
            Some(Message::Ready) => Ok(Self {
                child,
                input,
                output,
            }),
            Some(Message::Error { message }) => Err(message),
            _ => Err("The Apple speech helper exited before becoming ready".into()),
        }
    }

    pub(crate) async fn transcribe(
        self,
        mut pcm: mpsc::UnboundedReceiver<Vec<f32>>,
        sample_rate: u32,
        cancel: &CancellationToken,
        events: &mpsc::UnboundedSender<Event>,
    ) -> Result<(), String> {
        let Self {
            mut child,
            mut input,
            mut output,
        } = self;
        let write = async {
            let Some(first) = pcm.recv().await else {
                return Ok(());
            };
            input.write_all(&sample_rate.to_le_bytes()).await?;
            input.write_all(bytemuck::cast_slice(&first)).await?;
            while let Some(samples) = pcm.recv().await {
                // macOS targets are little-endian; the header and PCM use the same order.
                input.write_all(bytemuck::cast_slice(&samples)).await?;
            }
            input.shutdown().await?;
            drop(input);
            Ok::<(), std::io::Error>(())
        };
        let read = async {
            while let Some(message) = read_message(&mut output).await? {
                match message {
                    Message::Segment { segment } => {
                        if events.send(Event::Segment(segment)).is_err() {
                            return Ok(());
                        }
                    }
                    Message::Error { message } => return Err(message),
                    Message::Ready => {
                        return Err("Unexpected Apple speech helper startup message".into());
                    }
                }
            }
            Ok(())
        };
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                Ok(())
            }
            result = async {
                tokio::try_join!(
                    async { write.await.map_err(|error| format!("Apple speech PCM input failed: {error}")) },
                    read,
                )?;
                let status = child.wait().await
                    .map_err(|error| format!("Could not wait for the Apple speech helper: {error}"))?;
                if !status.success() {
                    return Err(format!("The Apple speech helper failed ({status})"));
                }
                Ok(())
            } => result,
        }
    }
}

async fn read_message(
    output: &mut Lines<BufReader<ChildStdout>>,
) -> Result<Option<Message>, String> {
    output
        .next_line()
        .await
        .map_err(|error| format!("Apple speech helper output failed: {error}"))?
        .map(|line| {
            serde_json::from_str(&line)
                .map_err(|error| format!("Invalid Apple speech helper output: {error}"))
        })
        .transpose()
}
