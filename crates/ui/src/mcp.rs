//! MCP (Model Context Protocol) integration for GPUI apps.
//!
//! Starts an IPC server inside the running app so an AI agent — through the
//! separate `gpui-mcp-server` binary — can look at the UI and drive it: read
//! what is on screen, click, type, press keys, dispatch named actions, take
//! screenshots, and read a state snapshot the app defines. The agent checks a
//! UI change the way a person would, by using the app.
//!
//! The prose version of all this, aimed at someone integrating the feature,
//! is the *MCP Inspector* page in `docs/`. The agent-facing documentation is
//! served by the server itself, from its `gpui_guide` tool.
//!
//! ## Usage
//!
//! ```ignore
//! fn main() {
//!     let app = Application::new();
//!     app.run(|cx| {
//!         gpui_component::init(cx);
//!         // Pick a stable identifier for your app — the gpui-mcp-server
//!         // uses it to target this app specifically when multiple GPUI
//!         // apps are running.
//!         gpui_component::mcp::init_mcp(cx, "my-app");
//!         // ... app code ...
//!     });
//! }
//! ```
//!
//! ## Socket naming
//!
//! The socket is created at `{temp_dir}/gpui-mcp-{app_name}-{pid}.sock`.
//! Including both the app name and the PID lets multiple GPUI apps — and
//! multiple instances of the same app — coexist without collision, while
//! still allowing the `gpui-mcp-server` to discover and filter by app.
//!
//! ## How a request is answered
//!
//! A listener thread accepts a connection, reads one newline-delimited JSON
//! request, and hands it to the GPUI main thread, which wakes for it — an
//! idle app does no work here. Requests are answered one at a time, in
//! arrival order: an agent drives one thing at a time, and letting a later
//! call overtake a [`methods::WAIT_FOR`] would reorder inputs it meant as a
//! sequence.
//!
//! ## The frame contract
//!
//! Everything readable here — the element tree, the snapshot, screenshots —
//! comes from the **last painted frame**, because that is where gpui's
//! inspector data lives. An answer assembled straight after a click would
//! therefore describe the app *before* the click.
//!
//! So the input methods do not answer until the frame showing their effect
//! has been painted, and `settled` says whether that frame arrived. What this
//! cannot cover is work the app starts on its own — an async load, a
//! debounce, an animation — which is what [`methods::WAIT_FOR`] is for.
//!
//! ## Naming things
//!
//! [`methods::UI_SNAPSHOT`] prints one line per element that means something.
//! An element earns its line by having a role, an id somebody wrote, or text;
//! the rest is layout scaffolding and is dropped, its children taking its
//! place. Roles come from the file that rendered the element — one widget per
//! file means `button/button.rs` renders a `button` — so an app on this crate
//! gets a semantic vocabulary without annotating anything.
//!
//! ## Security
//!
//! This gives anything that can reach the socket full control of the UI and a
//! view of its state. It is a development tool: keep it behind a feature flag
//! and never enable it in a shipped build.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

use futures::channel::oneshot;
use gpui::{App, AsyncApp, Keystroke, MouseButton as GpuiMouseButton, Pixels, point, px};
use gpui_mcp_protocol::protocol::*;
use serde_json::json;

/// Maximum number of stored log entries
const MAX_LOG_ENTRIES: usize = 500;

/// Distinguishes the temp files screenshots are handed over in, so two taken
/// in one batch cannot overwrite each other.
static SCREENSHOT_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A request on its way to the main thread, with the channel its answer goes
/// back on. The reply channel stays a blocking `std` one: it is received on
/// the connection thread, which has nothing else to do.
type RequestMsg = (IpcRequest, mpsc::Sender<IpcResponse>);

/// How long the connection thread waits for the main thread before giving up.
///
/// Generous on purpose: `wait_for` may legitimately hold a request open for
/// [`MAX_WAIT_MS`] and a batch for [`MAX_BATCH_MS`]. This is the backstop for
/// a wedged main thread, not a policy on how long work may take.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for one frame callback before deciding none is coming.
///
/// A window that is occluded, minimised or otherwise not being drawn will
/// never call back. Reporting that as `settled: false` is honest and costs a
/// fifth of a second; hanging until the request times out is neither.
const FRAME_TIMEOUT: Duration = Duration::from_millis(200);

/// Global log buffer, thread-safe
static LOG_BUFFER: std::sync::LazyLock<Arc<Mutex<VecDeque<String>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))));

/// App-specific state provider callback.
/// Registered once at startup via `mcp_set_app_state_provider`.
static APP_STATE_PROVIDER: std::sync::LazyLock<
    Mutex<Option<Box<dyn Fn(&App) -> serde_json::Value + Send>>>,
> = std::sync::LazyLock::new(|| Mutex::new(None));

fn px_to_f32(p: Pixels) -> f32 {
    f32::from(p)
}

fn convert_bounds(b: gpui::Bounds<Pixels>) -> Bounds {
    Bounds {
        x: px_to_f32(b.origin.x),
        y: px_to_f32(b.origin.y),
        width: px_to_f32(b.size.width),
        height: px_to_f32(b.size.height),
    }
}

/// Register an app-specific state provider for the MCP `get_app_state` tool.
///
/// The callback receives `&App` and should return a JSON value describing the
/// application's semantic state. It runs on the main thread whenever
/// `get_app_state` is called. Only one provider can be registered; calling
/// this again replaces the previous one.
pub fn mcp_set_app_state_provider(provider: impl Fn(&App) -> serde_json::Value + Send + 'static) {
    if let Ok(mut guard) = APP_STATE_PROVIDER.lock() {
        *guard = Some(Box::new(provider));
    }
}

/// Add a log entry (can be called from anywhere)
pub fn mcp_log(message: impl Into<String>) {
    if let Ok(mut buffer) = LOG_BUFFER.lock() {
        if buffer.len() >= MAX_LOG_ENTRIES {
            buffer.pop_front();
        }
        buffer.push_back(message.into());
    }
}

/// Sanitize an app name for use in a socket filename.
///
/// Allowed characters: `[a-zA-Z0-9_-]`. Anything else is replaced with `_`.
/// An empty result falls back to `"gpui-app"`.
fn sanitize_app_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "gpui-app".to_string()
    } else {
        cleaned
    }
}

/// Returns the socket path for the given app and current process.
///
/// Format: `{tmp_dir}/gpui-mcp-{app_name}-{pid}.sock`.
fn socket_path_for(app_name: &str) -> String {
    let sanitized = sanitize_app_name(app_name);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    dir.join(format!("gpui-mcp-{}-{}.sock", sanitized, pid))
        .to_string_lossy()
        .into_owned()
}

/// Initialize the MCP IPC server for this GPUI app.
///
/// `app_name` should be a stable identifier for the application (e.g.
/// `"elane"`, `"my-editor"`). It is used to namespace the socket file so
/// the `gpui-mcp-server` can discover and filter by app when multiple GPUI
/// apps are running at the same time.
///
/// Starts a Unix Domain Socket listener on a background thread. Requests are
/// answered on the GPUI main thread, which the listener wakes; an idle app
/// does no work for the MCP server at all.
pub fn init_mcp(cx: &mut App, app_name: &str) {
    let socket_path = socket_path_for(app_name);

    // Async and unbounded: the listener thread hands a request over and the
    // main thread's task is woken by it. The old arrangement polled a
    // `try_recv` every 10 ms, which cost an idle app a hundred main-loop
    // wakeups a second and still added up to 10 ms to every call.
    let (req_tx, req_rx) = async_channel::unbounded::<RequestMsg>();

    // Start IPC server on background thread
    let path = socket_path.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_ipc_listener(&path, req_tx) {
            eprintln!("[MCP] IPC Server error: {}", e);
        }
    });

    mcp_log(format!("MCP IPC Server started on {}", socket_path));
    eprintln!("[MCP] IPC Server listening on {}", socket_path);

    // Requests are answered on the main thread, one at a time and in arrival
    // order. Sequential on purpose: a `wait_for` suspends without blocking the
    // thread, and letting a later call overtake it would reorder inputs the
    // agent meant as a sequence.
    cx.spawn(async move |cx| {
        while let Ok((request, resp_tx)) = req_rx.recv().await {
            let response = respond(request, cx).await;
            let _ = resp_tx.send(response);
        }
    })
    .detach();
}

/// Unix Socket listener loop (runs on background thread)
fn run_ipc_listener(
    socket_path: &str,
    req_tx: async_channel::Sender<RequestMsg>,
) -> anyhow::Result<()> {
    // Remove old socket
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let tx = req_tx.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_ipc_connection(stream, tx) {
                        eprintln!("[MCP] Connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("[MCP] Accept error: {}", e);
            }
        }
    }

    Ok(())
}

/// Handle a single IPC connection (runs on connection thread)
fn handle_ipc_connection(
    stream: UnixStream,
    req_tx: async_channel::Sender<RequestMsg>,
) -> anyhow::Result<()> {
    let reader = BufReader::new(&stream);
    let mut writer = &stream;

    for line in reader.lines() {
        let line = line?;
        let request: IpcRequest = serde_json::from_str(&line)?;

        let (resp_tx, resp_rx) = mpsc::channel();

        let request_id = request.id.clone();

        req_tx
            .send_blocking((request, resp_tx))
            .map_err(|e| anyhow::anyhow!("Failed to send request to main thread: {}", e))?;

        let response = resp_rx.recv_timeout(RESPONSE_TIMEOUT).unwrap_or_else(|_| {
            IpcResponse::new(
                request_id,
                Err(format!(
                    "The app's main thread did not answer within {}s. It is blocked, \
                     in a modal loop, or busy.",
                    RESPONSE_TIMEOUT.as_secs()
                )),
            )
        });

        let response_json = serde_json::to_string(&response)?;
        writer.write_all(response_json.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    Ok(())
}

/// Answer one IPC request.
///
/// Async because the honest answer to an input is not available at the moment
/// the input returns: gpui has dispatched it, but nothing has been drawn, and
/// the element tree, the focus chain and every screenshot are read from the
/// last *painted* frame. Waiting for that frame here is the difference between
/// an agent seeing its click take effect and asking again — and asking again
/// costs it a whole model turn, thousands of times what the frame costs.
async fn respond(request: IpcRequest, cx: &AsyncApp) -> IpcResponse {
    // Refuse before dispatching: this app and the server that called it are
    // built from one crate but by different mechanisms, so they can drift
    // apart, and a silently misread request is worse than a named refusal.
    // The complaint travels back as a normal error, which is what puts it in
    // front of whoever is driving the app.
    if let Some(complaint) = version_complaint(
        request.protocol_version,
        "MCP server",
        "`cargo build --release` inside the gpui-mcp checkout",
    ) {
        return IpcResponse::new(request.id, Err(complaint));
    }

    let result = if request.method == methods::BATCH {
        run_batch(&request.params, cx).await
    } else {
        dispatch(&request.method, &request.params, true, cx).await
    };

    IpcResponse::new(request.id, result)
}

/// Run one method, waiting for a painted frame when the method changed
/// something.
///
/// `with_state` decides whether the answer carries the post-dispatch app state
/// and focus info. A standalone call wants it — that is the round trip it
/// saves. A batch step does not: the batch attaches it once, at the end,
/// instead of repeating it after every step.
async fn dispatch(
    method: &str,
    params: &serde_json::Value,
    with_state: bool,
    cx: &AsyncApp,
) -> Result<serde_json::Value, String> {
    if method == methods::BATCH {
        return Err("A batch cannot contain another batch.".into());
    }
    if method == methods::WAIT_FOR {
        return run_wait_for(params, with_state, cx).await;
    }
    if method == methods::A11Y_TREE {
        return run_a11y_tree(params, cx).await;
    }

    let window_id = params
        .get("window_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let result = cx.update(|cx| dispatch_sync(method, params, cx))?;

    if !is_input_method(method) {
        return Ok(result);
    }

    let settled = settle(window_id.as_deref(), cx).await;
    if !with_state {
        return Ok(result);
    }

    Ok(cx.update(|cx| attach_post_state(result, window_id.as_deref(), settled, cx)))
}

/// Methods that change the app, and whose answer therefore has to describe the
/// frame after the change rather than the one before it.
fn is_input_method(method: &str) -> bool {
    matches!(
        method,
        methods::CLICK_ELEMENT | methods::SEND_KEY | methods::TYPE_TEXT | methods::EXECUTE_ACTION
    )
}

/// The methods that can be answered from the frame already on screen.
fn dispatch_sync(
    method: &str,
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    match method {
        methods::GET_WINDOWS => handle_get_windows(cx),
        methods::CLICK_ELEMENT => handle_click_element(params, cx),
        methods::SEND_KEY => handle_send_key(params, cx),
        methods::GET_APP_STATE => handle_get_app_state(cx),
        methods::GET_LOGS => handle_get_logs(),
        methods::UI_SNAPSHOT => handle_ui_snapshot(params, cx),
        methods::A11Y_AUDIT => handle_a11y_audit(params, cx),
        methods::INSPECT_UI_TREE => handle_inspect_ui_tree(params, cx),
        methods::GET_ELEMENT => handle_get_element(params, cx),
        methods::TAKE_SCREENSHOT => handle_take_screenshot(params, cx),
        methods::EXECUTE_ACTION => handle_execute_action(params, cx),
        methods::LIST_ACTIONS => handle_list_actions(params, cx),
        methods::GET_FOCUS_INFO => handle_get_focus_info(params, cx),
        methods::TYPE_TEXT => handle_type_text(params, cx),
        // All three are answered by `dispatch`, which never sends them here.
        methods::WAIT_FOR | methods::BATCH | methods::A11Y_TREE => {
            Err(format!("{method} is answered asynchronously"))
        }
        _ => Err(format!("Unknown method: {}", method)),
    }
}

/// Wait for the frame that shows what was just dispatched.
///
/// `on_next_frame` callbacks run at the *start* of a frame request, before the
/// draw that request performs — so a single callback still sees the previous
/// `rendered_frame`. Two of them bracket exactly one completed draw, and that
/// is the draw carrying the input.
///
/// `false` means no frame arrived. That is reported rather than waited out: an
/// occluded or stalled window would otherwise hold the request open for
/// nothing.
async fn settle(window_id: Option<&str>, cx: &AsyncApp) -> bool {
    // Only the first await asks for a draw: it is the one that has to make
    // sure a draw happens at all, even for an input that dirtied nothing. The
    // second is just waiting for the frame request after that draw.
    next_frame(window_id, true, cx).await && next_frame(window_id, false, cx).await
}

/// Await one frame callback of the target window, or [`FRAME_TIMEOUT`].
///
/// `force_draw` marks the window dirty first. Worth it when something must be
/// painted before the answer is honest; not worth it while waiting, where it
/// would re-render the whole window on every frame for as long as the wait
/// lasts. A wait does not need it: whatever changes the condition marks the
/// window dirty by itself.
async fn next_frame(window_id: Option<&str>, force_draw: bool, cx: &AsyncApp) -> bool {
    let (tx, rx) = oneshot::channel::<()>();

    let registered = cx.update(|cx| {
        let handle = resolve_window(window_id, cx)?;
        handle
            .update(cx, |_, window, _| {
                if force_draw {
                    window.refresh();
                }
                window.on_next_frame(move |_, _| {
                    let _ = tx.send(());
                });
            })
            .map_err(|e| e.to_string())
    });

    if registered.is_err() {
        return false;
    }

    let timer = cx.background_executor().timer(FRAME_TIMEOUT);
    match futures::future::select(Box::pin(rx), Box::pin(timer)).await {
        // The sender is dropped rather than fired when the window goes away
        // mid-wait, which is a frame that never happened, not one that did.
        futures::future::Either::Left((delivered, _)) => delivered.is_ok(),
        futures::future::Either::Right(_) => false,
    }
}

/// Wait until the app looks the way the caller says it should.
///
/// The alternative is the agent looking again and again from the outside,
/// which costs a model turn per look. Here a look costs one frame callback.
/// Running out of time is not an error: `satisfied: false` is a fact, and
/// often the very fact being asked for.
async fn run_wait_for(
    params: &serde_json::Value,
    with_state: bool,
    cx: &AsyncApp,
) -> Result<serde_json::Value, String> {
    let opts: WaitForParams = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    let timeout = Duration::from_millis(
        opts.timeout_ms
            .unwrap_or(DEFAULT_WAIT_MS)
            .clamp(1, MAX_WAIT_MS),
    );
    let started = Instant::now();
    let mut frames = 0usize;
    let mut settled = true;

    // No condition at all means "give me a settled frame" — useful after
    // something the app started on its own.
    if !has_conditions(&opts) {
        settled = settle(opts.window_id.as_deref(), cx).await;
        frames = 2;
    }

    loop {
        let (satisfied, checks) = cx.update(|cx| evaluate_wait(&opts, cx))?;
        let expired = started.elapsed() >= timeout;

        if satisfied || expired {
            let mut answer = json!({
                "satisfied": satisfied,
                "waited_ms": started.elapsed().as_millis() as u64,
                "frames": frames,
                "settled": settled,
                "checks": checks,
            });
            if !satisfied {
                answer["timeout_ms"] = json!(timeout.as_millis() as u64);
                answer["hint"] = json!(
                    "Nothing failed here — the condition did not hold in time. `checks` \
                     says which part is missing."
                );
            }
            if with_state {
                answer = cx
                    .update(|cx| attach_post_state(answer, opts.window_id.as_deref(), settled, cx));
            }
            return Ok(answer);
        }

        // `next_frame` paces this loop by itself: it returns on the frame
        // callback, or after FRAME_TIMEOUT when no frames are coming.
        settled = next_frame(opts.window_id.as_deref(), false, cx).await;
        if settled {
            frames += 1;
        }
    }
}

/// Whether a `wait_for` asks about anything at all.
fn has_conditions(opts: &WaitForParams) -> bool {
    opts.element_id.is_some()
        || opts.text.is_some()
        || opts.key_context.is_some()
        || opts.app_state_path.is_some()
}

/// Check a `wait_for`'s conditions against the frame on screen right now.
///
/// Returns whether they hold together, and a breakdown per condition — which
/// is what makes running out of time actionable rather than merely
/// disappointing.
fn evaluate_wait(opts: &WaitForParams, cx: &mut App) -> Result<(bool, serde_json::Value), String> {
    let mut checks = serde_json::Map::new();
    let mut holds = true;

    if opts.element_id.is_some() || opts.text.is_some() {
        let query = opts.element_id.as_deref().map(expand_ref).transpose()?;
        let needle = opts.text.as_ref().map(|text| text.to_lowercase());

        let (element_found, text_found) =
            with_elements(opts.window_id.as_deref(), cx, |elements, window_id| {
                let mut element_found = false;
                let mut text_found = false;
                for info in elements {
                    if let (Some(query), false) = (&query, element_found) {
                        let full_id =
                            format!("{}/{}[{}]", window_id, info.global_id, info.instance_id);
                        element_found = id_matches(&full_id, &info.global_id, query);
                    }
                    if let (Some(needle), false) = (&needle, text_found) {
                        text_found = info
                            .text_content
                            .iter()
                            .any(|line| line.to_lowercase().contains(needle));
                    }
                }
                (element_found, text_found)
            })?;

        if let Some(query) = &opts.element_id {
            checks.insert(
                "element_id".into(),
                json!({ "query": query, "found": element_found }),
            );
            holds &= element_found;
        }
        if let Some(text) = &opts.text {
            checks.insert("text".into(), json!({ "query": text, "found": text_found }));
            holds &= text_found;
        }
    }

    if let Some(context) = &opts.key_context {
        let needle = context.to_lowercase();
        let focus = handle_get_focus_info(&json!({ "window_id": opts.window_id }), cx)?;
        let active = focus["key_contexts"].as_array().is_some_and(|contexts| {
            contexts.iter().any(|entry| {
                entry
                    .as_str()
                    .is_some_and(|entry| entry.to_lowercase().contains(&needle))
            })
        });
        checks.insert(
            "key_context".into(),
            json!({ "query": context, "active": active }),
        );
        holds &= active;
    }

    if let Some(path) = &opts.app_state_path {
        let state = handle_get_app_state(cx)?;
        let found = state.pointer(path).cloned();
        let matches = match (&opts.app_state_equals, &found) {
            (Some(expected), Some(actual)) => expected == actual,
            // A pointer that resolves to nothing satisfies nothing, including
            // "any value at all": the caller is waiting for the app to put
            // something there.
            (_, None) => false,
            (None, Some(actual)) => !actual.is_null(),
        };
        checks.insert(
            "app_state".into(),
            json!({
                "path": path,
                "value": found,
                "expected": opts.app_state_equals,
                "matches": matches,
            }),
        );
        holds &= matches;
    }

    let satisfied = if opts.absent { !holds } else { holds };
    Ok((satisfied, serde_json::Value::Object(checks)))
}

/// Run several methods inside one request.
///
/// Nothing here is faster than sending the steps one at a time — the socket
/// was never the slow part. What it saves is model turns: five steps sent
/// separately cost five round trips through the agent, sent together they cost
/// one.
async fn run_batch(params: &serde_json::Value, cx: &AsyncApp) -> Result<serde_json::Value, String> {
    let opts: BatchParams = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    if opts.steps.is_empty() {
        return Err("A batch needs at least one step.".into());
    }
    if opts.steps.len() > MAX_BATCH_STEPS {
        return Err(format!(
            "A batch takes at most {} steps, this one has {}.",
            MAX_BATCH_STEPS,
            opts.steps.len()
        ));
    }

    let deadline = Instant::now() + Duration::from_millis(MAX_BATCH_MS);
    let mut results = Vec::with_capacity(opts.steps.len());
    let mut failed = false;

    for step in &opts.steps {
        if Instant::now() >= deadline {
            results.push(json!({
                "method": step.method,
                "ok": false,
                "error": format!(
                    "The batch deadline of {}ms passed before this step ran.",
                    MAX_BATCH_MS
                ),
            }));
            failed = true;
            break;
        }

        let step_params = with_window_default(&step.params, opts.window_id.as_deref());
        match dispatch(&step.method, &step_params, false, cx).await {
            Ok(result) => results.push(json!({
                "method": step.method,
                "ok": true,
                "result": result,
            })),
            Err(error) => {
                results.push(json!({
                    "method": step.method,
                    "ok": false,
                    "error": error,
                }));
                failed = true;
                if opts.stop_on_error {
                    break;
                }
            }
        }
    }

    let answer = json!({
        "ok": !failed,
        "ran": results.len(),
        "of": opts.steps.len(),
        "steps": results,
    });

    let settled = settle(opts.window_id.as_deref(), cx).await;
    Ok(cx.update(|cx| attach_post_state(answer, opts.window_id.as_deref(), settled, cx)))
}

/// Give a batch step the batch's window unless it named one itself.
fn with_window_default(params: &serde_json::Value, window_id: Option<&str>) -> serde_json::Value {
    let Some(window_id) = window_id else {
        return params.clone();
    };

    let mut params = params.clone();
    match params.as_object_mut() {
        Some(object) => {
            object
                .entry("window_id")
                .or_insert_with(|| json!(window_id));
        }
        // A step with no params at all still belongs to the batch's window.
        None => params = json!({ "window_id": window_id }),
    }
    params
}

/// The three id forms every tool accepts: the full id, the global id, or a
/// suffix of the global id — first match winning.
///
/// Also accepts an id copied out of `format: "compact"` output, where crate
/// paths have been stripped from each segment and the `[instance]` suffix is
/// still attached. Refusing those would punish an agent for using the cheap
/// output format this server recommends, and "element not found" for an id
/// this very server just printed is the most confusing answer available.
fn id_matches(full_id: &str, global_id: &str, query: &str) -> bool {
    if full_id == query || global_id == query || global_id.ends_with(query) {
        return true;
    }
    if shorten_element_id(full_id) == query {
        return true;
    }

    let path = strip_id_decoration(query);
    let shortened = shorten_element_id(global_id);
    global_id == path || global_id.ends_with(path) || shortened == path || shortened.ends_with(path)
}

/// Drop a leading `WindowId(..)/` and a trailing `[instance]` from an id, so
/// what is left can be compared against a `global_id`.
fn strip_id_decoration(id: &str) -> &str {
    let without_window = id.find('/').map(|index| &id[index + 1..]).unwrap_or(id);
    without_window
        .rfind('[')
        .map(|index| &without_window[..index])
        .unwrap_or(without_window)
}

/// Run `f` over the inspector elements of the target window, which are the
/// elements of the frame last painted.
fn with_elements<T>(
    window_id: Option<&str>,
    cx: &mut App,
    f: impl FnOnce(&[gpui::InspectorElementInfo], &str) -> T,
) -> Result<T, String> {
    let handle = resolve_window(window_id, cx)?;
    let window_id = format!("{:?}", handle.window_id());

    handle
        .update(cx, |_, window, _| {
            f(&window.inspector_elements(), &window_id)
        })
        .map_err(|e| e.to_string())
}

// ===== Helpers =====

/// Resolve a window handle from an optional window_id string.
/// Falls back to: active window → first window.
fn resolve_window(window_id: Option<&str>, cx: &mut App) -> Result<gpui::AnyWindowHandle, String> {
    if let Some(id_str) = window_id {
        for handle in cx.windows() {
            let wid = format!("{:?}", handle.window_id());
            if wid == id_str {
                return Ok(handle);
            }
        }
        return Err(format!("Window not found: {}", id_str));
    }

    if let Some(handle) = cx.active_window() {
        return Ok(handle);
    }

    cx.windows()
        .into_iter()
        .next()
        .ok_or_else(|| "No windows available".to_string())
}

/// Returns the id of the window that driver tools will target when no
/// explicit window_id is provided. Priority mirrors `resolve_window(None, _)`:
/// OS-focused window → first window → None.
///
/// This is what MCP consumers actually care about for the `active_window` /
/// `is_active` fields: "which window will my commands hit?" — which differs
/// from `cx.active_window()` when the app itself is OS-backgrounded (e.g.
/// an Elane window exists but the LLM's terminal has OS focus). In that
/// case `cx.active_window()` returns None but the first available window
/// is still the correct dispatch target.
fn default_target_window_id(cx: &mut App) -> Option<gpui::WindowId> {
    if let Some(handle) = cx.active_window() {
        return Some(handle.window_id());
    }
    cx.windows().into_iter().next().map(|h| h.window_id())
}

/// Extract the last `.`-separated segment of a `global_id` — the actual
/// element name without its full ancestry path. e.g.
/// `"view-1.window_border.WindowBorder.backdrop.root.table"` → `"table"`.
fn short_name_of(global_id: &str) -> &str {
    global_id.rsplit('.').next().unwrap_or(global_id)
}

/// Collect up to `limit` elements that loosely match `query`, suggested
/// when an exact element lookup fails.
///
/// Matches case-insensitively on either the element's `short_name` (the
/// leaf segment of its global_id) or its rendered `text_content`. Results
/// are deduplicated by `short_name` so the LLM isn't drowning in repeats
/// of the same element type. The LLM can use the returned `short_name`
/// directly as a suffix match to retry the original call.
fn collect_match_candidates(
    query: &str,
    window_id: Option<&str>,
    cx: &mut App,
    limit: usize,
) -> Vec<serde_json::Value> {
    let query_lower = query.to_lowercase();
    let windows: Vec<gpui::AnyWindowHandle> = if let Some(wid) = window_id {
        cx.windows()
            .into_iter()
            .filter(|h| format!("{:?}", h.window_id()) == wid)
            .collect()
    } else {
        cx.windows()
    };

    let mut candidates: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for handle in &windows {
        if candidates.len() >= limit {
            break;
        }
        let found = handle
            .update(cx, |_, window, _cx| {
                let mut batch: Vec<(String, serde_json::Value)> = Vec::new();
                for info in window.inspector_elements() {
                    let short = short_name_of(&info.global_id).to_string();
                    let short_lower = short.to_lowercase();
                    let gid_lower = info.global_id.to_lowercase();
                    let text_joined = info.text_content.join(" ");
                    let text_lower = text_joined.to_lowercase();

                    // Prioritize leaf-name matches (most actionable for the
                    // LLM), then full-path matches (catches type-parameter
                    // names like FileTableDelegate), then text matches.
                    let short_match = short_lower.contains(&query_lower);
                    let path_match = !short_match && gid_lower.contains(&query_lower);
                    let text_match = !short_match
                        && !path_match
                        && !text_joined.is_empty()
                        && text_lower.contains(&query_lower);

                    if short_match || path_match || text_match {
                        let matched_on = if short_match {
                            "name"
                        } else if path_match {
                            "path"
                        } else {
                            "text"
                        };
                        batch.push((
                            short,
                            json!({
                                "text": if text_joined.is_empty() {
                                    serde_json::Value::Null
                                } else {
                                    json!(text_joined)
                                },
                                "matched_on": matched_on,
                            }),
                        ));
                    }
                }
                batch
            })
            .unwrap_or_default();

        for (short_name, details) in found {
            if candidates.len() >= limit {
                break;
            }
            if !seen.insert(short_name.clone()) {
                continue;
            }
            let mut entry = details;
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("short_name".into(), json!(short_name));
            }
            candidates.push(entry);
        }
    }
    candidates
}

/// Format an element-not-found error with candidate suggestions embedded
/// as a JSON payload. The LLM can parse the JSON after the "Error: "
/// prefix that `handle_tool_call` adds to failed responses.
fn not_found_error(query: &str, candidates: Vec<serde_json::Value>) -> String {
    if candidates.is_empty() {
        return format!("Element not found: {}", query);
    }
    json!({
        "message": format!("Element not found: {}", query),
        "candidates": candidates,
    })
    .to_string()
}

/// Attach post-dispatch state (`app_state` + `focus_info`) to a driver
/// response.
///
/// The MCP driver handlers (`execute_action`, `send_key`, `click_element`,
/// `type_text`) are almost always followed by a `get_app_state` +
/// `get_focus_info` call to answer "what changed?". Inlining both into the
/// driver response saves that round trip — and since `dispatch` only calls
/// this once the frame carrying the input has been painted, what it inlines is
/// the state *after* the input rather than the state it replaced.
///
/// `window_id` is the window the action targeted; it's used to scope
/// `focus_info` to the same window. Pass `None` to use the default target.
/// `settled` says whether that frame actually arrived: `false` means the
/// window is not being drawn, and everything here describes an older frame.
fn attach_post_state(
    mut response: serde_json::Value,
    window_id: Option<&str>,
    settled: bool,
    cx: &mut App,
) -> serde_json::Value {
    let app_state = handle_get_app_state(cx).unwrap_or(serde_json::Value::Null);

    let focus_params = json!({ "window_id": window_id });
    let focus_info = handle_get_focus_info(&focus_params, cx).unwrap_or(serde_json::Value::Null);

    if let Some(obj) = response.as_object_mut() {
        obj.insert("settled".into(), json!(settled));
        obj.insert("app_state".into(), app_state);
        obj.insert("focus_info".into(), focus_info);
    }

    response
}

// ===== Handler Implementations =====

fn handle_get_windows(cx: &mut App) -> Result<serde_json::Value, String> {
    // `is_active` reports "this window is the default dispatch target",
    // not "this window has OS focus". See `default_target_window_id`.
    let active_window_id = default_target_window_id(cx);

    let windows: Vec<WindowInfo> = cx
        .windows()
        .iter()
        .filter_map(|handle| {
            handle
                .update(cx, |_, window, _cx| {
                    let bounds = window.bounds();
                    WindowInfo {
                        id: format!("{:?}", handle.window_id()),
                        title: window.window_title(),
                        bounds: convert_bounds(bounds),
                        is_active: active_window_id == Some(handle.window_id()),
                        display_id: None,
                    }
                })
                .ok()
        })
        .collect();

    serde_json::to_value(&windows).map_err(|e| e.to_string())
}

fn handle_click_element(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let event: ClickEvent = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    let button = match event.button {
        MouseButton::Left => GpuiMouseButton::Left,
        MouseButton::Right => GpuiMouseButton::Right,
        MouseButton::Middle => GpuiMouseButton::Middle,
    };

    // If element_id is provided, resolve its bounds center
    let (position, resolved_id) = if let Some(ref element_id) = event.element_id {
        let (pos, id) = resolve_element_center(element_id, event.window_id.as_deref(), cx)?;
        (pos, Some(id))
    } else {
        (point(px(event.x), px(event.y)), None)
    };

    let handle = resolve_window(event.window_id.as_deref(), cx)?;

    handle
        .update(cx, |_, window, cx| {
            window.dispatch_click(position, button, cx);
        })
        .map_err(|e| e.to_string())?;

    let x = f32::from(position.x);
    let y = f32::from(position.y);
    if let Some(id) = &resolved_id {
        mcp_log(format!(
            "Click element '{}' at ({}, {}) button={:?}",
            id, x, y, event.button
        ));
    } else {
        mcp_log(format!("Click at ({}, {}) button={:?}", x, y, event.button));
    }

    let mut result = json!({ "success": true, "x": x, "y": y });
    if let Some(id) = resolved_id {
        result
            .as_object_mut()
            .map(|o| o.insert("resolved_element".into(), json!(id)));
    }
    Ok(result)
}

/// Resolve the center point of an element by ID.
/// Searches all windows (or a specific one) for the element and returns its bounds center.
fn resolve_element_center(
    query: &str,
    window_id: Option<&str>,
    cx: &mut App,
) -> Result<(gpui::Point<Pixels>, String), String> {
    let query = expand_ref(query)?;
    let query = query.as_str();
    let windows: Vec<gpui::AnyWindowHandle> = if let Some(wid) = window_id {
        cx.windows()
            .into_iter()
            .filter(|h| format!("{:?}", h.window_id()) == wid)
            .collect()
    } else {
        cx.windows()
    };

    for handle in &windows {
        let result = handle.update(cx, |_, window, _cx| {
            let window_id_str = format!("{:?}", handle.window_id());
            for info in window.inspector_elements() {
                let full_id = format!("{}/{}[{}]", window_id_str, info.global_id, info.instance_id);

                if id_matches(&full_id, &info.global_id, query) {
                    let center_x = info.bounds.origin.x + info.bounds.size.width / 2.0;
                    let center_y = info.bounds.origin.y + info.bounds.size.height / 2.0;
                    return Some((point(center_x, center_y), full_id));
                }
            }
            None
        });

        if let Ok(Some((pos, id))) = result {
            return Ok((pos, id));
        }
    }

    let candidates = collect_match_candidates(query, window_id, cx, 5);
    Err(not_found_error(query, candidates))
}

fn handle_send_key(params: &serde_json::Value, cx: &mut App) -> Result<serde_json::Value, String> {
    let event: KeyEvent = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    let mut keystroke_str = String::new();
    if event.modifiers.ctrl {
        keystroke_str.push_str("ctrl-");
    }
    if event.modifiers.alt {
        keystroke_str.push_str("alt-");
    }
    if event.modifiers.shift {
        keystroke_str.push_str("shift-");
    }
    if event.modifiers.meta {
        keystroke_str.push_str("cmd-");
    }
    keystroke_str.push_str(&event.key);

    let keystroke = Keystroke::parse(&keystroke_str).map_err(|e| format!("{:?}", e))?;

    // Use resolve_window() for consistent fallback behavior with the other
    // driver handlers (click/type/screenshot/execute_action). This lets the
    // LLM drive the app even when it's OS-backgrounded — the app's own
    // window is still a valid dispatch target.
    let handle = resolve_window(event.window_id.as_deref(), cx)?;

    let dispatched = handle
        .update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx))
        .map_err(|e| e.to_string())?;

    mcp_log(format!("Key '{}' dispatched={}", keystroke_str, dispatched));
    let response = json!({
        "success": true,
        "dispatched": dispatched,
        "keystroke": keystroke_str,
    });
    Ok(response)
}

fn handle_type_text(params: &serde_json::Value, cx: &mut App) -> Result<serde_json::Value, String> {
    let opts: TypeTextParams = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;

    let mut dispatched_count = 0usize;
    for ch in opts.text.chars() {
        let keystroke_str = match ch {
            ' ' => "space".to_string(),
            '\n' => "enter".to_string(),
            '\t' => "tab".to_string(),
            c => c.to_string(),
        };

        let keystroke = match Keystroke::parse(&keystroke_str) {
            Ok(k) => k,
            Err(_) => continue,
        };

        let ok = handle
            .update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx))
            .map_err(|e| e.to_string())?;

        if ok {
            dispatched_count += 1;
        }
    }

    mcp_log(format!(
        "Typed {} chars ({} dispatched)",
        opts.text.len(),
        dispatched_count
    ));
    let response = json!({
        "success": true,
        "text": opts.text,
        "chars": opts.text.len(),
        "dispatched": dispatched_count,
    });
    Ok(response)
}

fn handle_get_app_state(cx: &mut App) -> Result<serde_json::Value, String> {
    // `active_window` reports the default dispatch target, which falls back
    // to "first window" when the app is OS-backgrounded. See `default_target_window_id`.
    let active_window_id = default_target_window_id(cx).map(|id| format!("{:?}", id));
    let window_count = cx.windows().len();

    let windows: Vec<serde_json::Value> = cx
        .windows()
        .iter()
        .filter_map(|handle| {
            handle
                .update(cx, |_, window, _cx| {
                    let bounds = convert_bounds(window.bounds());
                    json!({
                        "id": format!("{:?}", handle.window_id()),
                        "title": window.window_title(),
                        "bounds": bounds,
                    })
                })
                .ok()
        })
        .collect();

    let mut result = json!({
        "window_count": window_count,
        "active_window": active_window_id,
        "windows": windows,
    });

    // Merge app-specific semantic state if a provider is registered
    if let Ok(guard) = APP_STATE_PROVIDER.lock() {
        if let Some(provider) = guard.as_ref() {
            let app_state = provider(cx);
            if let Some(obj) = result.as_object_mut() {
                obj.insert("app".into(), app_state);
            }
        }
    }

    Ok(result)
}

fn handle_get_logs() -> Result<serde_json::Value, String> {
    let logs: Vec<String> = LOG_BUFFER
        .lock()
        .map(|buffer| buffer.iter().cloned().collect())
        .unwrap_or_default();

    Ok(json!({ "logs": logs, "count": logs.len() }))
}

fn handle_inspect_ui_tree(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: InspectUiTreeParams = serde_json::from_value(params.clone()).unwrap_or_default();

    // Expand a `@ref` from the last snapshot before anything looks for it.
    let root_element_id = opts
        .root_element_id
        .as_deref()
        .map(expand_ref)
        .transpose()?;

    let compact = opts.format.as_deref() == Some("compact");
    // `is_active` reports default dispatch target, not OS focus. See `default_target_window_id`.
    let active_window_id = default_target_window_id(cx);

    let windows: Vec<gpui::AnyWindowHandle> = if let Some(ref wid) = opts.window_id {
        // Only the requested window
        cx.windows()
            .into_iter()
            .filter(|h| format!("{:?}", h.window_id()) == *wid)
            .collect()
    } else {
        cx.windows()
    };

    let children: Vec<UiElement> = windows
        .iter()
        .filter_map(|handle| {
            handle
                .update(cx, |_, window, _cx| {
                    let bounds = window.bounds();
                    let converted = convert_bounds(bounds);
                    let window_id_str = format!("{:?}", handle.window_id());

                    let inspector_elems = window.inspector_elements();
                    let mut element_children = build_element_tree(&window_id_str, inspector_elems);

                    // If root_element_id is set, find that subtree
                    if let Some(ref root_id) = root_element_id {
                        element_children = find_subtree(&element_children, root_id)
                            .map(|e| vec![e])
                            .unwrap_or_default();
                    }

                    // Apply depth limit (elements at depth 1 are window children)
                    if opts.max_depth > 0 {
                        truncate_tree(&mut element_children, 1, opts.max_depth);
                    }

                    // Apply type filter
                    if let Some(ref filter) = opts.element_type_filter {
                        let filter_lower = filter.to_lowercase();
                        filter_tree(&mut element_children, &filter_lower);
                    }

                    // Apply text content filter
                    if let Some(ref text_filter) = opts.text_filter {
                        let filter_lower = text_filter.to_lowercase();
                        filter_tree_by_text(&mut element_children, &filter_lower);
                    }

                    // Apply compact format
                    if compact {
                        for child in &mut element_children {
                            strip_verbose_fields(child);
                        }
                    }

                    UiElement {
                        id: window_id_str,
                        element_type: "Window".to_string(),
                        bounds: if compact {
                            Bounds {
                                x: 0.0,
                                y: 0.0,
                                width: 0.0,
                                height: 0.0,
                            }
                        } else {
                            converted.clone()
                        },
                        visible: true,
                        children: element_children,
                        properties: {
                            let mut props = std::collections::HashMap::new();
                            props.insert("title".into(), json!(window.window_title()));
                            props.insert(
                                "is_active".into(),
                                json!(active_window_id == Some(handle.window_id())),
                            );
                            props
                        },
                        source_location: None,
                        style_json: None,
                        content_size: if compact {
                            None
                        } else {
                            Some((converted.width, converted.height))
                        },
                        text_content: vec![],
                    }
                })
                .ok()
        })
        .collect();

    let total_elements = count_elements(&children);

    let tree = UiTree {
        root: UiElement {
            id: "app".to_string(),
            element_type: "Application".to_string(),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            visible: true,
            children,
            properties: Default::default(),
            source_location: None,
            style_json: None,
            content_size: None,
            text_content: vec![],
        },
        window_count: cx.windows().len(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    let mut result = serde_json::to_value(&tree).map_err(|e| e.to_string())?;
    // Add metadata at top level for easier consumption
    if let Some(obj) = result.as_object_mut() {
        obj.insert("total_elements".into(), json!(total_elements));
    }
    Ok(result)
}

/// Find an element in the tree by ID (full_id, global_id portion, or suffix match).
/// Returns a clone of the matched element with its full subtree.
fn find_subtree(elements: &[UiElement], query: &str) -> Option<UiElement> {
    for elem in elements {
        if id_matches(&elem.id, strip_id_decoration(&elem.id), query) {
            return Some(elem.clone());
        }
        if let Some(found) = find_subtree(&elem.children, query) {
            return Some(found);
        }
    }
    None
}

/// Strip verbose fields for compact output mode.
/// Removes bounds, content_mask, source_location, content_size, style_json.
/// Shortens element IDs by stripping crate paths.
fn strip_verbose_fields(elem: &mut UiElement) {
    elem.bounds = Bounds {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };
    elem.source_location = None;
    elem.style_json = None;
    elem.content_size = None;
    elem.properties.remove("content_mask");
    elem.properties.remove("instance_id");
    elem.id = shorten_element_id(&elem.id);

    for child in &mut elem.children {
        strip_verbose_fields(child);
    }
}

/// Shorten an element ID by stripping crate module paths from each segment.
/// `view-123.gpui_component::resizable::panel::ResizablePanel.resizable-panel-0`
/// becomes `view-123.ResizablePanel.resizable-panel-0`
fn shorten_element_id(id: &str) -> String {
    // Split off window prefix: "WindowId(1v1)/rest[0]"
    let (window_prefix, rest) = id
        .find('/')
        .map(|i| (&id[..i], &id[i + 1..]))
        .unwrap_or(("", id));

    // Split off instance suffix: "rest[0]"
    let (path, suffix) = rest
        .rfind('[')
        .map(|i| (&rest[..i], &rest[i..]))
        .unwrap_or((rest, ""));

    // Shorten each dot-separated segment
    let shortened: Vec<&str> = path
        .split('.')
        .map(|segment| {
            // If segment contains "::", take only the last part
            if let Some(last) = segment.rsplit("::").next() {
                last
            } else {
                segment
            }
        })
        .collect();

    if window_prefix.is_empty() {
        format!("{}{}", shortened.join("."), suffix)
    } else {
        format!("{}/{}{}", window_prefix, shortened.join("."), suffix)
    }
}

/// Count total elements in a tree
fn count_elements(elements: &[UiElement]) -> usize {
    elements
        .iter()
        .map(|e| 1 + count_elements(&e.children))
        .sum()
}

/// Truncate tree at max_depth
fn truncate_tree(elements: &mut Vec<UiElement>, current_depth: usize, max_depth: usize) {
    if current_depth >= max_depth {
        for elem in elements.iter_mut() {
            let child_count = count_elements(&elem.children);
            elem.children.clear();
            if child_count > 0 {
                elem.properties
                    .insert("truncated_children".into(), json!(child_count));
            }
        }
    } else {
        for elem in elements.iter_mut() {
            truncate_tree(&mut elem.children, current_depth + 1, max_depth);
        }
    }
}

/// Filter tree to only include elements matching the type filter (or their ancestors)
fn filter_tree(elements: &mut Vec<UiElement>, filter_lower: &str) {
    elements.retain_mut(|elem| {
        // Recursively filter children first
        filter_tree(&mut elem.children, filter_lower);

        // Keep this element if it matches or has matching descendants
        elem.element_type.to_lowercase().contains(filter_lower) || !elem.children.is_empty()
    });
}

/// Filter tree to only include elements with matching text content (or their ancestors)
fn filter_tree_by_text(elements: &mut Vec<UiElement>, filter_lower: &str) {
    elements.retain_mut(|elem| {
        // Recursively filter children first
        filter_tree_by_text(&mut elem.children, filter_lower);

        // Keep this element if its text matches or has matching descendants
        let has_matching_text = elem
            .text_content
            .iter()
            .any(|t| t.to_lowercase().contains(filter_lower));

        has_matching_text || !elem.children.is_empty()
    });
}

/// Build a hierarchical tree from GPUI's flat inspector element list, using
/// the dot-separated `global_id` as the hierarchy key.
///
/// An element's parent is the longest *proper* prefix of its `global_id` that
/// ends on a dot boundary and belongs to a real element. Trimming segments off
/// the right finds it in as many steps as the element is deep. The previous
/// version scanned every other element for every element to find the same
/// thing — millions of string comparisons on a UI of any size, paid again on
/// every inspect call.
fn build_element_tree(
    window_id: &str,
    elements: Vec<gpui::InspectorElementInfo>,
) -> Vec<UiElement> {
    use std::collections::HashMap;

    struct FlatEntry {
        global_id: String,
        element: UiElement,
    }

    let mut entries: Vec<FlatEntry> = elements
        .into_iter()
        .map(|info| {
            let full_id = format!("{}/{}[{}]", window_id, info.global_id, info.instance_id);

            let element_type = info
                .source_location
                .rsplit('/')
                .next()
                .and_then(|filename| filename.split('.').next())
                .unwrap_or("Element")
                .to_string();

            let bounds = convert_bounds(info.bounds);

            let mut properties = HashMap::new();
            properties.insert("instance_id".into(), json!(info.instance_id));
            let cm = info.content_mask.bounds;
            properties.insert(
                "content_mask".into(),
                json!({
                    "x": px_to_f32(cm.origin.x),
                    "y": px_to_f32(cm.origin.y),
                    "width": px_to_f32(cm.size.width),
                    "height": px_to_f32(cm.size.height),
                }),
            );

            FlatEntry {
                global_id: info.global_id.clone(),
                element: UiElement {
                    id: full_id,
                    element_type,
                    bounds: bounds.clone(),
                    visible: true,
                    children: vec![],
                    properties,
                    source_location: Some(info.source_location),
                    style_json: None,
                    content_size: Some((bounds.width, bounds.height)),
                    text_content: info.text_content,
                },
            }
        })
        .collect();

    // Sort by depth (fewer dots = higher in hierarchy)
    entries.sort_by(|a, b| {
        let depth_a = a.global_id.matches('.').count();
        let depth_b = b.global_id.matches('.').count();
        depth_a.cmp(&depth_b).then(a.global_id.cmp(&b.global_id))
    });

    // Where each element's parent sits in `entries`. Computed while nothing is
    // being moved, so the borrows of `entries` end before assembly starts.
    let parent_index: Vec<Option<usize>> = {
        let mut first_with_global: HashMap<&str, usize> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            // First one wins, which is how the previous version broke a tie
            // between two elements sharing a global_id.
            first_with_global
                .entry(entry.global_id.as_str())
                .or_insert(index);
        }

        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut prefix = entry.global_id.as_str();
                loop {
                    let dot = prefix.rfind('.')?;
                    prefix = &prefix[..dot];
                    if let Some(&parent) = first_with_global.get(prefix) {
                        // An element sharing its global_id with its own prefix
                        // would otherwise adopt itself.
                        return (parent != index).then_some(parent);
                    }
                }
            })
            .collect()
    };

    // Assemble deepest-first, so a child is complete before it moves into its
    // parent. Parents always sit at a lower index: their global_id has strictly
    // fewer dots, and the sort above put fewer dots first.
    let mut elements: Vec<Option<UiElement>> = entries
        .into_iter()
        .map(|entry| Some(entry.element))
        .collect();

    for index in (0..elements.len()).rev() {
        let Some(parent) = parent_index[index] else {
            continue;
        };
        let Some(child) = elements[index].take() else {
            continue;
        };
        match elements.get_mut(parent) {
            Some(Some(parent_element)) => parent_element.children.push(child),
            // Cannot happen given the ordering above; keeping the element at
            // top level beats dropping it from the tree.
            _ => elements[index] = Some(child),
        }
    }

    elements
        .into_iter()
        .flatten()
        .map(|mut element| {
            restore_child_order(&mut element);
            element
        })
        .collect()
}

/// Put each child list back into layout order.
///
/// The tree is assembled deepest-first, which appends each parent's children
/// in reverse; one pass undoes that so siblings read in the order the sort
/// established rather than backwards.
fn restore_child_order(element: &mut UiElement) {
    element.children.reverse();
    for child in &mut element.children {
        restore_child_order(child);
    }
}

// ===== Snapshot =====

/// What this crate's own source layout says an element is.
///
/// The file that rendered an element is already in its `source_location`, and
/// for gpui-component's widgets the file name *is* the role: `button/button.rs`
/// renders a button. So every app built on this crate gets a semantic
/// vocabulary for nothing, with not a line to annotate. An app's own widgets
/// get no role here — they are recognised by the ids their author chose and by
/// the text they paint.
const ROLES: &[(&str, &str)] = &[
    ("accordion", "group"),
    ("alert", "alert"),
    ("alert_dialog", "alertdialog"),
    ("app_menu_bar", "menubar"),
    ("avatar", "img"),
    ("badge", "status"),
    ("breadcrumb", "navigation"),
    ("button", "button"),
    ("button_group", "group"),
    ("button_icon", "button"),
    ("checkbox", "checkbox"),
    ("collapsible", "group"),
    ("color_picker", "colorpicker"),
    ("combobox", "combobox"),
    ("context_menu", "menu"),
    ("data_table", "table"),
    ("dialog", "dialog"),
    ("dropdown_button", "button"),
    ("dropdown_menu", "menu"),
    ("group", "group"),
    ("group_box", "group"),
    ("hover_card", "tooltip"),
    ("icon", "img"),
    ("input", "textbox"),
    ("kbd", "text"),
    ("label", "text"),
    ("link", "link"),
    ("list", "list"),
    ("list_item", "listitem"),
    ("menu", "menu"),
    ("menu_item", "menuitem"),
    ("notification", "alert"),
    ("number_input", "spinbutton"),
    ("otp_input", "textbox"),
    ("pagination", "navigation"),
    ("popover", "dialog"),
    ("popup_menu", "menu"),
    ("progress", "progressbar"),
    ("progress_circle", "progressbar"),
    ("radio", "radio"),
    ("rating", "rating"),
    ("searchable_list", "list"),
    ("select", "combobox"),
    ("separator", "separator"),
    ("sheet", "dialog"),
    ("slider", "slider"),
    ("spinner", "progressbar"),
    ("status_bar", "status"),
    ("stepper", "group"),
    ("switch", "switch"),
    ("tab", "tab"),
    ("tab_bar", "tablist"),
    ("table", "table"),
    ("tag", "text"),
    ("text_view", "text"),
    ("title_bar", "banner"),
    ("toggle", "button"),
    ("tooltip", "tooltip"),
    ("tree", "tree"),
    ("virtual_list", "list"),
];

/// Roles that describe a region rather than a control.
///
/// A file renders more than one kind of thing: `title_bar.rs` paints the title
/// bar *and* its close button, so the file name alone would label that button
/// "banner". A region role is therefore only used for an element that has
/// something inside it; a leaf falls back to its id and its text, which is
/// vague but not wrong.
const REGION_ROLES: &[&str] = &[
    "alert",
    "alertdialog",
    "banner",
    "dialog",
    "form",
    "group",
    "list",
    "menu",
    "menubar",
    "navigation",
    "status",
    "table",
    "tablist",
    "tree",
];

/// Roles an agent can act on, for `interactive_only`.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "colorpicker",
    "combobox",
    "link",
    "listitem",
    "menuitem",
    "radio",
    "rating",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "tree",
];

fn is_path_separator(c: char) -> bool {
    c == '/' || c == std::path::MAIN_SEPARATOR
}

fn role_for(source_location: &str) -> Option<&'static str> {
    let file = source_location
        .rsplit(is_path_separator)
        .next()
        .unwrap_or(source_location);
    let stem = file.split('.').next().unwrap_or(file);

    ROLES
        .iter()
        .find(|(name, _)| *name == stem)
        .map(|(_, role)| *role)
}

fn is_interactive(role: Option<&str>) -> bool {
    role.is_some_and(|role| INTERACTIVE_ROLES.contains(&role))
}

/// The last id segment an app actually chose, if it chose one.
///
/// gpui ids mix names somebody wrote with names it generated:
/// `view-4294967734`, numeric paths like `1-0-0`, and type names in CamelCase.
/// A lowercase, dashed or underscored segment is the written kind — the only
/// kind worth printing and worth telling an agent to target.
fn test_id_of(global_id: &str) -> Option<&str> {
    let segment = global_id.rsplit('.').next()?;

    if segment.starts_with("view-") {
        return None;
    }
    let written = segment
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');

    written.then_some(segment)
}

/// The text painted inside an element, short enough to read in a list.
fn name_of(element: &UiElement) -> Option<String> {
    let joined = element
        .text_content
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    (!joined.is_empty()).then(|| truncate_chars(&joined, 60))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut short: String = text.chars().take(limit).collect();
    short.push('…');
    short
}

/// What the last snapshot handed out, so `@e7` can be turned back into an
/// element.
///
/// One table, replaced whole by each snapshot: refs are a shorthand for "the
/// thing on line 7 of what I just showed you", and keeping older ones alive
/// would let an agent act on a line it can no longer see.
static SNAPSHOT_REFS: std::sync::LazyLock<Mutex<SnapshotRefs>> =
    std::sync::LazyLock::new(|| Mutex::new(SnapshotRefs::default()));

#[derive(Default)]
struct SnapshotRefs {
    id: u64,
    by_ref: std::collections::HashMap<String, String>,
}

/// Turn what a step named into something [`id_matches`] can compare against.
///
/// Two of the forms an agent has in front of it come from the snapshot rather
/// than from gpui: `@e7`, the thing on line seven of what was last printed,
/// and `#item`, which is how that snapshot writes an id. Neither is an id as
/// gpui stores it, and "element not found" for a string this very server just
/// printed is the most confusing answer available — so both are resolved here
/// instead of only the raw id underneath them. Anything else passes through
/// untouched.
fn expand_ref(query: &str) -> Result<String, String> {
    let Some(reference) = query.strip_prefix('@') else {
        return Ok(query.strip_prefix('#').unwrap_or(query).to_string());
    };

    let refs = SNAPSHOT_REFS
        .lock()
        .map_err(|_| "The snapshot ref table is poisoned.".to_string())?;

    refs.by_ref.get(reference).cloned().ok_or_else(|| {
        format!(
            "Unknown ref '{}'. Refs are handed out by ui_snapshot and the next snapshot \
             replaces them; take a fresh snapshot and use a ref it printed. \
             (Current snapshot: {}.)",
            query, refs.id
        )
    })
}

/// One line of a snapshot.
struct SnapshotNode {
    role: Option<&'static str>,
    name: Option<String>,
    test_id: Option<String>,
    full_id: String,
    /// The source location gpui recorded for this element. For a
    /// gpui-component widget that is the widget's own file, so it says *what*
    /// the element is rather than where the app put it — the id and the
    /// element path are what locate it. The snapshot leaves it out; the audit
    /// prints it.
    source: Option<String>,
    bounds: Bounds,
    children: Vec<SnapshotNode>,
}

/// Keep the elements that mean something and flatten away the rest.
///
/// An element earns a line by having a role, an id somebody chose, or text.
/// Everything else is layout scaffolding: it is dropped and its children take
/// its place, which is where most of the size difference against the full tree
/// comes from.
fn snapshot_nodes(elements: &[UiElement], interactive_only: bool) -> Vec<SnapshotNode> {
    let mut nodes = Vec::new();

    for element in elements {
        let children = snapshot_nodes(&element.children, interactive_only);

        let role = element
            .source_location
            .as_deref()
            .and_then(role_for)
            .filter(|role| !REGION_ROLES.contains(role) || !children.is_empty());
        let test_id = test_id_of(strip_id_decoration(&element.id)).map(str::to_string);
        let name = name_of(element);

        let says_something = role.is_some() || test_id.is_some() || name.is_some();
        let wanted = says_something && (!interactive_only || is_interactive(role));

        if wanted {
            nodes.push(SnapshotNode {
                role,
                name,
                test_id,
                full_id: element.id.clone(),
                source: element.source_location.clone(),
                bounds: element.bounds.clone(),
                children,
            });
        } else {
            nodes.extend(children);
        }
    }

    nodes
}

/// Keep matching nodes and the ancestors that lead to them.
fn filter_snapshot(nodes: &mut Vec<SnapshotNode>, needle: &str) {
    nodes.retain_mut(|node| {
        filter_snapshot(&mut node.children, needle);

        let hit = node.role.is_some_and(|role| role.contains(needle))
            || node
                .name
                .as_ref()
                .is_some_and(|name| name.to_lowercase().contains(needle))
            || node
                .test_id
                .as_ref()
                .is_some_and(|test_id| test_id.contains(needle));

        hit || !node.children.is_empty()
    });
}

/// A rendered snapshot, and the refs it handed out.
#[derive(Default)]
struct RenderedSnapshot {
    text: String,
    refs: std::collections::HashMap<String, String>,
    shown: usize,
    truncated: bool,
}

fn render_snapshot(
    nodes: &[SnapshotNode],
    depth: usize,
    include_bounds: bool,
    limit: usize,
    out: &mut RenderedSnapshot,
) {
    for node in nodes {
        if out.shown >= limit {
            out.truncated = true;
            return;
        }

        out.shown += 1;
        let reference = format!("e{}", out.shown);
        out.refs.insert(reference.clone(), node.full_id.clone());

        out.text.push_str(&"  ".repeat(depth));
        out.text.push_str("- ");
        out.text.push_str(node.role.unwrap_or("node"));
        if let Some(name) = &node.name {
            out.text.push_str(&format!(" \"{}\"", name));
        }
        if let Some(test_id) = &node.test_id {
            out.text.push_str(&format!(" #{}", test_id));
        }
        out.text.push_str(&format!(" @{}", reference));
        if include_bounds {
            out.text.push_str(&format!(
                " [{:.0},{:.0} {:.0}x{:.0}]",
                node.bounds.x, node.bounds.y, node.bounds.width, node.bounds.height
            ));
        }
        out.text.push('\n');

        render_snapshot(&node.children, depth + 1, include_bounds, limit, out);
        if out.truncated {
            return;
        }
    }
}

/// Answer [`methods::UI_SNAPSHOT`]: the window as a short, readable list.
///
/// This is what an agent should look at first. The full tree costs tens of
/// thousands of tokens on a real UI and answers questions about layout; this
/// costs a fraction of that and answers the question actually being asked,
/// which is "what is on screen and what can I do to it".
fn handle_ui_snapshot(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: UiSnapshotParams = serde_json::from_value(params.clone()).unwrap_or_default();

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;
    let window_id = format!("{:?}", handle.window_id());

    let mut elements = handle
        .update(cx, |_, window, _| {
            build_element_tree(&window_id, window.inspector_elements())
        })
        .map_err(|e| e.to_string())?;
    let painted = count_elements(&elements);

    if let Some(root) = &opts.root_element_id {
        let root = expand_ref(root)?;
        match find_subtree(&elements, &root) {
            Some(subtree) => elements = vec![subtree],
            None => {
                let candidates = collect_match_candidates(&root, opts.window_id.as_deref(), cx, 5);
                return Err(not_found_error(&root, candidates));
            }
        }
    }

    let mut nodes = snapshot_nodes(&elements, opts.interactive_only);
    if let Some(filter) = &opts.filter {
        filter_snapshot(&mut nodes, &filter.to_lowercase());
    }

    let limit = opts
        .max_elements
        .unwrap_or(DEFAULT_SNAPSHOT_ELEMENTS)
        .max(1);
    let mut rendered = RenderedSnapshot::default();
    render_snapshot(&nodes, 0, opts.include_bounds, limit, &mut rendered);

    let snapshot_id = {
        let mut refs = SNAPSHOT_REFS
            .lock()
            .map_err(|_| "The snapshot ref table is poisoned.".to_string())?;
        refs.id += 1;
        refs.by_ref = rendered.refs;
        refs.id
    };

    mcp_log(format!(
        "Snapshot {} of {}: {} lines from {} painted elements",
        snapshot_id, window_id, rendered.shown, painted
    ));

    Ok(json!({
        "snapshot_id": snapshot_id,
        "window_id": window_id,
        "elements": rendered.shown,
        "painted_elements": painted,
        "truncated": rendered.truncated,
        "snapshot": rendered.text,
    }))
}

// ===== Accessibility audit =====

/// One thing wrong with the UI, and where to fix it.
struct Finding {
    severity: &'static str,
    check: &'static str,
    message: String,
    role: Option<&'static str>,
    test_id: Option<String>,
    element: String,
    source: Option<String>,
    bounds: Option<Bounds>,
}

impl Finding {
    fn to_json(&self) -> serde_json::Value {
        let mut value = json!({
            "severity": self.severity,
            "check": self.check,
            "message": self.message,
            "element": self.element,
        });
        let object = value.as_object_mut().expect("object");
        if let Some(role) = self.role {
            object.insert("role".into(), json!(role));
        }
        if let Some(test_id) = &self.test_id {
            object.insert("test_id".into(), json!(test_id));
        }
        if let Some(source) = &self.source {
            object.insert("source".into(), json!(source));
        }
        if let Some(bounds) = &self.bounds {
            object.insert("bounds".into(), json!(bounds));
        }
        value
    }
}

/// Severity ranks, so `fail_on` can be compared and findings sorted.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "serious" => 2,
        "warning" => 1,
        _ => 0,
    }
}

/// Whether an audit passes at a given threshold.
///
/// `none` always passes — it is how a script says "report, do not gate", and a
/// threshold comparison that treats it as "warning" would take that away.
fn audit_passes(findings: &[Finding], fail_on: &str) -> bool {
    let threshold = severity_rank(fail_on);
    threshold == 0
        || !findings
            .iter()
            .any(|finding| severity_rank(finding.severity) >= threshold)
}

/// Every node in reading order, parents before children.
fn flatten_snapshot<'a>(nodes: &'a [SnapshotNode], out: &mut Vec<&'a SnapshotNode>) {
    for node in nodes {
        out.push(node);
        flatten_snapshot(&node.children, out);
    }
}

/// Answer [`methods::A11Y_AUDIT`].
///
/// This reads the same derived layer the snapshot prints, so it sees what that
/// layer sees and no more: no colours, so no contrast; no state, so nothing
/// about what a control announces when it changes. What it does catch is the
/// class of problem that hurts a screen reader and an agent alike: a control
/// nothing can name, an id that names several things, a target too small to
/// hit.
fn handle_a11y_audit(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: A11yAuditParams = serde_json::from_value(params.clone()).unwrap_or_default();

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;
    let window_id = format!("{:?}", handle.window_id());

    let mut elements = handle
        .update(cx, |_, window, _| {
            build_element_tree(&window_id, window.inspector_elements())
        })
        .map_err(|e| e.to_string())?;

    if let Some(root) = &opts.root_element_id {
        let root = expand_ref(root)?;
        match find_subtree(&elements, &root) {
            Some(subtree) => elements = vec![subtree],
            None => {
                let candidates = collect_match_candidates(&root, opts.window_id.as_deref(), cx, 5);
                return Err(not_found_error(&root, candidates));
            }
        }
    }

    let nodes = snapshot_nodes(&elements, false);
    let mut flat = Vec::new();
    flatten_snapshot(&nodes, &mut flat);

    let min_target = opts
        .min_target_size
        .unwrap_or(DEFAULT_MIN_TARGET_SIZE)
        .max(0.0);
    let mut findings = audit_controls(&flat, min_target);
    findings.extend(audit_ids(&flat));
    findings.extend(audit_unstable_ids(&flat));

    // Worst first: a list somebody reads from the top should start with what
    // matters.
    findings.sort_by_key(|finding| std::cmp::Reverse(severity_rank(finding.severity)));

    let serious = findings.iter().filter(|f| f.severity == "serious").count();
    let warnings = findings.iter().filter(|f| f.severity == "warning").count();

    let fail_on = opts.fail_on.as_deref().unwrap_or("serious");
    if !matches!(fail_on, "serious" | "warning" | "none") {
        // A typo here would silently produce an audit that can never fail,
        // which is the worst possible way for a gate to be broken.
        return Err(format!(
            "fail_on must be \"serious\", \"warning\" or \"none\", not \"{fail_on}\""
        ));
    }
    let ok = audit_passes(&findings, fail_on);

    let limit = opts.max_findings.unwrap_or(DEFAULT_MAX_FINDINGS).max(1);
    let truncated = findings.len() > limit;
    let listed: Vec<serde_json::Value> =
        findings.iter().take(limit).map(Finding::to_json).collect();

    mcp_log(format!(
        "a11y audit of {}: {} serious, {} warnings over {} elements",
        window_id,
        serious,
        warnings,
        flat.len()
    ));

    Ok(json!({
        "ok": ok,
        "window_id": window_id,
        "checked": flat.len(),
        "fail_on": fail_on,
        "serious": serious,
        "warnings": warnings,
        "truncated": truncated,
        "findings": listed,
    }))
}

/// Answer [`methods::A11Y_TREE`].
///
/// Two things have to happen before the tree can be read, which is why this is
/// answered asynchronously rather than from the frame already on screen.
/// GPUI builds the accessibility tree only while assistive technology is
/// attached — right for shipping, useless for checking — so the window is
/// switched into building it, and the flag takes effect from the *next* frame:
/// the one being painted latched its answer before the first node was pushed.
async fn run_a11y_tree(
    params: &serde_json::Value,
    cx: &AsyncApp,
) -> Result<serde_json::Value, String> {
    let opts: A11yTreeParams = serde_json::from_value(params.clone()).unwrap_or_default();
    let window_id = opts.window_id.clone();

    let was_building = cx.update(|cx| {
        let handle = resolve_window(window_id.as_deref(), cx)?;
        handle
            .update(cx, |_, window, _| {
                let was = window.is_a11y_active();
                window.set_a11y_force_active(true);
                was
            })
            .map_err(|e| e.to_string())
    })?;

    // Already building means the frame on screen already carries a tree, and
    // waiting would cost a frame to learn nothing.
    if !was_building {
        settle(window_id.as_deref(), cx).await;
    }

    cx.update(|cx| read_a11y_tree(window_id.as_deref(), cx))
}

/// Read the tree the last frame built, and say how much of the window it covers.
///
/// The count is the point of the answer as much as the tree is. Only elements
/// somebody annotated get a node, so a tree of eleven nodes over a window of
/// ninety-six painted elements is not a small window — it is a mostly
/// unannotated one, and an agent that is not told this will read the tree as
/// the whole UI.
fn read_a11y_tree(window_id: Option<&str>, cx: &mut App) -> Result<serde_json::Value, String> {
    let handle = resolve_window(window_id, cx)?;
    let id = format!("{:?}", handle.window_id());

    let (active, json, painted) = handle
        .update(cx, |_, window, _| {
            (
                window.is_a11y_active(),
                window.debug_a11y_tree_json(),
                window.inspector_elements().len(),
            )
        })
        .map_err(|e| e.to_string())?;

    let Some(json) = json.filter(|_| active) else {
        return Err(format!(
            "No accessibility tree for {id}. The window was asked to build one and no frame \
             carrying it arrived — an occluded or stalled window does that. Try again, or take a \
             ui_snapshot, which reads the frame already painted."
        ));
    };

    let tree: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| format!("The accessibility tree was not JSON: {e}"))?;

    let nodes = tree
        .get("nodes")
        .and_then(|nodes| nodes.as_object())
        .map(serde_json::Map::len)
        .unwrap_or(0);

    mcp_log(format!(
        "a11y tree of {id}: {nodes} nodes over {painted} painted elements"
    ));

    Ok(json!({
        "window_id": id,
        "nodes": nodes,
        "painted": painted,
        "tree": tree,
    }))
}
/// Controls nobody can name, hit, or see.
fn audit_controls(nodes: &[&SnapshotNode], min_target: f32) -> Vec<Finding> {
    let mut findings = Vec::new();

    for node in nodes {
        if !is_interactive(node.role) {
            continue;
        }
        let role = node.role.unwrap_or("control");

        if node.name.is_none() {
            findings.push(Finding {
                severity: "serious",
                check: "unnamed-control",
                message: format!(
                    "This {role} paints no text, so nothing can name it: a screen reader has \
                     nothing to announce and an agent has nothing to match on. Give it a label, \
                     or a tooltip, or at least an id it can be targeted by."
                ),
                role: node.role,
                test_id: node.test_id.clone(),
                element: shorten_element_id(&node.full_id),
                source: node.source.clone(),
                bounds: None,
            });
        }

        if node.bounds.width <= 0.0 || node.bounds.height <= 0.0 {
            findings.push(Finding {
                severity: "serious",
                check: "zero-size-control",
                message: format!("This {role} is painted with no area, so nobody can click it."),
                role: node.role,
                test_id: node.test_id.clone(),
                element: shorten_element_id(&node.full_id),
                source: node.source.clone(),
                bounds: Some(node.bounds.clone()),
            });
        } else if node.bounds.width < min_target || node.bounds.height < min_target {
            findings.push(Finding {
                severity: "warning",
                check: "target-too-small",
                message: format!(
                    "This {role} is {:.0}x{:.0} px, under the {:.0} px minimum. Small targets \
                     are hard to hit with a shaky hand or a finger.",
                    node.bounds.width, node.bounds.height, min_target
                ),
                role: node.role,
                test_id: node.test_id.clone(),
                element: shorten_element_id(&node.full_id),
                source: node.source.clone(),
                bounds: Some(node.bounds.clone()),
            });
        }
    }

    findings
}

/// Ids that name more than one thing.
///
/// This is an accessibility finding and a testing one at once. A suffix match
/// takes the first element it fits, so an id shared by forty list rows means a
/// recorded script targeting it clicks the wrong row — quietly, and only in
/// the run where the order changed.
fn audit_ids(nodes: &[&SnapshotNode]) -> Vec<Finding> {
    let mut by_id: std::collections::HashMap<&str, Vec<&&SnapshotNode>> =
        std::collections::HashMap::new();

    for node in nodes {
        if let Some(test_id) = &node.test_id {
            by_id.entry(test_id.as_str()).or_default().push(node);
        }
    }

    let mut duplicates: Vec<(&str, Vec<&&SnapshotNode>)> = by_id
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .collect();
    // A map has no order of its own, and a finding list that reshuffles
    // between runs is a bad diff.
    duplicates.sort_by_key(|(test_id, _)| *test_id);

    duplicates
        .into_iter()
        .map(|(test_id, group)| {
            let interactive = group.iter().any(|node| is_interactive(node.role));
            let first = group[0];

            Finding {
                severity: if interactive { "serious" } else { "warning" },
                check: "duplicate-id",
                message: format!(
                    "#{test_id} names {} elements in this window. A suffix match takes the \
                     first, so anything targeting it — a click, a wait, a recorded script — \
                     may act on the wrong one. Give them ids of their own.",
                    group.len()
                ),
                role: first.role,
                test_id: Some(test_id.to_string()),
                element: shorten_element_id(&first.full_id),
                source: first.source.clone(),
                bounds: None,
            }
        })
        .collect()
}

/// Ids that will name nothing tomorrow.
///
/// `#input-4294967299` passes every test for an id somebody chose — lowercase,
/// dashed, as deliberate-looking as `#save-button` — and the number in it is an
/// entity id gpui hands out fresh on every app start. So the snapshot prints
/// it, an agent targets it, a recorded script stores it, and the next run
/// finds nothing. That is the one class of bad id the derived layer cannot
/// tell apart from a good one by looking at it, which is why it gets a check
/// of its own.
///
/// Reported once per id, not once per element: sixty rows sharing a generated
/// id are one problem with one fix.
fn audit_unstable_ids(nodes: &[&SnapshotNode]) -> Vec<Finding> {
    let mut by_id: std::collections::HashMap<&str, Vec<&&SnapshotNode>> =
        std::collections::HashMap::new();

    for node in nodes {
        if let Some(test_id) = &node.test_id {
            if id_looks_generated(test_id) {
                by_id.entry(test_id.as_str()).or_default().push(node);
            }
        }
    }

    let mut unstable: Vec<(&str, Vec<&&SnapshotNode>)> = by_id.into_iter().collect();
    // Same reason as the duplicates: a map has no order, and a finding list
    // that reshuffles between runs is a bad diff.
    unstable.sort_by_key(|(test_id, _)| *test_id);

    unstable
        .into_iter()
        .map(|(test_id, group)| {
            let first = group[0];
            let subject = if group.len() > 1 {
                format!("these {} elements", group.len())
            } else {
                "it".to_string()
            };

            Finding {
                // A warning rather than serious: nothing here is broken for
                // anyone using the app right now. What breaks is everything
                // written down against it, on the next start, quietly.
                severity: "warning",
                check: "unstable-id",
                message: format!(
                    "#{test_id} ends in a number the app generates fresh on every start, so it \
                     reads like a name and is not one. Anything written down against it — a \
                     recorded script, a bug report, a note to yourself — matches nothing after a \
                     restart. Give {subject} an id of its own."
                ),
                role: first.role,
                test_id: Some(test_id.to_string()),
                element: shorten_element_id(&first.full_id),
                source: first.source.clone(),
                bounds: None,
            }
        })
        .collect()
}

fn handle_get_element(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let params: GetElementParams =
        serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
    let query = expand_ref(&params.element_id)?;
    let query = query.as_str();

    // Build the full tree for each window and search by ID.
    // This ensures the returned element includes its full subtree and text content.
    for handle in cx.windows() {
        let result = handle.update(cx, |_, window, _cx| {
            let window_id_str = format!("{:?}", handle.window_id());
            let inspector_elems = window.inspector_elements();
            let children = build_element_tree(&window_id_str, inspector_elems);

            // Check if query matches the window itself
            if window_id_str == query {
                let converted = convert_bounds(window.bounds());
                return Some(UiElement {
                    id: window_id_str,
                    element_type: "Window".to_string(),
                    bounds: converted.clone(),
                    visible: true,
                    children,
                    properties: {
                        let mut props = std::collections::HashMap::new();
                        props.insert("title".into(), json!(window.window_title()));
                        props
                    },
                    source_location: None,
                    style_json: None,
                    content_size: Some((converted.width, converted.height)),
                    text_content: vec![],
                });
            }

            // Search the tree for the element
            find_subtree(&children, query)
        });

        if let Ok(Some(element)) = result {
            return serde_json::to_value(&element).map_err(|e| e.to_string());
        }
    }

    let candidates = collect_match_candidates(&params.element_id, None, cx, 5);
    Err(not_found_error(&params.element_id, candidates))
}

fn handle_take_screenshot(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: TakeScreenshotParams = serde_json::from_value(params.clone()).unwrap_or_default();

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;
    let crop_to = opts.element_id.as_deref().map(expand_ref).transpose()?;

    let (image, scale_factor) = handle
        .update(cx, |_, window, _cx| {
            let scale = window.scale_factor();
            let img = window.render_to_image()?;
            Ok::<_, anyhow::Error>((img, scale))
        })
        .map_err(|e| format!("Failed to access window: {}", e))?
        .map_err(|e| format!("Failed to render screenshot: {}", e))?;

    // If element_id is set, resolve bounds and crop
    let (final_image, element_info) = if let Some(ref element_id) = crop_to {
        // Resolve element bounds (in logical pixels)
        let bounds_result = handle
            .update(cx, |_, window, _cx| {
                let window_id_str = format!("{:?}", handle.window_id());
                for info in window.inspector_elements() {
                    let full_id =
                        format!("{}/{}[{}]", window_id_str, info.global_id, info.instance_id);
                    if id_matches(&full_id, &info.global_id, element_id) {
                        return Some((info.bounds, full_id));
                    }
                }
                None
            })
            .map_err(|e| e.to_string())?;

        let (elem_bounds, resolved_id) = match bounds_result {
            Some(v) => v,
            None => {
                let candidates =
                    collect_match_candidates(element_id, opts.window_id.as_deref(), cx, 5);
                return Err(not_found_error(element_id, candidates));
            }
        };

        // Convert logical bounds to device pixels for cropping
        let x = (f32::from(elem_bounds.origin.x) * scale_factor).round() as u32;
        let y = (f32::from(elem_bounds.origin.y) * scale_factor).round() as u32;
        let w = (f32::from(elem_bounds.size.width) * scale_factor).round() as u32;
        let h = (f32::from(elem_bounds.size.height) * scale_factor).round() as u32;

        let (img_w, img_h) = image.dimensions();
        let x = x.min(img_w.saturating_sub(1));
        let y = y.min(img_h.saturating_sub(1));
        let w = w.min(img_w.saturating_sub(x));
        let h = h.min(img_h.saturating_sub(y));

        use image::GenericImageView;
        let cropped = image.view(x, y, w, h).to_image();
        (cropped, Some(resolved_id))
    } else {
        (image, None)
    };

    // Downscale before writing. An image costs an agent tokens by its pixel
    // dimensions, not by its file size, so a 4K window screenshot spends a
    // large part of a context window on detail nobody asked for. Cropped
    // element shots are usually below the limit already and pass through
    // untouched.
    let max_width = opts.max_width.unwrap_or(DEFAULT_SCREENSHOT_MAX_WIDTH);
    let (final_image, scale) = match final_image.width() {
        raw_width if max_width > 0 && raw_width > max_width => {
            let ratio = max_width as f32 / raw_width as f32;
            let scaled_height = ((final_image.height() as f32) * ratio).round().max(1.0) as u32;
            (
                image::imageops::resize(
                    &final_image,
                    max_width,
                    scaled_height,
                    image::imageops::FilterType::Triangle,
                ),
                Some(ratio),
            )
        }
        _ => (final_image, None),
    };

    let (width, height) = final_image.dimensions();

    // Save as PNG to a temp file. The name carries a sequence number as well
    // as the pid: a batch may take several screenshots, and naming them all
    // alike meant the last one overwrote the file the server had not read yet
    // — the first image came back showing the last frame, and the last came
    // back with no image at all.
    let sequence = SCREENSHOT_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp_path = std::env::temp_dir().join(format!(
        "gpui-screenshot-{}-{}.png",
        std::process::id(),
        sequence
    ));
    final_image
        .save(&temp_path)
        .map_err(|e| format!("Failed to save screenshot: {}", e))?;

    mcp_log(format!(
        "Screenshot captured: {}x{}{} -> {}",
        width,
        height,
        element_info
            .as_deref()
            .map(|id| format!(" (element: {})", id))
            .unwrap_or_default(),
        temp_path.display()
    ));

    let mut result = json!({
        "width": width,
        "height": height,
        "format": "png",
        "path": temp_path.to_string_lossy(),
    });
    if let Some(object) = result.as_object_mut() {
        if let Some(id) = element_info {
            object.insert("element_id".into(), json!(id));
        }
        // Say so when the image was shrunk: a coordinate read off it is not a
        // window coordinate any more, and a click sent there would land
        // somewhere else entirely.
        if let Some(scale) = scale {
            object.insert("scale".into(), json!(scale));
        }
    }
    Ok(result)
}

fn handle_execute_action(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: ExecuteActionParams =
        serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    // Build the action from its registered name
    let action_data = if opts.args.is_null() || opts.args == json!({}) {
        None
    } else {
        Some(opts.args.clone())
    };

    let action = cx
        .build_action(&opts.action, action_data)
        .map_err(|e| format!("Failed to build action '{}': {:?}", opts.action, e))?;

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;

    // Use FocusHandle::dispatch_action (synchronous) when the window has
    // a focused element — Window::dispatch_action uses cx.defer() which
    // would run the action *after* attach_post_state reads state, leaving
    // the MCP response stale. The direct path goes through
    // dispatch_action_on_node immediately so the action's side effects are
    // visible in the same handler tick.
    let (window_id, window_title, has_focus) = handle
        .update(cx, |_, window, cx| {
            let wid = format!("{:?}", handle.window_id());
            let title = window.window_title();
            let focused = window.focused(cx);
            let has_focus = focused.is_some();
            match focused {
                Some(focus_handle) => focus_handle.dispatch_action(action.as_ref(), window, cx),
                None => window.dispatch_action(action, cx),
            }
            (wid, title, has_focus)
        })
        .map_err(|e| format!("Failed to dispatch action: {}", e))?;

    mcp_log(format!(
        "Executed action: {} on window {} (focused={})",
        opts.action, window_id, has_focus
    ));
    let response = json!({
        "success": true,
        "action": opts.action,
        "window_id": window_id,
        "window_title": window_title,
        "window_had_focus": has_focus,
    });
    Ok(response)
}

fn handle_list_actions(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: ListActionsParams =
        serde_json::from_value(params.clone()).unwrap_or(ListActionsParams {
            filter: None,
            include_bindings: false,
            only_available: false,
            window_id: None,
        });

    // Take an owned copy of action names so we can release the immutable
    // borrow on cx before calling resolve_window() / handle.update() for
    // the only_available path.
    let all_names: Vec<&'static str> = cx.all_action_names().to_vec();

    let filtered_names: Vec<&'static str> = if let Some(ref filter) = opts.filter {
        let filter_lower = filter.to_lowercase();
        all_names
            .iter()
            .filter(|name| name.to_lowercase().contains(&filter_lower))
            .copied()
            .collect()
    } else {
        all_names.clone()
    };

    // Resolve available actions if requested: walk the focus chain's key
    // contexts and collect action names whose binding predicate matches.
    // `only_available` implies we must return bindings, since filtering
    // depends on them.
    let available_action_names: Option<std::collections::HashSet<String>> = if opts.only_available {
        let handle = resolve_window(opts.window_id.as_deref(), cx)?;
        let contexts: Vec<gpui::KeyContext> = handle
            .update(cx, |_, window, _cx| window.context_stack())
            .map_err(|e| e.to_string())?;

        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let mut names = std::collections::HashSet::new();
        for binding in keymap.bindings() {
            let matches = match binding.predicate() {
                None => true, // global binding — always active
                Some(pred) => pred.eval(&contexts),
            };
            if matches {
                names.insert(binding.action().name().to_string());
            }
        }
        Some(names)
    } else {
        None
    };

    let filtered_names: Vec<&str> = if let Some(ref available) = available_action_names {
        filtered_names
            .into_iter()
            .filter(|name| available.contains(*name))
            .collect()
    } else {
        filtered_names
    };

    // only_available implies include_bindings (the result is only useful
    // with the binding info attached — otherwise the LLM can't see WHY
    // each action is available).
    let include_bindings = opts.include_bindings || opts.only_available;

    if !include_bindings {
        return Ok(json!({
            "actions": filtered_names,
            "count": filtered_names.len(),
            "total_registered": all_names.len(),
        }));
    }

    // Build rich action info with keybindings and documentation
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let docs = cx.action_documentation();

    let actions: Vec<serde_json::Value> = filtered_names
        .iter()
        .map(|name| {
            // Find all keybindings for this action
            let bindings: Vec<serde_json::Value> = keymap
                .bindings()
                .filter(|binding| binding.action().name() == *name)
                .map(|binding| {
                    let keystrokes: Vec<String> = binding
                        .keystrokes()
                        .iter()
                        .map(|ks| format!("{}", ks))
                        .collect();

                    let context = binding.predicate().map(|p| format!("{}", p));

                    let mut entry = json!({
                        "keys": keystrokes.join(" "),
                    });
                    if let Some(ctx) = context {
                        entry
                            .as_object_mut()
                            .map(|o| o.insert("context".into(), json!(ctx)));
                    }
                    entry
                })
                .collect();

            let mut entry = json!({
                "action": name,
                "bindings": bindings,
            });

            if let Some(doc) = docs.get(name) {
                entry
                    .as_object_mut()
                    .map(|o| o.insert("description".into(), json!(doc)));
            }

            entry
        })
        .collect();

    Ok(json!({
        "actions": actions,
        "count": actions.len(),
        "total_registered": all_names.len(),
    }))
}

fn handle_get_focus_info(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: GetFocusInfoParams = serde_json::from_value(params.clone()).unwrap_or_default();

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;

    let info = handle
        .update(cx, |_, window, cx| {
            let focused = window.focused(cx);
            let window_id = format!("{:?}", handle.window_id());

            // Get active key context stack
            let key_contexts: Vec<String> = window
                .context_stack()
                .iter()
                .map(|ctx| format!("{:?}", ctx))
                .collect();

            match focused {
                Some(focus_handle) => {
                    json!({
                        "has_focus": true,
                        "focus_handle": format!("{:?}", focus_handle),
                        "window_id": window_id,
                        "window_title": window.window_title(),
                        "key_contexts": key_contexts,
                    })
                }
                None => {
                    json!({
                        "has_focus": false,
                        "window_id": window_id,
                        "window_title": window.window_title(),
                        "key_contexts": key_contexts,
                    })
                }
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_passes_valid_chars_through() {
        assert_eq!(sanitize_app_name("elane"), "elane");
        assert_eq!(sanitize_app_name("my-editor"), "my-editor");
        assert_eq!(sanitize_app_name("app_v2"), "app_v2");
        assert_eq!(sanitize_app_name("Story-123"), "Story-123");
    }

    #[test]
    fn sanitize_replaces_invalid_chars() {
        assert_eq!(sanitize_app_name("my app"), "my_app");
        assert_eq!(sanitize_app_name("app/name"), "app_name");
        assert_eq!(sanitize_app_name("app.with.dots"), "app_with_dots");
    }

    #[test]
    fn sanitize_empty_falls_back() {
        assert_eq!(sanitize_app_name(""), "gpui-app");
    }

    #[test]
    fn socket_path_contains_app_and_pid() {
        let path = socket_path_for("elane");
        let pid = std::process::id();
        assert!(path.ends_with(&format!("gpui-mcp-elane-{}.sock", pid)));
    }

    /// A painted element with nothing in it, for the tree tests below.
    fn element(global_id: &str) -> gpui::InspectorElementInfo {
        let bounds = gpui::Bounds {
            origin: point(px(0.0), px(0.0)),
            size: gpui::size(px(10.0), px(10.0)),
        };
        gpui::InspectorElementInfo {
            bounds,
            content_mask: gpui::ContentMask { bounds },
            global_id: global_id.to_string(),
            source_location: "crates/ui/src/button.rs:1:1".to_string(),
            instance_id: 0,
            text_content: vec![],
        }
    }

    fn child_ids(element: &UiElement) -> Vec<&str> {
        element.children.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn tree_nests_by_dotted_global_id() {
        let tree = build_element_tree(
            "W",
            vec![
                element("root"),
                element("root.a"),
                element("root.a.x"),
                element("root.b"),
            ],
        );

        assert_eq!(tree.len(), 1, "only `root` has no parent");
        assert_eq!(tree[0].id, "W/root[0]");
        assert_eq!(child_ids(&tree[0]), ["W/root.a[0]", "W/root.b[0]"]);
        assert_eq!(child_ids(&tree[0].children[0]), ["W/root.a.x[0]"]);
    }

    /// Siblings must read in layout order. The tree is assembled deepest-first,
    /// which appends them backwards, and only the final pass puts them right.
    #[test]
    fn tree_keeps_siblings_in_order() {
        let tree = build_element_tree(
            "W",
            vec![
                element("root"),
                element("root.a"),
                element("root.b"),
                element("root.c"),
                element("root.a.deep"),
            ],
        );

        assert_eq!(
            child_ids(&tree[0]),
            ["W/root.a[0]", "W/root.b[0]", "W/root.c[0]"]
        );
    }

    /// A prefix only counts when it ends on a dot: `rootish` is not inside
    /// `root`, however much of the string they share.
    #[test]
    fn tree_respects_dot_boundaries() {
        let tree = build_element_tree("W", vec![element("root"), element("rootish")]);
        assert_eq!(tree.len(), 2);
    }

    /// gpui does not paint an inspector entry for every level, so the parent is
    /// the nearest ancestor that exists — not necessarily the direct one.
    #[test]
    fn tree_attaches_to_the_nearest_existing_ancestor() {
        let tree = build_element_tree("W", vec![element("root"), element("root.a.x")]);

        assert_eq!(tree.len(), 1);
        assert_eq!(child_ids(&tree[0]), ["W/root.a.x[0]"]);
    }

    #[test]
    fn tree_keeps_an_element_with_no_ancestor_at_the_top() {
        let tree = build_element_tree("W", vec![element("alone"), element("other")]);
        assert_eq!(tree.len(), 2);
        assert!(tree.iter().all(|element| element.children.is_empty()));
    }

    /// A painted element for the snapshot tests.
    fn ui_element(
        id: &str,
        source: Option<&str>,
        text: &[&str],
        children: Vec<UiElement>,
    ) -> UiElement {
        UiElement {
            id: id.to_string(),
            element_type: "test".into(),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            visible: true,
            children,
            properties: Default::default(),
            source_location: source.map(str::to_string),
            style_json: None,
            content_size: None,
            text_content: text.iter().map(|line| line.to_string()).collect(),
        }
    }

    fn render(nodes: &[SnapshotNode]) -> String {
        let mut out = RenderedSnapshot::default();
        render_snapshot(nodes, 0, false, 100, &mut out);
        out.text
    }

    /// The file that rendered an element is its role, for this crate's own
    /// widgets — that is the whole trick, so it had better hold.
    #[test]
    fn a_role_comes_from_the_file_that_rendered_it() {
        assert_eq!(
            role_for("crates/ui/src/button/button.rs:42:5"),
            Some("button")
        );
        assert_eq!(
            role_for("crates/ui/src/input/input.rs:1:1"),
            Some("textbox")
        );
        assert_eq!(
            role_for("crates/ui/src/list/list_item.rs:9:3"),
            Some("listitem")
        );
        // Windows records its paths with backslashes.
        let windows_path =
            ["crates", "ui", "src", "checkbox.rs:7:1"].join(std::path::MAIN_SEPARATOR_STR);
        assert_eq!(role_for(&windows_path), Some("checkbox"));
        // An app's own widget has no role until it says what it is.
        assert_eq!(role_for("src/my_widget.rs:3:1"), None);
        assert_eq!(role_for("crates/ui/src/button/mod.rs:1:1"), None);
    }

    #[test]
    fn only_written_id_segments_count_as_test_ids() {
        assert_eq!(test_id_of("view-1.root.save-button"), Some("save-button"));
        assert_eq!(test_id_of("view-1.search_input"), Some("search_input"));
        assert_eq!(test_id_of("view-1.item"), Some("item"));

        // Generated: a view handle, a numeric path, a type name.
        assert_eq!(test_id_of("root.view-4294967734"), None);
        assert_eq!(test_id_of("root.1-0-0"), None);
        assert_eq!(test_id_of("root.ResizablePanelGroup"), None);
    }

    #[test]
    fn a_name_is_the_painted_text_kept_short() {
        let element = ui_element("W/a[0]", None, &["  Save  ", "", "changes"], vec![]);
        assert_eq!(name_of(&element).as_deref(), Some("Save changes"));

        let empty = ui_element("W/a[0]", None, &["", "   "], vec![]);
        assert!(name_of(&empty).is_none());

        let long = "x".repeat(80);
        let wordy = ui_element("W/a[0]", None, &[&long], vec![]);
        let name = name_of(&wordy).unwrap();
        assert_eq!(name.chars().count(), 61, "60 characters plus the ellipsis");
        assert!(name.ends_with('…'));
    }

    /// The size difference against the full tree comes from dropping the
    /// scaffolding, so the scaffolding must actually be dropped — and what was
    /// inside it must survive.
    #[test]
    fn layout_wrappers_are_dropped_and_their_children_kept() {
        let tree = vec![ui_element(
            "W/view-1.ResizablePanelGroup[0]",
            Some("crates/ui/src/resizable/mod.rs:1:1"),
            &[],
            vec![ui_element(
                "W/view-1.ResizablePanelGroup.1-0-0[0]",
                Some("src/layout.rs:1:1"),
                &[],
                vec![ui_element(
                    "W/view-1.ResizablePanelGroup.1-0-0.save-button[0]",
                    Some("crates/ui/src/button/button.rs:1:1"),
                    &["Save"],
                    vec![],
                )],
            )],
        )];

        let nodes = snapshot_nodes(&tree, false);
        assert_eq!(render(&nodes), "- button \"Save\" #save-button @e1\n");
    }

    #[test]
    fn an_element_earns_a_line_by_role_id_or_text() {
        let tree = vec![
            ui_element("W/a.plain[0]", Some("src/x.rs:1:1"), &[], vec![]),
            ui_element("W/a.1-0-0[0]", Some("src/x.rs:1:1"), &["hello"], vec![]),
            ui_element(
                "W/a.2-0-0[0]",
                Some("crates/ui/src/switch.rs:1:1"),
                &[],
                vec![],
            ),
        ];

        assert_eq!(
            render(&snapshot_nodes(&tree, false)),
            "- node #plain @e1\n- node \"hello\" @e2\n- switch @e3\n"
        );
    }

    /// `title_bar.rs` paints the title bar and its close button alike, so the
    /// file name would call that button a banner. A region role has to earn
    /// itself by containing something.
    #[test]
    fn a_region_role_needs_something_inside_it() {
        let tree = vec![ui_element(
            "W/a.title-bar[0]",
            Some("crates/ui/src/title_bar.rs:1:1"),
            &[],
            vec![ui_element(
                "W/a.title-bar.close[0]",
                Some("crates/ui/src/title_bar.rs:9:1"),
                &[],
                vec![],
            )],
        )];

        assert_eq!(
            render(&snapshot_nodes(&tree, false)),
            "- banner #title-bar @e1\n  - node #close @e2\n"
        );
    }

    #[test]
    fn interactive_only_lifts_the_things_you_can_act_on() {
        let tree = vec![ui_element(
            "W/a.panel[0]",
            Some("crates/ui/src/group_box.rs:1:1"),
            &[],
            vec![
                ui_element(
                    "W/a.panel.title[0]",
                    Some("crates/ui/src/label.rs:1:1"),
                    &["Settings"],
                    vec![],
                ),
                ui_element(
                    "W/a.panel.dark-mode[0]",
                    Some("crates/ui/src/switch.rs:1:1"),
                    &[],
                    vec![],
                ),
            ],
        )];

        assert_eq!(
            render(&snapshot_nodes(&tree, true)),
            "- switch #dark-mode @e1\n"
        );
    }

    #[test]
    fn nesting_shows_as_indentation_and_refs_run_in_reading_order() {
        let tree = vec![ui_element(
            "W/a.sidebar[0]",
            Some("crates/ui/src/sidebar/menu.rs:1:1"),
            &[],
            vec![
                ui_element(
                    "W/a.sidebar.item[0]",
                    Some("crates/ui/src/list/list_item.rs:1:1"),
                    &["One"],
                    vec![],
                ),
                ui_element(
                    "W/a.sidebar.item[1]",
                    Some("crates/ui/src/list/list_item.rs:1:1"),
                    &["Two"],
                    vec![],
                ),
            ],
        )];

        assert_eq!(
            render(&snapshot_nodes(&tree, false)),
            "- menu #sidebar @e1\n  - listitem \"One\" #item @e2\n  - listitem \"Two\" #item @e3\n"
        );
    }

    #[test]
    fn a_filter_keeps_the_path_to_what_matched() {
        let tree = vec![ui_element(
            "W/a.sidebar[0]",
            Some("crates/ui/src/sidebar/menu.rs:1:1"),
            &[],
            vec![
                ui_element(
                    "W/a.sidebar.item[0]",
                    Some("crates/ui/src/list/list_item.rs:1:1"),
                    &["Accordion"],
                    vec![],
                ),
                ui_element(
                    "W/a.sidebar.item[1]",
                    Some("crates/ui/src/list/list_item.rs:1:1"),
                    &["Badge"],
                    vec![],
                ),
            ],
        )];

        let mut nodes = snapshot_nodes(&tree, false);
        filter_snapshot(&mut nodes, "accordion");
        assert_eq!(
            render(&nodes),
            "- menu #sidebar @e1\n  - listitem \"Accordion\" #item @e2\n"
        );
    }

    #[test]
    fn a_snapshot_says_when_it_stopped_early() {
        let tree: Vec<UiElement> = (0..5)
            .map(|index| {
                ui_element(
                    &format!("W/a.row-{index}[0]"),
                    Some("crates/ui/src/button/button.rs:1:1"),
                    &[],
                    vec![],
                )
            })
            .collect();

        let mut out = RenderedSnapshot::default();
        render_snapshot(&snapshot_nodes(&tree, false), 0, false, 2, &mut out);

        assert!(out.truncated);
        assert_eq!(out.shown, 2);
        assert_eq!(out.refs.len(), 2);
    }

    #[test]
    fn an_id_that_is_not_a_ref_passes_through_untouched() {
        assert_eq!(expand_ref("save-button").unwrap(), "save-button");
        assert_eq!(
            expand_ref("WindowId(1)/view-1.panel[0]").unwrap(),
            "WindowId(1)/view-1.panel[0]"
        );
    }

    /// The snapshot prints `#item`, so `#item` is what an agent copies. An
    /// earlier version accepted only the bare `item` and answered "element not
    /// found" for a string it had just printed itself.
    #[test]
    fn the_hash_a_snapshot_prints_is_accepted() {
        assert_eq!(expand_ref("#save-button").unwrap(), "save-button");
        assert_eq!(expand_ref("save-button").unwrap(), "save-button");
    }

    /// A ref from a snapshot that has been replaced must fail loudly rather
    /// than resolve to whatever now sits on that line.
    #[test]
    fn an_unknown_ref_says_to_take_a_new_snapshot() {
        let error = expand_ref("@e9999").expect_err("no such ref");
        assert!(error.contains("@e9999"), "{error}");
        assert!(error.contains("ui_snapshot"), "{error}");
    }

    fn audit(tree: &[UiElement], min_target: f32) -> Vec<Finding> {
        let nodes = snapshot_nodes(tree, false);
        let mut flat = Vec::new();
        flatten_snapshot(&nodes, &mut flat);
        let mut findings = audit_controls(&flat, min_target);
        findings.extend(audit_ids(&flat));
        findings.extend(audit_unstable_ids(&flat));
        findings
    }

    fn button(id: &str, text: &[&str]) -> UiElement {
        ui_element(
            id,
            Some("crates/ui/src/button/button.rs:42:5"),
            text,
            vec![],
        )
    }

    /// An icon button with no label is the classic one: a screen reader has
    /// nothing to say, and an agent has nothing to match on.
    #[test]
    fn a_control_with_no_text_is_serious() {
        // A generous minimum, so only the naming check can fire.
        let findings = audit(&[button("W/a.bell[0]", &[])], 1.0);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "unnamed-control");
        assert_eq!(findings[0].severity, "serious");
        assert_eq!(findings[0].test_id.as_deref(), Some("bell"));
        assert_eq!(
            findings[0].source.as_deref(),
            Some("crates/ui/src/button/button.rs:42:5"),
            "a finding without a line to open is a chore"
        );
    }

    #[test]
    fn a_named_control_of_a_decent_size_is_fine() {
        let findings = audit(&[button("W/a.save[0]", &["Save"])], 10.0);
        assert!(findings.is_empty(), "{:?}", findings[0].check);
    }

    #[test]
    fn a_target_under_the_minimum_is_a_warning() {
        // The helper paints everything 10x10.
        let findings = audit(&[button("W/a.save[0]", &["Save"])], 24.0);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "target-too-small");
        assert_eq!(findings[0].severity, "warning");
        assert!(
            findings[0].message.contains("10x10"),
            "{}",
            findings[0].message
        );
    }

    /// Text alone does not make something a control, so a label is not audited
    /// as one.
    #[test]
    fn only_interactive_elements_are_audited_as_controls() {
        let label = ui_element(
            "W/a.title[0]",
            Some("crates/ui/src/label.rs:1:1"),
            &["Settings"],
            vec![],
        );
        assert!(audit(&[label], 24.0).is_empty());
    }

    /// The finding that matters to both readers of this tool: an id that names
    /// several things breaks a screen reader's promise and a recorded script
    /// alike.
    #[test]
    fn an_id_naming_several_controls_is_serious() {
        let findings = audit(
            &[
                button("W/a.item[0]", &["One"]),
                button("W/a.item[1]", &["Two"]),
                button("W/a.item[2]", &["Three"]),
            ],
            10.0,
        );

        let duplicate = findings
            .iter()
            .find(|finding| finding.check == "duplicate-id")
            .expect("a duplicate finding");
        assert_eq!(duplicate.severity, "serious");
        assert!(
            duplicate.message.contains("names 3 elements"),
            "{}",
            duplicate.message
        );
        assert_eq!(duplicate.test_id.as_deref(), Some("item"));
    }

    /// The one bad id that looks exactly like a good one. Nothing else in the
    /// derived layer can tell `#input-4294967299` from `#save-button`, and the
    /// difference only shows on the next app start.
    #[test]
    fn an_id_carrying_an_entity_number_is_reported() {
        let findings = audit(&[button("W/a.input-4294967299[0]", &["Search"])], 10.0);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "unstable-id");
        assert_eq!(findings[0].severity, "warning");
        assert_eq!(findings[0].test_id.as_deref(), Some("input-4294967299"));
    }

    /// A counter is something somebody wrote. Reporting `#item-3` would train
    /// the reader to skip the check.
    #[test]
    fn a_small_number_in_an_id_is_left_alone() {
        let findings = audit(&[button("W/a.item-3[0]", &["One"])], 10.0);
        assert!(findings.is_empty(), "{} findings", findings.len());
    }

    /// Sixty rows sharing a generated id are one problem with one fix, so they
    /// get one line — the duplicate check is what says how many there are.
    #[test]
    fn one_generated_id_on_many_elements_is_one_finding() {
        let findings = audit(
            &[
                button("W/a.row-4294967300[0]", &["One"]),
                button("W/a.row-4294967300[1]", &["Two"]),
            ],
            10.0,
        );

        let unstable: Vec<_> = findings
            .iter()
            .filter(|finding| finding.check == "unstable-id")
            .collect();
        assert_eq!(unstable.len(), 1);
        assert!(
            unstable[0].message.contains("these 2 elements"),
            "{}",
            unstable[0].message
        );
    }

    #[test]
    fn a_repeated_id_on_things_you_cannot_click_is_only_a_warning() {
        let panel =
            |id: &str| ui_element(id, Some("crates/ui/src/group_box.rs:1:1"), &["x"], vec![]);
        let findings = audit(&[panel("W/a.panel[0]"), panel("W/a.panel[1]")], 1.0);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "duplicate-id");
        assert_eq!(findings[0].severity, "warning");
    }

    #[test]
    fn severity_ranks_order_the_list() {
        assert!(severity_rank("serious") > severity_rank("warning"));
        assert!(severity_rank("warning") > severity_rank("none"));
        assert_eq!(severity_rank("nonsense"), 0);
    }

    /// `none` means report without gating. An earlier version compared against
    /// a floor of one, which quietly turned it into "warning" — caught by
    /// running the audit against a real app, not by reading the code.
    #[test]
    fn fail_on_none_always_passes() {
        let findings = audit(&[button("W/a.bell[0]", &[])], 1.0);
        assert_eq!(findings.len(), 1, "a serious finding to gate on");

        assert!(!audit_passes(&findings, "serious"));
        assert!(!audit_passes(&findings, "warning"));
        assert!(audit_passes(&findings, "none"));
    }

    #[test]
    fn a_warning_only_fails_at_the_warning_threshold() {
        // 10x10, named: too small, nothing worse.
        let findings = audit(&[button("W/a.save[0]", &["Save"])], 24.0);
        assert_eq!(findings[0].severity, "warning");

        assert!(audit_passes(&findings, "serious"));
        assert!(!audit_passes(&findings, "warning"));
    }

    #[test]
    fn a_clean_window_passes_at_every_threshold() {
        let findings = audit(&[button("W/a.save[0]", &["Save"])], 1.0);
        assert!(findings.is_empty());

        for threshold in ["serious", "warning", "none"] {
            assert!(audit_passes(&findings, threshold), "{threshold}");
        }
    }

    #[test]
    fn an_id_matches_in_all_three_forms() {
        let full = "WindowId(1)/view-1.panel[0]";
        let global = "view-1.panel";

        assert!(id_matches(full, global, full), "full id");
        assert!(id_matches(full, global, global), "global id");
        assert!(id_matches(full, global, "panel"), "suffix");
        assert!(!id_matches(full, global, "sidebar"));
    }

    /// The id `format: "compact"` prints is shortened and keeps its instance
    /// suffix. Handing it straight back must work, or the cheap output format
    /// would be a trap.
    #[test]
    fn an_id_copied_from_compact_output_still_matches() {
        let global = "view-1.gpui_component::sidebar::SidebarMenu.item";
        let full = format!("WindowId(1)/{global}[0]");
        let compact = shorten_element_id(&full);

        assert_eq!(compact, "WindowId(1)/view-1.SidebarMenu.item[0]");
        assert!(id_matches(&full, global, &compact), "compact full id");
        assert!(
            id_matches(&full, global, "view-1.SidebarMenu.item"),
            "compact global id"
        );
        assert!(id_matches(&full, global, "SidebarMenu.item"), "suffix");
        assert!(!id_matches(&full, global, "view-1.Sidebar.item"));
    }

    /// An instance suffix on the query used to make every path fail: it is not
    /// part of a global_id, so nothing it was compared against could match.
    #[test]
    fn an_instance_suffix_does_not_break_a_query() {
        let global = "view-1.panel";
        let full = "WindowId(1)/view-1.panel[0]";

        assert!(id_matches(full, global, "view-1.panel[0]"));
        assert!(id_matches(full, global, "panel[0]"));
    }

    /// The methods whose answer has to wait for a frame are exactly the ones
    /// that change something.
    #[test]
    fn only_the_inputs_count_as_input() {
        for method in [
            methods::CLICK_ELEMENT,
            methods::SEND_KEY,
            methods::TYPE_TEXT,
            methods::EXECUTE_ACTION,
        ] {
            assert!(is_input_method(method), "{method} changes the app");
        }
        for method in [
            methods::GET_WINDOWS,
            methods::INSPECT_UI_TREE,
            methods::TAKE_SCREENSHOT,
            methods::GET_LOGS,
            methods::WAIT_FOR,
        ] {
            assert!(!is_input_method(method), "{method} only reads");
        }
    }

    #[test]
    fn a_batch_step_inherits_the_window_but_may_override_it() {
        let inherited = with_window_default(&json!({ "key": "enter" }), Some("WindowId(2)"));
        assert_eq!(inherited["window_id"], "WindowId(2)");
        assert_eq!(inherited["key"], "enter");

        let explicit =
            with_window_default(&json!({ "window_id": "WindowId(9)" }), Some("WindowId(2)"));
        assert_eq!(explicit["window_id"], "WindowId(9)");

        let no_params = with_window_default(&serde_json::Value::Null, Some("WindowId(2)"));
        assert_eq!(no_params["window_id"], "WindowId(2)");

        let no_batch_window = with_window_default(&json!({ "key": "enter" }), None);
        assert!(no_batch_window.get("window_id").is_none());
    }

    #[test]
    fn a_wait_with_nothing_to_wait_for_is_recognised() {
        assert!(!has_conditions(&WaitForParams::default()));

        assert!(has_conditions(&WaitForParams {
            element_id: Some("results".into()),
            ..Default::default()
        }));
        assert!(has_conditions(&WaitForParams {
            app_state_path: Some("/app/rows".into()),
            ..Default::default()
        }));
        // `absent` inverts a condition; on its own there is nothing to invert.
        assert!(!has_conditions(&WaitForParams {
            absent: true,
            ..Default::default()
        }));
    }
}
