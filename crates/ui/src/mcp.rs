//! MCP (Model Context Protocol) Integration für GPUI Apps.
//!
//! Startet einen IPC Server der es dem gpui-mcp-server ermöglicht,
//! auf UI-Zustand zuzugreifen und Events zu dispatchen.
//!
//! ## Benutzung
//!
//! ```ignore
//! fn main() {
//!     let app = Application::new();
//!     app.run(|cx| {
//!         gpui_component::init(cx);
//!         gpui_component::mcp::init_mcp(cx);
//!         // ... App-Code ...
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

/// Maximale Anzahl gespeicherter Log-Einträge
const MAX_LOG_ENTRIES: usize = 500;

/// Typ für Request-Nachrichten vom IPC-Thread an den Main-Thread
type RequestMsg = (IpcRequest, mpsc::Sender<IpcResponse>);

/// Globaler Log-Buffer, thread-safe
static LOG_BUFFER: std::sync::LazyLock<Arc<Mutex<VecDeque<String>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))));

/// Konvertiert gpui::Pixels zu f32
fn px_to_f32(p: Pixels) -> f32 {
    f32::from(p)
}

/// Konvertiert gpui::Bounds<Pixels> zu protocol::Bounds
fn convert_bounds(b: gpui::Bounds<Pixels>) -> Bounds {
    Bounds {
        x: px_to_f32(b.origin.x),
        y: px_to_f32(b.origin.y),
        width: px_to_f32(b.size.width),
        height: px_to_f32(b.size.height),
    }
}

/// Fügt einen Log-Eintrag hinzu (kann von überall aufgerufen werden)
pub fn mcp_log(message: impl Into<String>) {
    if let Ok(mut buffer) = LOG_BUFFER.lock() {
        if buffer.len() >= MAX_LOG_ENTRIES {
            buffer.pop_front();
        }
        buffer.push_back(message.into());
    }
}

/// Initialisiert den MCP IPC Server.
///
/// Startet einen Unix Socket Listener auf einem Background-Thread und
/// pollt eingehende Requests auf dem GPUI Main-Thread.
pub fn init_mcp(cx: &mut App) {
    let socket_path = std::env::var("GPUI_MCP_SOCKET")
        .unwrap_or_else(|_| "/tmp/gpui-mcp.sock".to_string());

    let (req_tx, req_rx) = mpsc::channel::<RequestMsg>();

    // IPC Server auf Background-Thread starten
    let path = socket_path.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_ipc_listener(&path, req_tx) {
            eprintln!("[MCP] IPC Server error: {}", e);
        }
    });

    mcp_log(format!("MCP IPC Server gestartet auf {}", socket_path));
    eprintln!("[MCP] IPC Server listening on {}", socket_path);

    // Main-Thread Polling: empfängt Requests und handelt sie mit GPUI-Zugriff
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;

            // Alle pending Requests abarbeiten
            while let Ok((request, resp_tx)) = req_rx.try_recv() {
                let ipc_response = cx.update(|cx| handle_request(&request, cx));
                let _ = resp_tx.send(ipc_response);
            }
        }
    })
    .detach();
}

/// Unix Socket Listener Loop (läuft auf Background-Thread)
fn run_ipc_listener(
    socket_path: &str,
    req_tx: mpsc::Sender<RequestMsg>,
) -> anyhow::Result<()> {
    // Alten Socket entfernen
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

/// Handelt eine einzelne IPC-Verbindung (läuft auf Connection-Thread)
fn handle_ipc_connection(
    stream: std::os::unix::net::UnixStream,
    req_tx: mpsc::Sender<RequestMsg>,
) -> anyhow::Result<()> {
    let reader = BufReader::new(&stream);
    let mut writer = &stream;

    for line in reader.lines() {
        let line = line?;
        let request: IpcRequest = serde_json::from_str(&line)?;

        // Oneshot-Channel für Response
        let (resp_tx, resp_rx) = mpsc::channel();

        // Request an Main-Thread senden
        req_tx.send((request, resp_tx)).map_err(|e| {
            anyhow::anyhow!("Failed to send request to main thread: {}", e)
        })?;

        // Auf Response warten (mit Timeout)
        let response = resp_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| IpcResponse {
                id: String::new(),
                result: Err("Request timeout".into()),
            });

        let response_json = serde_json::to_string(&response)?;
        writer.write_all(response_json.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    Ok(())
}

/// Handelt einen IPC Request auf dem GPUI Main-Thread
fn handle_request(request: &IpcRequest, cx: &mut App) -> IpcResponse {
    let result = match request.method.as_str() {
        methods::GET_WINDOWS => handle_get_windows(cx),
        methods::CLICK_ELEMENT => handle_click_element(&request.params, cx),
        methods::SEND_KEY => handle_send_key(&request.params, cx),
        methods::GET_APP_STATE => handle_get_app_state(cx),
        methods::GET_LOGS => handle_get_logs(),
        methods::INSPECT_UI_TREE => handle_inspect_ui_tree(cx),
        methods::GET_ELEMENT => handle_get_element(&request.params, cx),
        methods::TAKE_SCREENSHOT => handle_take_screenshot(&request.params),
        methods::EXECUTE_ACTION => handle_execute_action(&request.params),
        _ => Err(format!("Unknown method: {}", request.method)),
    };

    IpcResponse {
        id: request.id.clone(),
        result,
    }
}

// ===== Handler Implementierungen =====

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

    let Some(handle) = cx.active_window() else {
        return Err("No active window".into());
    };

    handle
        .update(cx, |_, window, cx| {
            window.dispatch_click(position, button, cx);
        })
        .map_err(|e| e.to_string())?;

    mcp_log(format!("Click at ({}, {})", event.x, event.y));
    Ok(json!({ "success": true }))
}

fn handle_send_key(
    params: &serde_json::Value,
    cx: &mut App,
) -> Result<serde_json::Value, String> {
    let event: KeyEvent = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    // Keystroke-String bauen: [ctrl-][alt-][shift-][cmd-]key
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

    mcp_log(format!("Key '{}' dispatched: {}", keystroke_str, dispatched));
    Ok(json!({ "success": true, "dispatched": dispatched }))
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
                        "bounds": {
                            "x": bounds.x,
                            "y": bounds.y,
                            "width": bounds.width,
                            "height": bounds.height,
                        },
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

    Ok(json!({ "logs": logs }))
}

fn handle_inspect_ui_tree(cx: &mut App) -> Result<serde_json::Value, String> {
    let active_window_id = cx.active_window().map(|w| w.window_id());

    let children: Vec<UiElement> = cx
        .windows()
        .iter()
        .filter_map(|handle| {
            handle
                .update(cx, |_, window, _cx| {
                    let bounds = window.bounds();
                    let converted = convert_bounds(bounds);
                    let window_id_str = format!("{:?}", handle.window_id());

                    // Collect inspector elements from the rendered frame
                    let inspector_elems = window.inspector_elements();
                    let element_children = build_element_tree(&window_id_str, inspector_elems);

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

    serde_json::to_value(&tree).map_err(|e| e.to_string())
}

/// Baut aus einer flachen Liste von InspectorElementInfo einen hierarchischen Baum auf.
/// Nutzt die dot-separierte global_id (z.B. "view-1.panel.sidebar") als Hierarchie-Schlüssel.
fn build_element_tree(
    window_id: &str,
    elements: Vec<gpui::InspectorElementInfo>,
) -> Vec<UiElement> {
    use std::collections::HashMap;

    // Jedes Element bekommt eine eindeutige ID: window_id/global_id[instance_id]
    struct FlatEntry {
        full_id: String,
        global_id: String,
        element: UiElement,
    }

    // Flache Einträge erzeugen
    let mut entries: Vec<FlatEntry> = elements
        .into_iter()
        .map(|info| {
            let full_id = format!("{}/{}[{}]", window_id, info.global_id, info.instance_id);

            // Element-Typ aus dem Dateinamen der Source-Location extrahieren
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

    // Nach global_id-Tiefe sortieren (weniger dots = höher in Hierarchie)
    entries.sort_by(|a, b| {
        let depth_a = a.global_id.matches('.').count();
        let depth_b = b.global_id.matches('.').count();
        depth_a.cmp(&depth_b).then(a.global_id.cmp(&b.global_id))
    });

    // Hierarchie aufbauen: Element ist Kind eines anderen wenn dessen global_id
    // ein Prefix des Kinds ist (mit dot-Trennung).
    // Wir bauen bottom-up: Elemente mit der größten Tiefe werden zuerst als Kinder zugeordnet.
    let mut id_to_element: HashMap<String, UiElement> = HashMap::new();
    let mut id_to_global: HashMap<String, String> = HashMap::new();
    let mut insertion_order: Vec<String> = Vec::new();

    for entry in &entries {
        id_to_element.insert(entry.full_id.clone(), entry.element.clone());
        id_to_global.insert(entry.full_id.clone(), entry.global_id.clone());
        insertion_order.push(entry.full_id.clone());
    }

    // Kinder den Eltern zuordnen (von tiefstem zu flachstem)
    // Für jedes Element suchen wir das nächste Eltern-Element (längster Prefix-Match)
    let mut child_assigned: HashMap<String, bool> = HashMap::new();

    // Von tiefstem zu flachstem durchgehen
    for i in (0..insertion_order.len()).rev() {
        let child_id = &insertion_order[i];
        let child_global = id_to_global[child_id].clone();

        // Suche besten Eltern-Kandidat: längster global_id der ein echtes Prefix ist
        let mut best_parent: Option<String> = None;
        let mut best_prefix_len = 0;

        for j in 0..insertion_order.len() {
            if j == i {
                continue;
            }
            let candidate_id = &insertion_order[j];
            let candidate_global = &id_to_global[candidate_id];

            if child_global.starts_with(candidate_global)
                && child_global.len() > candidate_global.len()
                && child_global.as_bytes().get(candidate_global.len()) == Some(&b'.')
                && candidate_global.len() > best_prefix_len
            {
                best_prefix_len = candidate_global.len();
                best_parent = Some(candidate_id.clone());
            }
        }

        if let Some(parent_id) = best_parent {
            // Kind aus der Map nehmen und dem Elternteil hinzufügen
            if let Some(child_elem) = id_to_element.remove(child_id) {
                if let Some(parent_elem) = id_to_element.get_mut(&parent_id) {
                    parent_elem.children.push(child_elem);
                    child_assigned.insert(child_id.clone(), true);
                }
            }
        }
    }

    // Rückgabe: nur Top-Level-Elemente (die keinem Elternteil zugeordnet wurden)
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

    // Suche über alle Windows — zuerst Window-Level, dann Inspector-Elemente
    for handle in cx.windows() {
        let result = handle.update(cx, |_, window, _cx| {
            let window_id_str = format!("{:?}", handle.window_id());

            // Exakte Window-ID-Suche
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

            // Inspector-Element-Suche: exakte full_id oder global_id-Match
            for info in window.inspector_elements() {
                let full_id =
                    format!("{}/{}[{}]", window_id_str, info.global_id, info.instance_id);

                let matches = full_id == *query
                    || info.global_id == *query
                    || info.global_id.ends_with(query);

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
    _params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Placeholder: render_to_image ist nur hinter test-support Feature verfügbar
    Ok(json!({
        "error": "Screenshots are not yet supported (requires test-support feature)",
        "png_base64": "",
        "width": 0,
        "height": 0,
        "highlighted_elements": []
    }))
}

fn handle_execute_action(
    params: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: ExecuteActionParams =
        serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;

    mcp_log(format!("Execute action: {} (args: {})", params.action, params.args));

    // Action-Dispatch über String-Name erfordert eine Action-Registry.
    // Da wir Actions nicht dynamisch aus Strings konstruieren können,
    // geben wir erstmal eine Info-Meldung zurück.
    Ok(json!({
        "status": "not_implemented",
        "message": format!(
            "Dynamic action dispatch not yet supported. Action: '{}', Args: {}",
            params.action, params.args
        )
    }))
}
