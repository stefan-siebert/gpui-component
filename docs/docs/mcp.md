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

        // Optional: what a known starting state is, for `reset_app`. A
        // recorded script can then begin from one instead of from
        // whatever the last session left behind.
        gpui_component::mcp::mcp_set_reset_hook(|_arguments, cx| {
            // close documents, clear selection, go back to the first tab
            Ok(())
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
| `a11y_audit` | controls nothing can name, ids that name several elements, targets under 24px |
| `a11y_tree` | the AccessKit tree: real roles, announced labels, input values, node actions — annotated elements only |
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
| `set_viewport` | resize a window to an exact content size, so layout is reproducible |
| `reset_app` | put the app back into a known starting state, via `mcp_set_reset_hook` |
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

## Checking accessibility

`a11y_audit` reports the problems the derived layer can see, and they turn out
to be the ones that hurt a screen-reader user and an agent equally:

| check | severity | what it means |
|---|---|---|
| `unnamed-control` | serious | an interactive element painting no text — nothing to announce, nothing to target by name |
| `duplicate-id` | serious on a control, else warning | one id naming several elements; a suffix match takes the first |
| `target-too-small` | warning | a side under 24 px (WCAG 2.2's minimum) |
| `zero-size-control` | serious | an interactive element with no area at all |
| `unstable-id` | warning | an id ending in a number generated fresh on every start, like `#input-4294967299` — it reads like a name and is not one |

Icon-only buttons and repeated list-row ids are what a first run usually turns
up, in any application — they are the normal state of a UI that nobody has had
reason to name yet.

The fix serves both readers at once: give the element a label and an id of its
own, and it becomes announceable *and* targetable. Contrast is not checked and
cannot be, because colours never reach the MCP side.

Put the audit into a recorded script and a failing one fails the replay, which
keeps it checked:

```json
{ "method": "a11y_audit", "params": { "fail_on": "serious" } }
```

## The real accessibility tree

The audit reads a derived layer. GPUI also builds the actual AccessKit tree —
the one a screen reader is handed — and `a11y_tree` returns it: real roles
(`Button`, `MenuBar`, `TextInput`), the label a control announces even when it
paints only an icon, an input's current value, and the actions each node
offers.

It is sparser than the window, because a node exists only where somebody
annotated the element. Against this repo's own gallery that is 11 nodes over
96 painted elements, and the answer reports both numbers rather than letting
the tree pass for the whole UI.

You rarely need the tool itself. `ui_snapshot` already folds the tree into its
lines: where an element has a node, the declared role wins over the one
inferred from the source file, the label supplies a name when nothing is
painted, state is appended (`checked`, `selected`, `value="…"`), and the line
ends in `✓` so a declared fact is never mistaken for an inferred one. The
audit uses the same overlay — it reports `announced` beside `checked`, and
`unnamed-control` now distinguishes an element that needs a label from one
that needs a role first.

The join is exact rather than approximate: gpui derives a node's id from the
same `GlobalElementId` the inspector reports, and `InspectorElementInfo`
carries it. Matching on the node's own leaf element id and source location
would not do — this repo's four title-bar buttons share both.

GPUI builds the tree only while assistive technology is attached. Reading it
switches the window into building it and waits one frame, which is why the
first `ui_snapshot`, `a11y_audit` or `a11y_tree` against a window costs a
frame more than the ones after it. That needs
`Window::set_a11y_force_active` from the patched gpui this crate builds
against.

The widgets that already annotate themselves are Button, ToggleButton,
Checkbox, Radio, Switch, Tab, List, MenuItem and PopupMenu. Anything else in
your app needs `.role(...)` and `.aria_label(...)` before it appears.

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
