use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::model::{CallToolResult, ContentBlock};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Ctx, Exception, Function, Promise};
use serde_json::{Value, json};
use tokio::{
    sync::oneshot,
    task::JoinSet,
    time::{sleep_until, timeout_at},
};
use tokio_util::sync::CancellationToken;

use crate::{Desktop, Operation, Response};

const RUN_DURATION: Duration = Duration::from_secs(60);
const JS_MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const MAX_ACTIVE_CALLS: usize = 32;

pub(crate) struct Output {
    result: Result<Value, String>,
    images: Vec<Vec<u8>>,
    observations: Vec<Value>,
}

impl Output {
    pub(crate) fn failed(error: impl Into<String>) -> Self {
        Self {
            result: Err(error.into()),
            images: Vec::new(),
            observations: Vec::new(),
        }
    }

    pub(crate) fn into_tool_result(self) -> CallToolResult {
        let mut result = match self.result {
            Ok(result) => CallToolResult::structured(json!({
                "result": result,
                "observations": self.observations,
            })),
            Err(error) => CallToolResult::structured_error(json!({
                "error": error,
                "observations": self.observations,
            })),
        };
        result.content.extend(
            self.images
                .into_iter()
                .map(|png| ContentBlock::image(STANDARD.encode(png), "image/png")),
        );
        result
    }
}

type Reply = Result<Value, String>;

struct NativeCall {
    started: Option<oneshot::Receiver<Reply>>,
    finished: oneshot::Receiver<Reply>,
}

#[derive(Default)]
struct Work {
    tasks: JoinSet<()>,
    last_input: Option<oneshot::Receiver<()>>,
    observations: Vec<Option<Response>>,
}

struct Invocation<D> {
    desktop: Arc<D>,
    work: Mutex<Work>,
    cancel: CancellationToken,
}

impl<D: Desktop> Invocation<D> {
    fn start(self: &Arc<Self>, operation: Operation, options: Value) -> Result<NativeCall, String> {
        let mut work = self.work.lock().expect("native work lock");
        if self.cancel.is_cancelled() {
            return Err("Script was cancelled".into());
        }
        while let Some(result) = work.tasks.try_join_next() {
            // DEFENSE: The backend can panic while posting a native call.
            if let Err(error) = result {
                self.cancel.cancel();
                return Err(error.to_string());
            }
        }
        // DEFENSE: An unawaited loop can otherwise enqueue arbitrary native work.
        if work.tasks.len() >= MAX_ACTIVE_CALLS {
            return Err(format!(
                "At most {MAX_ACTIVE_CALLS} native calls can be active"
            ));
        }

        let (started_sender, started) = if operation == Operation::ObserveScreen {
            let (sender, receiver) = oneshot::channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let observation = started.is_some().then(|| {
            let index = work.observations.len();
            work.observations.push(None);
            index
        });
        // Desktop-changing effects follow script call order. Reads have an
        // independent schedule so the script can observe an ongoing action.
        let mut input_order = match operation {
            Operation::ObserveScreen | Operation::ReadUi => None,
            _ => {
                let (done, next) = oneshot::channel();
                Some((work.last_input.replace(next), done))
            }
        };
        let (result, finished) = oneshot::channel();
        let invocation = Arc::clone(self);
        work.tasks.spawn(async move {
            if let Some((Some(previous), _)) = &mut input_order {
                tokio::select! {
                    _ = invocation.cancel.cancelled() => return,
                    result = previous => {
                        // DEFENSE: A panicking backend drops its completion
                        // sender before the join is observed.
                        if result.is_err() {
                            invocation.cancel.cancel();
                            return;
                        }
                    }
                }
            }
            if invocation.cancel.is_cancelled() {
                return;
            }

            let reply = invocation
                .desktop
                .call(
                    operation,
                    options,
                    started_sender,
                    invocation.cancel.clone(),
                )
                .await;
            let reply = match (observation, reply) {
                (Some(index), Ok(mut response)) => {
                    let Some(metadata) = response.value.as_object_mut() else {
                        // DEFENSE: Desktop is public and a third-party backend may supply a scalar.
                        let _ = result
                            .send(Err("Screen observation metadata must be an object".into()));
                        return;
                    };
                    metadata.insert("observationId".into(), json!(index));
                    let metadata = response.value.clone();
                    invocation
                        .work
                        .lock()
                        .expect("native work lock")
                        .observations[index] = Some(response);
                    Ok(metadata)
                }
                (_, reply) => reply.map(|response| response.value),
            };
            let _ = result.send(reply);
            if let Some((_, done)) = input_order {
                let _ = done.send(());
            }
        });
        Ok(NativeCall { started, finished })
    }

    async fn drain(&self) -> Result<(), String> {
        let mut tasks = {
            let mut work = self.work.lock().expect("native work lock");
            std::mem::take(&mut work.tasks)
        };
        let mut outcome = Ok(());
        while let Some(result) = tasks.join_next().await {
            // DEFENSE: Stop queued work after an adapter panics, and join every
            // native call still in flight before cleanup.
            if let Err(error) = result {
                self.cancel.cancel();
                outcome = Err(error.to_string());
            }
        }
        outcome
    }

    fn output(&self, result: Result<Value, String>) -> Output {
        let observations = {
            let mut work = self.work.lock().expect("native work lock");
            std::mem::take(&mut work.observations)
        };
        let mut output = Output {
            result,
            observations: Vec::new(),
            images: Vec::new(),
        };
        for observation in observations.into_iter().flatten() {
            let image_offset = output.images.len();
            let image_count = observation.images.len();
            let mut metadata = observation.value;
            metadata["imageOffset"] = json!(image_offset);
            metadata["imageCount"] = json!(image_count);
            output.observations.push(metadata);
            output.images.extend(observation.images);
        }
        output
    }
}

pub(crate) async fn execute<D: Desktop>(
    desktop: Arc<D>,
    code: String,
    cancel: CancellationToken,
) -> Output {
    let invocation = Arc::new(Invocation {
        desktop,
        work: Mutex::new(Work::default()),
        cancel: cancel.child_token(),
    });
    let deadline = Instant::now() + RUN_DURATION;
    let end = deadline.into();
    let mut result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Script was cancelled".into()),
        result = timeout_at(end, evaluate(Arc::clone(&invocation), code, deadline)) => {
            // The QuickJS interrupt can return before the async timer is polled.
            if deadline <= Instant::now() {
                Err("Script exceeded its 60 second deadline".into())
            } else {
                result.unwrap_or_else(|_| Err("Script exceeded its 60 second deadline".into()))
            }
        }
    };
    if result.is_err() {
        invocation.cancel.cancel();
    }

    // Timeouts and cancellation reject queued calls and notify the active
    // backend. An acknowledgement already in progress must settle before cleanup.
    let drain = invocation.drain();
    tokio::pin!(drain);
    let drained = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            result = Err("Script was cancelled".into());
            invocation.cancel.cancel();
            drain.await
        }
        _ = sleep_until(end) => {
            result = Err("Script exceeded its 60 second deadline".into());
            invocation.cancel.cancel();
            drain.await
        }
        result = &mut drain => result,
    };
    result = result.and_then(|value| drained.map(|()| value));
    if let Err(error) = invocation.desktop.finish_run(result.is_ok()).await {
        // DEFENSE: Native key-up can fail if input posting becomes unavailable.
        result = Err(match result {
            Ok(_) => error,
            Err(script_error) => format!("{script_error}; desktop cleanup failed: {error}"),
        });
    }
    invocation.output(result)
}

async fn evaluate<D: Desktop>(
    invocation: Arc<Invocation<D>>,
    code: String,
    deadline: Instant,
) -> Result<Value, String> {
    let runtime = AsyncRuntime::new().map_err(|error| error.to_string())?;
    runtime.set_memory_limit(JS_MEMORY_LIMIT).await;
    let cancel = invocation.cancel.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || {
            deadline <= Instant::now() || cancel.is_cancelled()
        })))
        .await;
    let context = AsyncContext::full(&runtime)
        .await
        .map_err(|error| error.to_string())?;
    context
        .async_with(async move |ctx| {
            bind(&ctx, invocation)
                .catch(&ctx)
                .map_err(|error| error.to_string())?;
            let source = format!("(async () => {{\n{code}\n}})()");
            let promise: Promise = ctx
                .eval(source)
                .catch(&ctx)
                .map_err(|error| error.to_string())?;
            let result = promise
                .into_future::<rquickjs::Value>()
                .await
                .catch(&ctx)
                .map_err(|error| error.to_string())?;
            let json = ctx
                .json_stringify(result)
                .catch(&ctx)
                .map_err(|error| error.to_string())?
                .map(|json| json.to_string())
                .transpose()
                .map_err(|error| error.to_string())?;
            match json {
                Some(json) => serde_json::from_str(&json).map_err(|error| error.to_string()),
                None => Ok(Value::Null),
            }
        })
        .await
}

fn bind<'js, D: Desktop>(ctx: &Ctx<'js>, invocation: Arc<Invocation<D>>) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    for (name, operation) in [
        ("observeScreen", Operation::ObserveScreen),
        ("readUI", Operation::ReadUi),
        ("pointer", Operation::Pointer),
        ("keyboard", Operation::Keyboard),
        ("scroll", Operation::Scroll),
        ("insertText", Operation::InsertText),
        ("performUIAction", Operation::PerformUiAction),
    ] {
        let function = binding(ctx, Arc::clone(&invocation), operation)?;
        let function = if operation == Operation::Pointer {
            let make_pointer: Function = ctx.eval(include_str!("pointer.js"))?;
            let desktop = Arc::clone(&invocation.desktop);
            let offsets = Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, duration: f64, interval: f64| {
                    desktop
                        .pointer_offsets(duration, interval)
                        .map_err(|error| Exception::throw_range(&ctx, &error))
                },
            )?;
            make_pointer.call((function, offsets))?
        } else {
            function
        };
        globals.set(name, function.with_name(name)?)?;
    }
    Ok(())
}

fn binding<'js, D: Desktop>(
    ctx: &Ctx<'js>,
    invocation: Arc<Invocation<D>>,
    operation: Operation,
) -> rquickjs::Result<Function<'js>> {
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>,
              input: rquickjs::function::Opt<rquickjs::Value<'js>>|
              -> rquickjs::Result<Promise<'js>> {
            let call = input
                .0
                .filter(|value| !value.is_undefined())
                .map_or_else(|| Ok(json!({})), |input| options(&ctx, input))
                .and_then(|options| invocation.start(operation, options));
            let call = match call {
                Ok(call) => call,
                Err(error) => {
                    let finished = reject(&ctx, &error)?;
                    if operation == Operation::ObserveScreen {
                        finished.set("started", finished.clone())?;
                        finished.set("finished", finished.clone())?;
                    }
                    return Ok(finished);
                }
            };

            let finished = promise(&ctx, call.finished)?;
            if let Some(started) = call.started {
                finished.set("started", promise(&ctx, started)?)?;
                finished.set("finished", finished.clone())?;
            }
            Ok(finished)
        },
    )
}

fn options<'js>(ctx: &Ctx<'js>, input: rquickjs::Value<'js>) -> Result<Value, String> {
    let value = ctx
        .json_stringify(input)
        .catch(ctx)
        .map_err(|error| error.to_string())?
        .ok_or("Operation options must be a JSON object")?
        .to_string()
        .map_err(|error| error.to_string())?;
    let options: Value = serde_json::from_str(&value).map_err(|error| error.to_string())?;
    if !options.is_object() {
        return Err("Operation options must be a JSON object".into());
    }
    Ok(options)
}

fn reject<'js>(ctx: &Ctx<'js>, error: &str) -> rquickjs::Result<Promise<'js>> {
    let (promise, _, reject) = Promise::new(ctx)?;
    reject.call::<_, ()>((Exception::from_message(ctx.clone(), error)?,))?;
    Ok(promise)
}

fn promise<'js>(ctx: &Ctx<'js>, reply: oneshot::Receiver<Reply>) -> rquickjs::Result<Promise<'js>> {
    let context = ctx.clone();
    Promise::wrap_future(ctx, async move {
        let response = reply
            .await
            .unwrap_or_else(|_| Err("Native call ended without a reply".into()));
        match response {
            Ok(value) => context.json_parse(value.to_string()),
            Err(error) => Err(Exception::throw_message(&context, &error)),
        }
    })
}
