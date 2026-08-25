---
title: MCP Inspector
description: Let an AI coding agent look at and drive your running app.
order: 0
---

The `mcp` feature starts a small server inside your running app that an AI
agent — Claude Code, Claude Desktop, anything speaking the
[Model Context Protocol](https://modelcontextprotocol.io) — can use to look at
the UI and drive it: read what is on screen, click, type, press keys, dispatch
named actions, take screenshots, and read a state snapshot your app defines.

The point is that the agent checks a UI change the way a person would, by using
the app, instead of guessing from the code.

::: warning Fork only
This feature lives in the
[stefan-siebert/gpui-component](https://github.com/stefan-siebert/gpui-component)
fork and needs the matching gpui fork, because it uses inspector APIs upstream
gpui does not expose yet.
:::

## How it fits together

```
 your agent
     │  MCP over stdio
     ▼
 gpui-mcp-server                 ← a separate binary, one per machine
     │  one JSON request per connection, over a Unix-domain socket
     │  {temp_dir}/gpui-mcp-{app}-{pid}.sock
     ▼
 your app
   gpui_component::mcp           ← this feature
```

The server is stateless and re-discovers the socket on every call, so you can
restart your app as often as you like without restarting the agent's session.

## Turning it on

Put it behind a feature of your own, so a shipped build never carries an IPC
server:

```toml
[features]
mcp = ["gpui-component/mcp"]

[dependencies]
gpui = { git = "https://github.com/stefan-siebert/zed", branch = "gpui-mcp-patches-v2" }
gpui-component = { git = "https://github.com/stefan-siebert/gpui-component", branch = "main" }
```

gpui-component names that same git source for gpui, so the two resolve to
**one** gpui. A second copy — a path dependency next to the git one, or
upstream gpui from crates.io — makes every type mismatch.

Then start it once, after `gpui_component::init`:

```rs
app.run(|cx| {
    gpui_component::init(cx);

    #[cfg(feature = "mcp")]
    {
        // The name is how the server finds this app among others.
        gpui_component::mcp::init_mcp(cx, "my-app");

        // Optional: what `get_app_state` should report for your app.
        gpui_component::mcp::mcp_set_app_state_provider(|cx| {
            serde_json::json!({ "rows": 0, "selected": null })
        });
    }

    // ... windows, views ...
});
```

`init_mcp` binds `{temp_dir}/gpui-mcp-my-app-{pid}.sock` and spawns a listener
thread. Requests are answered on the GPUI main thread, so they see consistent
state and can dispatch real input. `gpui_component::mcp::mcp_log(..)` appends
to the 500-entry buffer the agent can read back.

Build with the feature during development, without it for release. Building the
server binary and registering it with an agent is described in the
[gpui-mcp README](https://github.com/stefan-siebert/gpui-mcp).

## What the agent can do

| tool | what it does |
|---|---|
| `gpui_guide` | the server's own documentation, so an agent starts informed |
| `ui_snapshot` | the window as one short line per meaningful element |
| `inspect_ui_tree` | the full element hierarchy, for layout questions |
| `get_element` | one element with its subtree |
| `get_windows`, `get_app_state` | windows, and your state provider's snapshot |
| `get_focus_info` | the focus handle and the active key contexts |
| `list_actions`, `execute_action` | your app's GPUI actions, and dispatching one |
| `send_key`, `type_text`, `click_element` | real input through the focus chain |
| `wait_for` | wait, in-app and per frame, until a condition holds |
| `batch` | several of the above in one request |
| `take_screenshot` | the window or one element, rendered by GPUI itself |
| `get_logs` | the `mcp_log` buffer |
| `replay_script` | replay a recorded session — to reach a state, or as a test |

## What the agent sees

`ui_snapshot` is the cheap way to look at a window:

```
ui_snapshot 4 — WindowId(1v1), 9 of 96 painted elements
- button #github @e1
- menu #gallery-sidebar @e2
  - listitem "Accordion" #item @e3
- textbox #search @e4
```

An element earns a line by having a **role**, an **id somebody chose**, or
**text**. Everything else is layout scaffolding: it is dropped and its children
take its place. On a real window this is a small fraction of the size of the
full tree, which matters because an agent pays for what it reads.

### Where the roles come from

From the file that rendered the element. gpui-component keeps one widget per
file, so `button/button.rs` renders a `button` and `list/list_item.rs` renders
a `listitem` — your app gets that vocabulary without annotating anything.

Two rules keep it honest:

- A role describing a *region* (`banner`, `list`, `dialog`, …) is only used
  when the element actually contains something. One file paints both a title
  bar and its close button, and calling that button a banner would be worse
  than saying nothing.
- Widgets **your** app writes have no role yet. They appear by their id and
  their text, which is usually enough to find and click them.

### Giving elements good names

The snapshot prints an id segment as `#name` when it looks like something a
person wrote — lowercase, with dashes or underscores. Generated segments
(`view-4294967734`, `1-0-0`, type names) are left out, because they change.

```rs
div().id("results").child(..)     // shows up as #results
```

So the single most useful thing you can do for an agent driving your app is to
give the elements it should target explicit, stable ids. See
[ElementId](./element_id) for how ids compose.

The `@e3` at the end of each line is a **ref**: shorthand for "the thing on
that line of the snapshot I just showed you". It works anywhere an element id
is taken. Each snapshot replaces the whole set, so a ref from an older snapshot
fails with a message saying to take a new one, rather than resolving to
whatever now sits on that line.

## What the answers mean

Everything the agent reads — the element tree, the snapshot, screenshots —
comes from the **last painted frame**. That would make an answer to a click
describe the app *before* the click, so the input tools do not answer until the
frame showing their effect has been painted, and the app state and focus info
they carry describe that frame.

- `settled: true` — that frame arrived.
- `settled: false` — no frame was painted while the tool waited. The window is
  minimised, occluded, or on a platform that stops drawing invisible windows,
  and everything in that answer describes an older frame.

Work your app starts on its own — an async load, a debounce, an animation — is
not covered by that, which is what `wait_for` is for. It checks once per
painted frame, inside the app, until an element, a text, a key context or a
value in your app state matches.

## Cost

The `mcp` feature enables gpui's `inspector`, which records hitboxes and
painted text on every frame. That is a development cost, and the reason this
belongs behind a feature flag.

While idle the MCP server does nothing at all: the listener thread wakes the
main thread when a request arrives, rather than the main thread polling for
one.

## Security

The in-app server gives anything that can reach the socket full control of the
UI and a view of its state. It is a development tool: keep it behind a feature
flag, never enable it in a shipped build, and remember that the socket lives in
a per-user temp directory but is not otherwise authenticated.
