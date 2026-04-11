//! MCP (Model Context Protocol) Integration for GPUI Apps.
//!
//! Starts an IPC server that allows the gpui-mcp-server to
//! access UI state and dispatch events.
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

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

use gpui::{point, px, App, Keystroke, MouseButton as GpuiMouseButton, Pixels};
use gpui_mcp_protocol::protocol::*;
use serde_json::json;

/// Maximum number of stored log entries
const MAX_LOG_ENTRIES: usize = 500;

/// Type for request messages from IPC thread to main thread
type RequestMsg = (IpcRequest, mpsc::Sender<IpcResponse>);

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
pub fn mcp_set_app_state_provider(
    provider: impl Fn(&App) -> serde_json::Value + Send + 'static,
) {
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
/// Starts a Unix Domain Socket listener on a background thread and
/// polls incoming requests on the GPUI main thread.
pub fn init_mcp(cx: &mut App, app_name: &str) {
    let socket_path = socket_path_for(app_name);

    let (req_tx, req_rx) = mpsc::channel::<RequestMsg>();

    // Start IPC server on background thread
    let path = socket_path.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_ipc_listener(&path, req_tx) {
            eprintln!("[MCP] IPC Server error: {}", e);
        }
    });

    mcp_log(format!("MCP IPC Server started on {}", socket_path));
    eprintln!("[MCP] IPC Server listening on {}", socket_path);

    // Main thread polling: receives requests and handles them with GPUI access
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;

            // Process all pending requests
            while let Ok((request, resp_tx)) = req_rx.try_recv() {
                let ipc_response = cx.update(|cx| handle_request(&request, cx));
                let _ = resp_tx.send(ipc_response);
            }
        }
    })
    .detach();
}

/// Unix Socket listener loop (runs on background thread)
fn run_ipc_listener(
    socket_path: &str,
    req_tx: mpsc::Sender<RequestMsg>,
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
    req_tx: mpsc::Sender<RequestMsg>,
) -> anyhow::Result<()> {
    let reader = BufReader::new(&stream);
    let mut writer = &stream;

    for line in reader.lines() {
        let line = line?;
        let request: IpcRequest = serde_json::from_str(&line)?;

        let (resp_tx, resp_rx) = mpsc::channel();

        req_tx.send((request, resp_tx)).map_err(|e| {
            anyhow::anyhow!("Failed to send request to main thread: {}", e)
        })?;

        let response = resp_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| IpcResponse {
                id: String::new(),
                result: Err("Request timeout (10s)".into()),
            });

        let response_json = serde_json::to_string(&response)?;
        writer.write_all(response_json.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    Ok(())
}

/// Handle an IPC request on the GPUI main thread
fn handle_request(request: &IpcRequest, cx: &mut App) -> IpcResponse {
    let result = match request.method.as_str() {
        methods::GET_WINDOWS => handle_get_windows(cx),
        methods::CLICK_ELEMENT => handle_click_element(&request.params, cx),
        methods::SEND_KEY => handle_send_key(&request.params, cx),
        methods::GET_APP_STATE => handle_get_app_state(cx),
        methods::GET_LOGS => handle_get_logs(),
        methods::INSPECT_UI_TREE => handle_inspect_ui_tree(&request.params, cx),
        methods::GET_ELEMENT => handle_get_element(&request.params, cx),
        methods::TAKE_SCREENSHOT => handle_take_screenshot(&request.params, cx),
        methods::EXECUTE_ACTION => handle_execute_action(&request.params, cx),
        methods::LIST_ACTIONS => handle_list_actions(&request.params, cx),
        methods::GET_FOCUS_INFO => handle_get_focus_info(&request.params, cx),
        methods::TYPE_TEXT => handle_type_text(&request.params, cx),
        _ => Err(format!("Unknown method: {}", request.method)),
    };

    IpcResponse {
        id: request.id.clone(),
        result,
    }
}

// ===== Helpers =====

/// Resolve a window handle from an optional window_id string.
/// Falls back to: active window → first window.
fn resolve_window(
    window_id: Option<&str>,
    cx: &mut App,
) -> Result<gpui::AnyWindowHandle, String> {
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
/// `get_focus_info` call to answer "what changed?". Inlining both into
/// the driver response halves the round-trips for the most common pattern.
///
/// `window_id` is the window the action targeted; it's used to scope
/// `focus_info` to the same window. Pass `None` to use the default target.
fn attach_post_state(
    mut response: serde_json::Value,
    window_id: Option<&str>,
    cx: &mut App,
) -> serde_json::Value {
    let app_state = handle_get_app_state(cx).unwrap_or(serde_json::Value::Null);

    let focus_params = json!({ "window_id": window_id });
    let focus_info = handle_get_focus_info(&focus_params, cx).unwrap_or(serde_json::Value::Null);

    if let Some(obj) = response.as_object_mut() {
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
    Ok(attach_post_state(result, event.window_id.as_deref(), cx))
}

/// Resolve the center point of an element by ID.
/// Searches all windows (or a specific one) for the element and returns its bounds center.
fn resolve_element_center(
    query: &str,
    window_id: Option<&str>,
    cx: &mut App,
) -> Result<(gpui::Point<Pixels>, String), String> {
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
                let full_id =
                    format!("{}/{}[{}]", window_id_str, info.global_id, info.instance_id);

                let matches = full_id == query
                    || info.global_id == query
                    || info.global_id.ends_with(query);

                if matches {
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

fn handle_send_key(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
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
        .update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx)
        })
        .map_err(|e| e.to_string())?;

    mcp_log(format!("Key '{}' dispatched={}", keystroke_str, dispatched));
    let response = json!({
        "success": true,
        "dispatched": dispatched,
        "keystroke": keystroke_str,
    });
    Ok(attach_post_state(response, event.window_id.as_deref(), cx))
}

fn handle_type_text(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: TypeTextParams =
        serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

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
            .update(cx, |_, window, cx| {
                window.dispatch_keystroke(keystroke, cx)
            })
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
    Ok(attach_post_state(response, opts.window_id.as_deref(), cx))
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
    let opts: InspectUiTreeParams =
        serde_json::from_value(params.clone()).unwrap_or(InspectUiTreeParams {
            max_depth: 0,
            window_id: None,
            element_type_filter: None,
            root_element_id: None,
            format: None,
            text_filter: None,
        });

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
                    let mut element_children =
                        build_element_tree(&window_id_str, inspector_elems);

                    // If root_element_id is set, find that subtree
                    if let Some(ref root_id) = opts.root_element_id {
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
        // Match by full ID
        if elem.id == query {
            return Some(elem.clone());
        }
        // Match by global_id portion (strip window prefix and instance suffix)
        let global_id = elem
            .id
            .find('/')
            .map(|i| &elem.id[i + 1..])
            .unwrap_or(&elem.id);
        let global_id = global_id
            .rfind('[')
            .map(|i| &global_id[..i])
            .unwrap_or(global_id);
        if global_id == query {
            return Some(elem.clone());
        }
        // Match by suffix
        if global_id.ends_with(query) {
            return Some(elem.clone());
        }
        // Recurse into children
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

/// Build a hierarchical tree from GPUI's flat inspector element list.
/// Uses dot-separated global_id as hierarchy key.
/// Optimized: builds parent lookup via sorted prefix matching instead of O(n²) scan.
fn build_element_tree(
    window_id: &str,
    elements: Vec<gpui::InspectorElementInfo>,
) -> Vec<UiElement> {
    use std::collections::HashMap;

    struct FlatEntry {
        full_id: String,
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
                full_id: full_id.clone(),
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

    // Build hierarchy using sorted order for efficient parent lookup
    let mut id_to_element: HashMap<String, UiElement> = HashMap::new();
    let mut id_to_global: HashMap<String, String> = HashMap::new();
    let mut insertion_order: Vec<String> = Vec::new();

    for entry in &entries {
        id_to_element.insert(entry.full_id.clone(), entry.element.clone());
        id_to_global.insert(entry.full_id.clone(), entry.global_id.clone());
        insertion_order.push(entry.full_id.clone());
    }

    // Assign children to parents (deepest first)
    let mut child_assigned: HashMap<String, bool> = HashMap::new();

    for i in (0..insertion_order.len()).rev() {
        let child_id = &insertion_order[i];
        let child_global = id_to_global[child_id].clone();

        let mut best_parent: Option<String> = None;
        let mut best_prefix_len = 0;

        for j in 0..insertion_order.len() {
            if j == i {
                continue;
            }
            let candidate_id = &insertion_order[j];
            let candidate_global = &id_to_global[candidate_id];

            if child_global.starts_with(candidate_global.as_str())
                && child_global.len() > candidate_global.len()
                && child_global.as_bytes().get(candidate_global.len()) == Some(&b'.')
                && candidate_global.len() > best_prefix_len
            {
                best_prefix_len = candidate_global.len();
                best_parent = Some(candidate_id.clone());
            }
        }

        if let Some(parent_id) = best_parent {
            if let Some(child_elem) = id_to_element.remove(child_id) {
                if let Some(parent_elem) = id_to_element.get_mut(&parent_id) {
                    parent_elem.children.push(child_elem);
                    child_assigned.insert(child_id.clone(), true);
                }
            }
        }
    }

    // Return only top-level elements
    insertion_order
        .iter()
        .filter(|id| !child_assigned.contains_key(*id))
        .filter_map(|id| id_to_element.remove(id))
        .collect()
}

fn handle_get_element(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let params: GetElementParams =
        serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
    let query = &params.element_id;

    // Build the full tree for each window and search by ID.
    // This ensures the returned element includes its full subtree and text content.
    for handle in cx.windows() {
        let result = handle.update(cx, |_, window, _cx| {
            let window_id_str = format!("{:?}", handle.window_id());
            let inspector_elems = window.inspector_elements();
            let children = build_element_tree(&window_id_str, inspector_elems);

            // Check if query matches the window itself
            if &window_id_str == query {
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
    let opts: TakeScreenshotParams =
        serde_json::from_value(params.clone()).unwrap_or(TakeScreenshotParams {
            highlight_elements: vec![],
            window_id: None,
            element_id: None,
        });

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;

    let (image, scale_factor) = handle
        .update(cx, |_, window, _cx| {
            let scale = window.scale_factor();
            let img = window.render_to_image()?;
            Ok::<_, anyhow::Error>((img, scale))
        })
        .map_err(|e| format!("Failed to access window: {}", e))?
        .map_err(|e| format!("Failed to render screenshot: {}", e))?;

    // If element_id is set, resolve bounds and crop
    let (final_image, element_info) = if let Some(ref element_id) = opts.element_id {
        // Resolve element bounds (in logical pixels)
        let bounds_result = handle
            .update(cx, |_, window, _cx| {
                let window_id_str = format!("{:?}", handle.window_id());
                for info in window.inspector_elements() {
                    let full_id = format!(
                        "{}/{}[{}]",
                        window_id_str, info.global_id, info.instance_id
                    );
                    let matches = full_id == *element_id
                        || info.global_id == *element_id
                        || info.global_id.ends_with(element_id.as_str());
                    if matches {
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

    let (width, height) = final_image.dimensions();

    // Save as PNG to a temp file
    let temp_path =
        std::env::temp_dir().join(format!("gpui-screenshot-{}.png", std::process::id()));
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
    if let Some(id) = element_info {
        result
            .as_object_mut()
            .map(|o| o.insert("element_id".into(), json!(id)));
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
                Some(focus_handle) => {
                    focus_handle.dispatch_action(action.as_ref(), window, cx)
                }
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
    Ok(attach_post_state(response, opts.window_id.as_deref(), cx))
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
    let available_action_names: Option<std::collections::HashSet<String>> = if opts.only_available
    {
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

                    let context = binding
                        .predicate()
                        .map(|p| format!("{}", p));

                    let mut entry = json!({
                        "keys": keystrokes.join(" "),
                    });
                    if let Some(ctx) = context {
                        entry.as_object_mut()
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
    let opts: GetFocusInfoParams =
        serde_json::from_value(params.clone()).unwrap_or(GetFocusInfoParams { window_id: None });

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
}
