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
//!         gpui_component::mcp::init_mcp(cx);
//!         // ... app code ...
//!     });
//! }
//! ```

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

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

/// Add a log entry (can be called from anywhere)
pub fn mcp_log(message: impl Into<String>) {
    if let Ok(mut buffer) = LOG_BUFFER.lock() {
        if buffer.len() >= MAX_LOG_ENTRIES {
            buffer.pop_front();
        }
        buffer.push_back(message.into());
    }
}

/// Initialize the MCP IPC server.
///
/// Starts a Unix Socket listener on a background thread and
/// polls incoming requests on the GPUI main thread.
pub fn init_mcp(cx: &mut App) {
    let socket_path = std::env::var("GPUI_MCP_SOCKET")
        .unwrap_or_else(|_| "/tmp/gpui-mcp.sock".to_string());

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
    stream: std::os::unix::net::UnixStream,
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

// ===== Handler Implementations =====

fn handle_get_windows(cx: &mut App) -> Result<serde_json::Value, String> {
    let active_window_id = cx.active_window().map(|w| w.window_id());

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

    let position = point(px(event.x), px(event.y));
    let handle = resolve_window(event.window_id.as_deref(), cx)?;

    handle
        .update(cx, |_, window, cx| {
            window.dispatch_click(position, button, cx);
        })
        .map_err(|e| e.to_string())?;

    mcp_log(format!("Click at ({}, {}) button={:?}", event.x, event.y, event.button));
    Ok(json!({ "success": true }))
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

    let Some(handle) = cx.active_window() else {
        return Err("No active window".into());
    };

    let dispatched = handle
        .update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx)
        })
        .map_err(|e| e.to_string())?;

    mcp_log(format!("Key '{}' dispatched={}", keystroke_str, dispatched));
    Ok(json!({ "success": true, "dispatched": dispatched, "keystroke": keystroke_str }))
}

fn handle_get_app_state(cx: &mut App) -> Result<serde_json::Value, String> {
    let active_window_id = cx.active_window().map(|w| format!("{:?}", w.window_id()));
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

    Ok(json!({
        "window_count": window_count,
        "active_window": active_window_id,
        "windows": windows,
    }))
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
        });

    let active_window_id = cx.active_window().map(|w| w.window_id());

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

                    // Apply depth limit (elements at depth 1 are window children)
                    if opts.max_depth > 0 {
                        truncate_tree(&mut element_children, 1, opts.max_depth);
                    }

                    // Apply type filter
                    if let Some(ref filter) = opts.element_type_filter {
                        let filter_lower = filter.to_lowercase();
                        filter_tree(&mut element_children, &filter_lower);
                    }

                    UiElement {
                        id: window_id_str,
                        element_type: "Window".to_string(),
                        bounds: converted.clone(),
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
                        content_size: Some((converted.width, converted.height)),
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

    for handle in cx.windows() {
        let result = handle.update(cx, |_, window, _cx| {
            let window_id_str = format!("{:?}", handle.window_id());

            if &window_id_str == query {
                let converted = convert_bounds(window.bounds());
                let inspector_elems = window.inspector_elements();
                let children = build_element_tree(&window_id_str, inspector_elems);
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
                });
            }

            for info in window.inspector_elements() {
                let full_id =
                    format!("{}/{}[{}]", window_id_str, info.global_id, info.instance_id);

                let matches = full_id == *query
                    || info.global_id == *query
                    || info.global_id.ends_with(query.as_str());

                if matches {
                    let element_type = info
                        .source_location
                        .rsplit('/')
                        .next()
                        .and_then(|f| f.split('.').next())
                        .unwrap_or("Element")
                        .to_string();

                    let bounds = convert_bounds(info.bounds);
                    let mut properties = std::collections::HashMap::new();
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

                    return Some(UiElement {
                        id: full_id,
                        element_type,
                        bounds: bounds.clone(),
                        visible: true,
                        children: vec![],
                        properties,
                        source_location: Some(info.source_location),
                        style_json: None,
                        content_size: Some((bounds.width, bounds.height)),
                    });
                }
            }

            None
        });

        if let Ok(Some(element)) = result {
            return serde_json::to_value(&element).map_err(|e| e.to_string());
        }
    }

    Err(format!("Element not found: {}", params.element_id))
}

fn handle_take_screenshot(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: TakeScreenshotParams =
        serde_json::from_value(params.clone()).unwrap_or(TakeScreenshotParams {
            highlight_elements: vec![],
            window_id: None,
        });

    let handle = resolve_window(opts.window_id.as_deref(), cx)?;

    let image = handle
        .update(cx, |_, window, _cx| window.render_to_image())
        .map_err(|e| format!("Failed to access window: {}", e))?
        .map_err(|e| format!("Failed to render screenshot: {}", e))?;

    let (width, height) = image.dimensions();

    // Save as PNG to a temp file
    let temp_path = std::env::temp_dir().join(format!("gpui-screenshot-{}.png", std::process::id()));
    image
        .save(&temp_path)
        .map_err(|e| format!("Failed to save screenshot: {}", e))?;

    mcp_log(format!(
        "Screenshot captured: {}x{} -> {}",
        width,
        height,
        temp_path.display()
    ));

    Ok(json!({
        "width": width,
        "height": height,
        "format": "png",
        "path": temp_path.to_string_lossy(),
    }))
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

    handle
        .update(cx, |_, window, cx| {
            window.dispatch_action(action, cx);
        })
        .map_err(|e| format!("Failed to dispatch action: {}", e))?;

    mcp_log(format!("Executed action: {}", opts.action));
    Ok(json!({ "success": true, "action": opts.action }))
}

fn handle_list_actions(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let opts: ListActionsParams =
        serde_json::from_value(params.clone()).unwrap_or(ListActionsParams { filter: None });

    let all_names = cx.all_action_names();

    let actions: Vec<&str> = if let Some(ref filter) = opts.filter {
        let filter_lower = filter.to_lowercase();
        all_names
            .iter()
            .filter(|name| name.to_lowercase().contains(&filter_lower))
            .copied()
            .collect()
    } else {
        all_names.to_vec()
    };

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

            match focused {
                Some(focus_handle) => {
                    json!({
                        "has_focus": true,
                        "focus_handle": format!("{:?}", focus_handle),
                        "window_id": window_id,
                        "window_title": window.window_title(),
                    })
                }
                None => {
                    json!({
                        "has_focus": false,
                        "window_id": window_id,
                        "window_title": window.window_title(),
                    })
                }
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(info)
}
