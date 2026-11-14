//! Input dispatch is serialized across scripts, while observation may read a
//! snapshot between events. The mutex pairs each native send with its Rocq
//! acknowledgement. It is released during timed waits.
use crate::locked_use::DesktopPermission;
use selvedge_macos_ax::{self as ax, InputProcess, InputWindow};
use selvedge_macos_input::{self as native, Point, WindowRoute};
use selvedge_rules::input::{Change, Held, State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

const MAX_DURATION_MS: f64 = 30_000.0;
const MAX_STEPS: usize = 1024;

#[derive(Default)]
pub struct Input {
    dispatch: tokio::sync::Mutex<()>,
    state: Mutex<DispatchState>,
}

#[derive(Default)]
struct DispatchState {
    rules: State,
    requires_permission: bool,
    running: bool,
    dispatch_epoch: u64,
    used_in_run: bool,
    drag_position: Option<Point>,
    target: Option<Target>,
}

#[derive(Clone, Copy)]
struct Target {
    process: InputProcess,
    window: Option<InputWindow>,
}

impl Target {
    fn window(id: u32) -> Result<Self, String> {
        let window = ax::input_window(id)?;
        Ok(Self {
            process: window.process(),
            window: Some(window),
        })
    }

    fn contains(self, point: Point) -> bool {
        let (x, y, width, height) = self.window.expect("a pointer target has a window").bounds();
        (x..x + width).contains(&point.x) && (y..y + height).contains(&point.y)
    }

    fn process(pid: i32) -> Result<Self, String> {
        Ok(Self {
            process: ax::input_process(pid)?,
            window: None,
        })
    }
}

#[derive(Clone, Copy)]
enum Event {
    Move(Point),
    Key { code: u16, pid: i32, down: bool },
    PressMouse { point: Point, button: u8 },
    Release(Held),
    Scroll { at: Point, dx: i32, dy: i32 },
}

struct Step {
    offset: Duration,
    event: Event,
}

struct Continuation {
    hold: bool,
    operation: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pointer {
    window_id: Option<u32>,
    #[serde(default)]
    samples: Vec<Position>,
    #[serde(default)]
    buttons: Vec<u8>,
    #[serde(default)]
    hold: bool,
    operation: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Position {
    at_ms: f64,
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Keyboard {
    pid: Option<i32>,
    #[serde(default)]
    events: Vec<KeyEvent>,
    #[serde(default)]
    hold: bool,
    operation: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KeyEvent {
    #[serde(default)]
    at_ms: f64,
    key: String,
    down: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scroll {
    window_id: Option<u32>,
    x: f64,
    y: f64,
    events: Vec<ScrollEvent>,
    operation: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScrollEvent {
    #[serde(default)]
    at_ms: f64,
    #[serde(default)]
    dx: i32,
    #[serde(default)]
    dy: i32,
}

impl Input {
    pub fn new(requires_permission: bool) -> Self {
        let input = Self::default();
        input.state().requires_permission = requires_permission;
        input
    }

    pub fn snapshot(&self) -> Value {
        self.state().snapshot()
    }

    pub fn observation_epoch(&self) -> Option<u64> {
        let epoch = self.state().dispatch_epoch;
        epoch.is_multiple_of(2).then_some(epoch)
    }

    pub async fn pointer(
        &self,
        plan: Pointer,
        clock: Instant,
        cancel: CancellationToken,
        permission: &DesktopPermission,
    ) -> Result<Value, String> {
        let _dispatch = self.dispatch.lock().await;
        if plan.samples.len() > MAX_STEPS {
            return Err(format!("At most {MAX_STEPS} pointer samples are allowed"));
        }
        // DEFENSE: JavaScript may supply an unsupported button or repeat one
        // already listed, which would otherwise send a partially valid drag.
        if plan.buttons.iter().any(|button| *button > 31) {
            return Err("Mouse button codes range from 0 to 31".into());
        }

        let target = if plan.samples.is_empty() && plan.window_id.is_none() {
            None
        } else {
            Some(self.window_target(plan.window_id, plan.operation)?)
        };
        let mut positions = plan.samples.into_iter();
        let mut steps = Vec::new();
        if let Some(first) = positions.next() {
            let start = point(first.x, first.y)?;
            let dragging = self
                .state()
                .rules
                .held()
                .any(|held| matches!(held, Held::Mouse(_)));
            if !dragging && !target.expect("a trajectory has a target").contains(start) {
                return Err("Pointer start lies outside the target window".into());
            }
            let start_offset = offset(first.at_ms)?;
            steps.push(Step {
                offset: start_offset,
                event: Event::Move(start),
            });

            for button in self.state().rules.new_pointer_presses(plan.buttons) {
                steps.push(Step {
                    offset: start_offset,
                    event: Event::PressMouse {
                        point: start,
                        button,
                    },
                });
            }
            for sample in positions {
                steps.push(Step {
                    offset: offset(sample.at_ms)?,
                    event: Event::Move(point(sample.x, sample.y)?),
                });
            }
        } else if !plan.buttons.is_empty() {
            return Err("Pointer buttons require a position function".into());
        }
        self.run(
            Continuation {
                hold: plan.hold,
                operation: plan.operation,
            },
            target,
            steps,
            clock,
            cancel,
            permission,
        )
        .await
    }

    pub async fn keyboard(
        &self,
        plan: Keyboard,
        clock: Instant,
        cancel: CancellationToken,
        permission: &DesktopPermission,
    ) -> Result<Value, String> {
        let _dispatch = self.dispatch.lock().await;
        if plan.events.len() > MAX_STEPS {
            return Err(format!("At most {MAX_STEPS} keyboard events are allowed"));
        }
        let active = self.active_target(plan.operation)?;
        let target = match plan.pid {
            Some(pid) => Some(Target::process(pid)?),
            None => active,
        };
        let mut steps = Vec::with_capacity(plan.events.len());
        if !plan.events.is_empty() {
            let pid = target
                .ok_or("Keyboard input requires an explicit PID or an active operation")?
                .process
                .pid();
            for key in plan.events {
                steps.push(Step {
                    offset: offset(key.at_ms)?,
                    event: Event::Key {
                        code: native::key_code(&key.key)?,
                        pid,
                        down: key.down,
                    },
                });
            }
        }
        self.run(
            Continuation {
                hold: plan.hold,
                operation: plan.operation,
            },
            target,
            steps,
            clock,
            cancel,
            permission,
        )
        .await
    }

    pub async fn scroll(
        &self,
        plan: Scroll,
        clock: Instant,
        cancel: CancellationToken,
        permission: &DesktopPermission,
    ) -> Result<Value, String> {
        let _dispatch = self.dispatch.lock().await;
        if plan.events.is_empty() || plan.events.len() > MAX_STEPS {
            return Err(format!("Scroll requires between 1 and {MAX_STEPS} events"));
        }
        let target = self.window_target(plan.window_id, plan.operation)?;
        let at = point(plan.x, plan.y)?;
        if !target.contains(at) {
            return Err("Scroll point lies outside the target window".into());
        }
        let steps = plan
            .events
            .into_iter()
            .map(|event| {
                Ok(Step {
                    offset: offset(event.at_ms)?,
                    event: Event::Scroll {
                        at,
                        dx: event.dx,
                        dy: event.dy,
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.run(
            Continuation {
                operation: plan.operation,
                hold: plan.operation.is_some(),
            },
            Some(target),
            steps,
            clock,
            cancel,
            permission,
        )
        .await
    }

    fn active_target(&self, reference: Option<u64>) -> Result<Option<Target>, String> {
        let state = self.state();
        state.rules.authorize(reference)?;
        Ok(reference.and(state.target))
    }

    fn window_target(
        &self,
        window_id: Option<u32>,
        reference: Option<u64>,
    ) -> Result<Target, String> {
        let active = self.active_target(reference)?;
        let bound = active.filter(|target| target.window.is_some());
        let target = match (bound, window_id) {
            (Some(target), None) => target,
            (Some(target), Some(id)) if target.window.is_some_and(|window| window.id() == id) => {
                target
            }
            (_, Some(id)) => Target::window(id)?,
            (None, None) => {
                return Err(
                    "Pointer and scroll require a windowId or a window-bound operation".into(),
                );
            }
        };
        target
            .window
            .expect("the target contains a window")
            .revalidate()?;
        Ok(target)
    }

    async fn run(
        &self,
        continuation: Continuation,
        target: Option<Target>,
        steps: Vec<Step>,
        clock: Instant,
        cancel: CancellationToken,
        permission: &DesktopPermission,
    ) -> Result<Value, String> {
        if cancel.is_cancelled() {
            return Err("Input was cancelled before dispatch".into());
        }
        {
            let mut state = self.state();
            let active = continuation.operation.and(state.target);
            if let (Some(active), Some(target)) = (active, target)
                && active.process.pid() == target.process.pid()
                && active.process != target.process
            {
                // DEFENSE: macOS can reuse the PID after the held target exits.
                return Err("The input operation's process has been replaced".into());
            }
            state.rules.begin(
                continuation.operation,
                target.map(|target| {
                    (
                        target.process.pid(),
                        target.window.map(|window| window.id()),
                    )
                }),
                steps.iter().map(|step| {
                    // Truncation preserves order for all representable Duration values.
                    (step.offset.as_nanos() as u64, step.event.change())
                }),
            )?;
            state.target = target.or(active).map(|mut target| {
                let bound = state.rules.window();
                if target.window.map(|window| window.id()) != bound {
                    target.window = active.and_then(|active| active.window);
                }
                target
            });
            state.used_in_run = true;
            state.running = true;
        }
        let started_at = Instant::now();
        let mut events = Vec::with_capacity(steps.len());
        let mut failure = None;
        for step in steps {
            let due = started_at + step.offset;
            if due > Instant::now() {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {}
                    _ = sleep_until(due) => {}
                }
            }
            if cancel.is_cancelled() {
                failure = Some("Input was cancelled during dispatch".into());
                break;
            }
            if let Err(error) = permission.check().await {
                failure = Some(error);
                break;
            }
            if cancel.is_cancelled() {
                failure = Some("Input was cancelled during dispatch".into());
                break;
            }
            match self.state().send(step.event) {
                Ok(()) => events.push(json!({
                    "scheduledMs": step.offset.as_secs_f64() * 1000.0,
                    "sentAtMs": clock.elapsed().as_secs_f64() * 1000.0
                })),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if cancel.is_cancelled() && failure.is_none() {
            failure = Some("Input was cancelled during dispatch".into());
        }
        let (releases, cleanup) = self
            .release(continuation.hold, failure.is_some(), clock, permission)
            .await;
        let mut state = self.state();
        state.running = false;
        let mut result = state.snapshot();
        result["startedAtMs"] = json!(started_at.duration_since(clock).as_secs_f64() * 1000.0);
        result["events"] = json!(events);
        result["releases"] = json!(releases);
        result["finishedAtMs"] = json!(clock.elapsed().as_secs_f64() * 1000.0);
        let failure = match (failure, cleanup) {
            (None, Ok(())) => return Ok(result),
            (Some(error), Ok(())) | (None, Err(error)) => error,
            (Some(error), Err(cleanup)) => format!("{error}; release failed: {cleanup}"),
        };
        Err(format!("{failure}; partial dispatch receipt: {result}"))
    }

    pub async fn exclusive<T>(&self, operation: impl Future<Output = T>) -> T {
        let _dispatch = self.dispatch.lock().await;
        // AX effects can become visible while the native call is still
        // pending. Odd epochs mark this open dispatch interval.
        self.state().dispatch_epoch += 1;
        let result = operation.await;
        self.state().dispatch_epoch += 1;
        result
    }

    pub async fn finish_run(
        &self,
        successful: bool,
        clock: Instant,
        permission: &DesktopPermission,
    ) -> Result<(), String> {
        let _dispatch = self.dispatch.lock().await;
        let used = std::mem::take(&mut self.state().used_in_run);
        if permission.managed() || (used && !successful) {
            let (_, cleanup) = self.release(false, true, clock, permission).await;
            if permission.managed() {
                self.state().rules.invalidate_held();
            }
            return cleanup;
        }
        Ok(())
    }

    async fn release(
        &self,
        hold: bool,
        failed: bool,
        clock: Instant,
        permission: &DesktopPermission,
    ) -> (Vec<Value>, Result<(), String>) {
        let controls = self.state().rules.releases(hold, failed);
        let mut receipts = Vec::new();
        let mut errors = Vec::new();
        for held in controls {
            if let Err(error) = permission.check().await {
                errors.push(error);
                break;
            }
            match self.state().release_control(held, clock) {
                Ok(receipt) => receipts.push(receipt),
                // DEFENSE: An application's target or event route can expire
                // after its original key-down or mouse-down succeeded.
                Err(error) => errors.push(error),
            }
        }
        let result = if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        };
        (receipts, result)
    }

    fn state(&self) -> MutexGuard<'_, DispatchState> {
        self.state.lock().expect("input dispatch mutex poisoned")
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let mut state = self.state();
        if state.requires_permission {
            return;
        }
        for held in state.rules.releases(false, true) {
            if let Err(error) = state.release_control(held, Instant::now()) {
                eprintln!("Input cleanup on shutdown failed: {error}");
            }
        }
    }
}

impl DispatchState {
    fn snapshot(&self) -> Value {
        let (mut mouse, mut keyboard) = (Vec::new(), Vec::new());
        for held in self.rules.held() {
            match held {
                Held::Mouse(button) => mouse.push(button),
                Held::Key { code, pid } => {
                    keyboard.push(json!({"key": key_name(code), "pid": pid}))
                }
            }
        }
        json!({
            "operation": self.rules.reference(),
            "running": self.running,
            "pid": self.rules.reference().and(self.target.map(|target| target.process.pid())),
            "windowId": self.rules.reference().and(self.rules.window()),
            "held": { "buttons": mouse, "keys": keyboard }
        })
    }

    fn release_control(&mut self, held: Held, clock: Instant) -> Result<Value, String> {
        self.send(Event::Release(held))?;
        Ok(json!({
            "control": match held {
                Held::Mouse(button) => json!({"button":button}),
                Held::Key {code,pid} => json!({"key":key_name(code),"pid":pid}),
            },
            "sentAtMs": clock.elapsed().as_secs_f64() * 1000.0
        }))
    }

    fn send(&mut self, event: Event) -> Result<(), String> {
        let mut buttons = Vec::new();
        let mut modifiers = Vec::new();
        for held in self.rules.held() {
            match held {
                Held::Mouse(button) => buttons.push(button),
                Held::Key { code, .. } => {
                    modifiers.push(key_name(code));
                }
            }
        }
        let drag_position = &mut self.drag_position;
        let group = self.rules.event_group();
        let target = self
            .target
            .expect("every dispatched event has a destination");
        let window = || {
            let window = target.window.expect("a pointer event has a window");
            let (x, y, _, _) = window.bounds();
            WindowRoute {
                pid: window.pid(),
                window_id: window.id(),
                x,
                y,
            }
        };
        let releasing = matches!(event, Event::Release(_));
        let mut posted = false;
        let result = self.rules.dispatch(event.change(), || {
            if !target.process.is_live() {
                // A dead process has no remaining background input state. A
                // release must never target a new process reusing its PID.
                return if releasing {
                    Ok(())
                } else {
                    Err("The input target process is no longer live".into())
                };
            }
            if matches!(
                event,
                Event::Move(_) | Event::PressMouse { .. } | Event::Scroll { .. }
            ) {
                target
                    .window
                    .expect("a pointing event has a window")
                    .revalidate()?;
            }
            let result = match event {
                Event::Move(point) => {
                    let window = window();
                    if buttons.is_empty() {
                        native::move_pointer_in_window(window, point, None, group, &modifiers)?;
                    } else {
                        for button in buttons {
                            native::move_pointer_in_window(
                                window,
                                point,
                                Some(button),
                                group,
                                &modifiers,
                            )?;
                        }
                        *drag_position = Some(point);
                    }
                    Ok(())
                }
                Event::PressMouse { point, button } => {
                    native::mouse_button_in_window(
                        window(),
                        point,
                        button,
                        true,
                        group,
                        &modifiers,
                    )?;
                    *drag_position = Some(point);
                    Ok(())
                }
                Event::Release(Held::Mouse(button)) => {
                    let point = drag_position.expect("a held mouse button has a posted position");
                    native::mouse_button_in_window(
                        window(),
                        point,
                        button,
                        false,
                        group,
                        &modifiers,
                    )
                }
                Event::Key { code, pid, down } => {
                    native::keyboard(pid, key_name(code), down, &modifiers)
                }
                Event::Release(Held::Key { code, pid }) => {
                    native::keyboard(pid, key_name(code), false, &modifiers)
                }
                Event::Scroll { at, dx, dy } => {
                    native::scroll_in_window(window(), at, dx, dy, &modifiers)
                }
            };
            posted = result.is_ok();
            result
        });
        self.dispatch_epoch += 2 * u64::from(posted);
        result
    }
}

impl Event {
    fn change(self) -> Change {
        match self {
            Self::Move(_) | Self::Scroll { .. } => Change::Unchanged,
            Self::PressMouse { button, .. } => Change::Press(Held::Mouse(button)),
            Self::Key { code, pid, down } => {
                let key = Held::Key { code, pid };
                if down {
                    Change::Press(key)
                } else {
                    Change::Release(key)
                }
            }
            Self::Release(held) => Change::Release(held),
        }
    }
}

fn key_name(code: u16) -> &'static str {
    native::key_name(code).expect("held key codes originate from the macOS key mapping")
}

fn offset(ms: f64) -> Result<Duration, String> {
    // DEFENSE: A script can supply a negative or unbounded delay.
    if !(0.0..=MAX_DURATION_MS).contains(&ms) {
        return Err(format!(
            "Input times must be between 0 and {MAX_DURATION_MS} ms"
        ));
    }
    Ok(Duration::from_secs_f64(ms / 1000.0))
}

fn point(x: f64, y: f64) -> Result<Point, String> {
    if x.is_finite() && y.is_finite() {
        Ok(Point { x, y })
    } else {
        Err("Pointer coordinates must be finite".into())
    }
}
