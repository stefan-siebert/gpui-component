# gpui-component Fork — Patches & Changes

**Fork:** `stefan-siebert/gpui-component`, branch `main` (local checkout `../gpui-component`, consumed by Elane as a *path* dependency: `gpui-component = { path = "../gpui-component/crates/component" }`, `gpui-component-assets = { package = "gpui-kit-assets", path = "../gpui-component/crates/assets" }`).
**Upstream:** `longbridge/gpui-component` (`upstream/main`).
**Baseline:** upstream `8b3631f0` (2026-10-21), merged by the 2026-10-21 merge commit (§14).
**Inventory below:** written against tag `pre-upstream-merge-2026-10-21` = `cefb81b2` (the fork before
that merge; previous merge base `f8cd4860`, 2026-09-26). §14 records what the merge changed on top —
read it before trusting a section's commit list.

This file inventories the **custom work on top of upstream**. The authoritative picture is the
**net diff vs the merge base, per file** — many of the 131 custom commits were written against
`crates/ui` (before upstream's `gpui-base` split on 2026-09-01 and the `crates/ui → crates/component`
rename on 2026-09-25) and were re-seated by hand inside merge commits. Their original hashes therefore
no longer touch the files they live in today; `git log -- <path>` on a current path misses them. Where
that is the case the table names the original commit *and* the merge that re-seated it.

## How to regenerate this list

```bash
cd ../gpui-component
MB=$(git merge-base HEAD upstream/main)
# Custom commits (upstream PRs carry "(#NNNN)", fork commits do not):
git log --oneline --no-merges $MB..HEAD
# Net diff of all fork patches vs the upstream merge point (the truth):
git diff --stat $MB..HEAD
git diff --stat $MB..HEAD -- crates ':!crates/component/src/mcp.rs'   # without the MCP server
# Merge resolutions (which patches were re-seated or dropped, and why):
git log --merges --format='%h %ad%n%B' --date=short $MB~50..HEAD
# Upstream pressure per fork-touched file:
git diff --name-only $MB..HEAD | while read f; do
  echo "$(git log --oneline $MB..upstream/main -- "$f" | wc -l) $f"; done | sort -rn
```

zsh users: quote revision:path arguments as `"${T}:path"` — `$T:c…` is a zsh colon modifier.

---

## Summary

- **131 custom commits** (`--no-merges`, 2026-01-26 … 2026-10-19), **8 merges** of upstream
  (`85b01431` 03-16, `d952dbd8` 04-02, `2b6f4b3b` 04-20, `015fc094` 04-25, `d211c2c3` 06-17,
  `d21ef144` 08-04, `0086f5a9` 09-01, `5f336c79` 09-25).
- **Net diff:** 79 files, **+10,366 / −1,459** (incl. `Cargo.lock` +564/−642 and docs).
  - under `crates/`: 67 files, +9,222 / −762;
  - of which `crates/component/src/mcp.rs` alone: +4,726 (new file);
  - `crates/` without `mcp.rs`: 66 files, **+4,496 / −762**.
- 19 commits since the last merge (`5a9f2632` … `cefb81b2`, 2026-09-26 → 10-19) are the only ones
  that apply cleanly as commits; everything older exists only as merge-resolved net diff.

⚠️ Load-bearing for Elane. Elane's `CLAUDE.md` lists this fork among the forks that are "not optional".
The two hard couplings are (1) the **gpui dependency** (git fork, not `gpui-pre`) and (2) the **MCP
inspector server**, which needs gpui-fork-only APIs.

---

## 0. Build plumbing — gpui from the zed fork, not `gpui-pre` (decided 2026-09-25, `5f336c79`)

| Commit | Change |
|---|---|
| `7ea5f25e` | Point gpui to the zed fork (`stefan-siebert/zed`, branch `gpui-mcp-patches-v2`). |
| `5f336c79` (merge) | Kept the fork's gpui **by git** when upstream switched to exact-pinned `gpui-pre =0.3.6` snapshots from crates.io: "Elane's `[patch]` of the fork could not reach" crates.io snapshot packages. `gpui`, `gpui_platform`, `gpui_web`, `gpui_macros`, `reqwest_client` → git fork; `sum-tree` → `zed-sum-tree 0.2.0`; `reqwest` → `zed-industries/reqwest` rev `c1566246…` (`zed-reqwest 0.12.15-zed`). |
| `5f336c79` | `[patch."https://github.com/stefan-siebert/zed"]` → `../gpui-fork/crates/{gpui,gpui_platform,gpui_macros,http_client,reqwest_client}` (local sibling). |
| `747fe3d2` | `gpui-mcp-protocol = { package = "gpui-mcp-inspector", git = stefan-siebert/gpui-mcp, branch main }` **by git** (a by-git consumer — speedy/indexorama-gui — has no `../gpui-mcp`), plus `[patch."https://github.com/stefan-siebert/gpui-mcp"]` → `../gpui-mcp` for local work. |
| `d211c2c3` (merge) | `windows` workspace dep **0.62** (was upstream's 0.58) to unify with gpui's; hence `reduce_motion.rs` and `native_menu/windows.rs` use 0.62 spellings (`windows::core::BOOL`, `Option<HWND>`/`None` handle params, `GlobalFree(Some(..))`, `CreateStreamOnHGlobal(h, true)`) — `bd3eba84`. |
| `5f336c79` | `psm = { git = rust-lang/stacker }` folded into `[patch.crates-io]`. |
| `5f336c79`, 2026-10-21 merge | `component-macros/src/crate_path.rs`: `IntoPlot` derive also resolves a plain `gpui` crate — upstream only knows `gpui-kit` / `gpui-pre` / `gpui-fast`. `"gpui"` is tried **before** `"gpui-fast"`: proc-macro-crate finds that optional dependency in the manifest even while its feature is off, and the derive then names a crate that is not linked. |
| `b27f5d09` + merges | `t!(..)` results wrapped in `SharedString::from(..)` (`color_picker.rs` ×4, `dock/panel.rs`): the fork's gpui has no `IntoElement` for `Cow<'static, str>` that upstream's snapshot relies on. |
| `899c4c88` + `0086f5a9` | Every `PaintQuad { .. }` literal gets `effect_type: 0, effect_params: [0.0; 4]` — the gpui fork's quad-effects fields (gpui-fork `a9093714`). Sites: `base/{scrollable_mask,scrollbar,selectable_text,text/inline}.rs`, `shell/src/runtime.rs`, `story/.../checkerboard.rs`. |
| `3ad4ed60`, `20a8927e`, `ea4e996b`, `dfc4e99d` | Lockfile regenerations (taffy 0.12.1 for a gpui-fork layout fix, naga for gpui_windows, gpui-mcp protocol bump). |
| `9d5d9df8` | `.gitignore` `.claude/settings.local.json`. |
| `1daad621`, `2324b931`, `6e200380` | Clippy/compile fixes; `6e200380`: the tree-sitter-less `Tree` stub must be `Clone` (`InputState::set_highlighter` calls `tree().cloned()`). |

**Must survive merges**
- Root `Cargo.toml`: gpui family **by git from the fork + `[patch]` to `../gpui-fork`**. Taking upstream's
  `gpui = { package = "gpui-pre", version = "=0.3.9" }` hunk gives Elane two gpuis (Elane itself pins the
  fork by git and patches it to `../gpui-fork`) and nothing compiles. Upstream additionally added optional
  `gpui-fast*` deps (0.1.10, crates.io) — even optional, they enter `Cargo.lock` and pull the
  `gpui-pre-*` 0.3.9 graph into resolution; decide whether to keep the `gpui-fast` feature at all.
- Every **new** `PaintQuad` literal upstream adds needs the two effect fields (compile error otherwise).
  Upstream at `8b3631f0` has no new literal sites beyond the six already patched (its `plot/shape` sites
  use the `quad()` constructor, which the fork's gpui fills in).
- New `t!()` used directly as a child → wrap in `SharedString::from`.
- `windows = "0.62"`; any upstream Win32 code written against 0.58 needs the 0.62 spellings.

**Upstream's gpui baseline moved.** Upstream pinned `gpui-pre` 0.3.7 (`501cdbaa`, 09-28), 0.3.8
(`87d10ae5`, 10-05) and **0.3.9 (`1e7ffaa6`, 10-12) = `zed@93d7456` (2026-10-10, "git_ui: Fix commit
editor button hover colors (#65448)")**. The gpui fork (`../gpui-fork`, `gpui-mcp-patches-v2`) sits on
zed `933d8d93` (2026-09-25); `93d7456` is **not** an ancestor, and lies **243 zed commits** further. Upstream
gpui-component code from after 10-12 may call gpui API the fork does not have yet — merge zed ≥ `93d7456`
into the gpui fork *first* (the same order `d21ef144` recorded: "Must follow the gpui-fork merge").

---

## 1. MCP inspector server (`crates/component/src/mcp.rs`, feature `mcp`) — the fork's reason to exist

New file, +4,726 lines, 56 tests. Fork-only; needs gpui-fork inspector APIs. App side of
[stefan-siebert/gpui-mcp](https://github.com/stefan-siebert/gpui-mcp); wire types from `gpui-mcp-protocol`.
Feature: `mcp = ["inspector", "dep:gpui-mcp-protocol", "dep:image"]`; deps `async-channel`, `futures`
(unconditional), `image` (png, `opt-level = 3` in dev — screenshots), Windows: `uds_windows = "1.1"` +
`Win32_Networking_WinSock`. `lib.rs`: `#[cfg(feature = "mcp")] pub mod mcp;`. Story: `mcp` feature calls
`gpui_component::mcp::init_mcp(cx, "story")`. Docs: `website/docs/mcp.md` + `website/zh-CN/docs/mcp.md`,
`CLAUDE.md` section, README bullet.

| Commit | Change |
|---|---|
| `5b161bc7` | MCP integration for UI inspection and remote control. |
| `1c33ad73`, `1e463ba6` | `take_screenshot` via `render_to_image`; element-scoped crops. |
| `c2ec6c2f`, `6f522c36`, `8c47cfcd`, `3b2d3260` | AI-usability: compact ids (crate paths stripped), `text_filter`, `get_element` full subtrees, key contexts in `get_focus_info`. |
| `e0c11b56`, `e4595861` | Unix-domain-socket IPC on every platform, PID-based discovery, socket namespaced by app name (`gpui-mcp-<app>-<pid>.sock`). |
| `4f2c5e52`, `8aaaf9db`, `47f36ba3`, `45abb235` | default-window fallback; driver tools answer with post-dispatch `app_state` + `focus_info`; lookup failures list candidates; `list_actions only_available`. |
| `9dfee1b3`, `e95be7b2` | Protocol version stamped/checked on every answer; protocol v2: answer **after the frame**, in-app `wait_for`, `batch`. |
| `828906c9`, `fa516f4e`, `88157296`, `56bb3c9a`, `f6835592` | `ui_snapshot` with roles derived from the rendering source file (`ROLES` table); `a11y_audit`; unstable-id detection, `#id` accepted; `a11y_tree` (real AccessKit tree) and its overlay on snapshot/audit. |
| `7691dda1`, `104b9cfd` | `set_viewport` and `reset_app` (`mcp_set_reset_hook`); window measured by content size, scale factor reported. |
| `26954262` | `send_key`/`type_text` **press and release** — GPUI activates a focused button on key-*up*, so Enter/Space never clicked anything. |
| `286a79b2` | First request calls the fork's `App::request_inspector_ids` and waits a frame per window (zed #64309 builds inspector ids only while a window's own inspector is open → empty snapshots). Lazy, so an app no tool talks to keeps upstream's saving. Needs gpui-fork `1dd7db18`. |
| `e371a164`, `9220aa15` | Sandboxed macOS app binds in `~/Library/Caches/gpui-mcp` (`SANDBOX_SOCKET_DIR`, detects `APP_SANDBOX_CONTAINER_ID`); screenshots land there too. Doc comment lists the two temporary-exception entitlements required. |
| `939cd7ac` | `drag` (a frame painted between every step) and `scroll` (mouse moved first); answers carry selected text before/after release. |
| `c0040a36`, `d4098de7` | `send_key` `repeat` (hold a key): press, `delay_ms`, auto-repeats with `is_held` via gpui-fork `Window::dispatch_keystroke_repeat`, release; per-step `state_changes`. |
| `2797ebd6` | `take_screenshot` flattens the frame onto the theme background (`flatten_onto`): the readback is premultiplied RGBA with the window's alpha, so a translucent window (macOS glass) showed washed-out grey in every PNG. |
| `cefb81b2` | Windows: accept with WinSock `accept` and no peer address (under Wine the peer's family is not AF_UNIX and `uds_windows` refused every connection). |
| (this commit) | `ui_snapshot`: AccessKit `Row` reads as `row` and counts as interactive (table rows were missing from `interactive_only`); lines with the same full id merge into one (`merge_shared_ids`) — gpui reports an id-less child under its parent's id and node, so every tab printed twice, once nameless. |
| `f3abb84a`, `d867c579`, `b2e46ae8`, `2b2dfca3` | Documentation. |

Public API (Elane uses `init_mcp` in `src/services/inspector.rs` and `mcp_set_app_state_provider` in
`src/ui/main_window/mcp_state.rs`): `init_mcp(cx, app_name)`, `mcp_set_app_state_provider`,
`mcp_set_reset_hook`, `mcp_log`, `SANDBOX_SOCKET_DIR`.

**gpui-fork APIs it calls** (an upstream gpui lacks them): `App::request_inspector_ids`,
`Window::set_a11y_force_active`, `InspectorElementInfo::accesskit_node_id`, `inspector_elements()`,
`Window::dispatch_keystroke` / `dispatch_keystroke_repeat`, `render_to_image`, gpui's `inspector` feature.

**Must survive merges**
- `settle()` waits **two** frames (first forces a draw, second waits for the frame after it). Do not
  "simplify" to one frame callback — callbacks run before the draw (CLAUDE.md of this repo).
- `PROTOCOL_VERSION` lives in `gpui-mcp-protocol`; a protocol change is a three-repo commit
  (gpui-mcp, this file, Elane lockfile).
- `ROLES` maps *file names* to roles: upstream's new widget files (e.g. `diff`, `speech`, syntect
  views, anything moved between `component` and `base`) need entries, and renames silently drop roles.
- Windows accept path without peer address (`cefb81b2`).
- Tests: the 56 in `mcp.rs` (`socket_path_contains_app_and_pid`, `held_key_state_changes_show_a_flicker`,
  `the_hash_a_snapshot_prints_is_accepted`, `send_key_and_type_text_activate_a_focused_button`, …).

---

## 2. Root / window appearance — `RootExt` (`crates/component/src/root.rs`, `window_border.rs`)

| Commit | Change |
|---|---|
| `21719dd7`, `48be2ffa`, `d211c2c3` | Configurable `border_radius` on `WindowBorder` (all four corners; upstream rounds only the top two and uses the `BORDER_RADIUS` const). |
| `7c5fc117` | `Root::background_fn` (per-frame root background, alpha < 1 for translucent windows) + `Settings::content_style`. |
| `5f886676`, `664ada36` | Window border respects tiling state for resize zones; larger corner grips, refresh on mouse move. |
| `5f336c79` (merge) | Upstream moved `Root` into `gpui_base` with component state as a `RootPlugin`. The fork's appearance moved into `WindowState.appearance` (`WindowAppearance { bordered, shadow_size, border_radius, background_fn }`) and is set through the new **`RootExt`** trait on `gpui_base::Root` (methods take `cx`): `bordered`, `window_shadow_size`, `border_radius`, `background_fn`. `RootPlugin::style()` applies the background and rounds the content (CSD, non-tiled corners); `decorate()` wraps in `window_border()` only when `bordered`. `lib.rs`: `pub use root::RootExt;`. |

Elane: `RootExt` imported in `src/main.rs`, `editor_window.rs`, `help_window.rs`, `operations_center.rs`, …;
`.border_radius(px(10.), cx)` and `.background_fn(crate::ui::glass::root_background, cx)` on every window (10 sites).
`bordered`/`window_shadow_size` currently unused by Elane.

**Must survive merges:** `WindowState` must keep the `appearance` field and `style()`/`decorate()` must keep
consulting it — upstream changed `root.rs` in 5 commits since the base (tooltip/dialog/text-selection layers).
No tests lock this.

---

## 3. Title bar (`crates/component/src/title_bar.rs`, +509/−117, icons)

| Commit | Change |
|---|---|
| `5e1da312` | Structure simplified to Zed's approach (one container, all handlers there). |
| `2a49154b`, `c92bc3dd` | Windows: own drag, double-click (GPUI's `click_count` is unreliable over `WindowControlArea::Drag`), top-edge resize by posting `WM_NCLBUTTONDOWN+HTTOP`, stop propagation (a focusable parent's auto-focus `prevent_default` blocks DefWindowProc); drag only after 4px. |
| `e2a78073` + `crates/assets/assets/icons/generic_{close,maximize,minimize,restore}.svg` | Linux-native control style (small round buttons); Windows tall rectangular ones. Icons added to `default-icons.txt` (`5f336c79`, upstream replaced the directory scan by that list). |
| `13bffea4`, `7175ad6d`, `6ad7631c`, `47dbd4fe`, `bc42a322` | Unused-`event` warning churn across cfgs. |
| `b3a31dcd` | macOS fullscreen collapses the traffic-light padding. |
| `f7bf2d3e` | **Windows livelock fix:** a `WM_NCLBUTTONDOWN` the title bar posted itself comes back as a `MouseDown` and must fall through to DefWindowProc untouched (`synthetic_nc_down_posted`); on a maximized window it otherwise re-posts forever (100% core, frozen screen; measured 2026-08-20). No top-resize post on a maximized window. |
| `d21ef144` (merge) | Upstream's "only draw the controls the WM supports" (`88a854c7`) folded into the fork's icon builder. |
| `f58bfbc3` | Linux SSD guard qualified as `gpui::Decorations`. |
| `0086f5a9` (merge) | Upstream's gradient background wired into the fork body. |

Public API: `TitleBar::button_layout(Option<WindowButtonLayout>)` (follow the DE via `cx.button_layout()`),
`TitleBar::title(element)` (absolutely centred title), `on_close_window` works on Windows too (upstream: Linux only).
Uses gpui's `WindowButton`, `WindowButtonLayout`, `MAX_BUTTONS_PER_SIDE` (upstream zed #47506, not a fork patch).
Elane: `TitleBar::new().button_layout(cx.button_layout())` in 9+ windows.
Website `title-bar.md` (en/zh-CN) adjusted.

**Must survive merges:** the whole fork body (upstream touched the file once since the base:
`4921e5b6`, test ids). `synthetic_nc_down_posted` re-entry guard. No tests.

---

## 4. Settings: keyboard navigation, custom pages, external page control

Files: `setting/settings.rs` (+718/−50), `page.rs`, `item.rs`, `fields/{mod,string}.rs`, `setting/mod.rs`,
`setting/tests.rs`, `sidebar/menu.rs` (+99/−25), `lib.rs` (`setting::init(cx)`).

| Commit | Change |
|---|---|
| `1ea89009` | Keyboard navigation for the settings UI (first version). |
| `af920252` | `SettingPage::custom_content(Fn(&mut Window,&mut App)->AnyElement)` — page with header only and a caller-rendered body (Elane's keybindings table); reset-all suppressed. |
| `0786dc2f`, `23d508a0` | Sidebar resize range capped / default width trimmed (later folded into upstream's `sidebar_size_range`, `d21ef144`). |
| `da8046d0` | Input fields responsive/aligned: `min_w_0()` on horizontal field column. |
| `7a4f2983` | `SettingField::password(..)` — `SettingFieldType::Input { masked }`, `StringField::new(masked)` → `InputState::masked`. |
| `7c5fc117` | `Settings::content_style(&StyleRefinement)` for the content panel (translucent windows). |
| (fork, merge-carried) | `Settings::force_page(ix)`, `on_page_change(Fn(usize,&mut App))`, `auto_focus_search()`; after reset-all, `window.focus_next(cx)` so focus is not lost. |
| `a65f127a` | **Roving-tabindex navigation**: key context `SettingsNav` (bindings registered in `setting::init`: up/down/home/end/left/right/enter/space); `Nav`/`NavKey`/`NavHandlers`; exactly one entry is a tab stop; → opens a submenu or enters content, ← closes/goes to parent; Enter/Space go into the content. `SidebarMenu::key_context`, `SidebarMenuItem::track_focus(&FocusHandle)`, controlled `open(bool)` + `on_open_change`, focus-visible style. |
| `5f336c79` (merge) | `Nav` builds from upstream's `SettingsFilter` (original page/group indices), so `force_page`/`on_page_change` keep meaning the same page while searching; `SettingsFilter.custom` keeps custom-content pages visible under any query. |
| `5a9f2632` | Upstream's two pixel-row click tests `#[cfg_attr(windows, ignore)]` (Windows control boost, §9). |

Elane: `src/ui/components/settings_view.rs` uses `content_style`, `force_page`, `on_page_change`,
`auto_focus_search`, `custom_content` (×3); `settings_view/plugin_settings.rs` uses `SettingField::password`.

**Must survive merges:** `Activate` comment in `NavHandlers::activate` — Enter/Space are handled on key-down and
move focus so gpui's key-up click is dropped; do not turn it into `on_click`. Custom pages stay visible under search.
Tests: `one_tab_leads_from_the_navigation_into_the_content`, `arrows_move_through_the_navigation_and_switch_the_page`,
`right_opens_a_submenu_and_left_returns_to_its_page`, `enter_selects_the_page_once_and_moves_into_its_content`,
`right_on_an_entry_without_submenu_moves_into_the_content`.
Upstream pressure: `settings.rs` 0 upstream commits; `page.rs` 2 (`952202f6` focus rings in viewport, `14c26d30` bounce scroll);
`item.rs` 2 (`9254c580`, `e949d3e5` accessible names for fields); `sidebar/menu.rs` 1 (`29eaf422` caret name).

---

## 5. DataTable (`table/state.rs` +683/−324, `column.rs`, `data_table.rs`, `delegate.rs`)

| Commit | Change |
|---|---|
| `f4ee580d`, `7a8e4658`, `f440e9a8`, `55b9eabd`, `47ec024e`, `95926167` | `Column::auto_width()` / `flex_grow()` — fill remaining width (`update_auto_widths`, `prev_table_width`, `original_min_width`); live `flex_1` while idle (no resize lag), computed px width only during a column drag (`auto_columns_flex`); body renders scrollable columns directly in an `h_flex` (no horizontal virtual list — jitter fix); `min_w_full()` instead of `w_full()` (Wayland oscillation). Double-click on the resize handle resets an auto column. |
| `b457c6dd`, `fbc384ee`, `034f6e6f`, `42a1ae4c`, `9dde6fd3`, `4fcd78df`, `7a622ced`, `b4554217` | `Column::header_colspan(n)` (`header_col_spans`), flat and grouped headers; resize handle of a span targets the **last** spanned column and gets correct bounds; 8px grip; single-row header height + bottom border restored. |
| `ff14f753`, `b55c3b39` | Selection overlay: top border check uses `visible_range`; overlay clamped at viewport edges + 1px horizontal padding on `inner_table`. |
| `2d36e884`, `b10e2e24` | `TableState::last_clicked_col()`, `TableEvent::RowClick { row_ix, col_ix, click_count, was_already_selected }` (click-to-rename); right-click dismissal does not count as "already selected". |
| `26835e96` | `row_header` toggle restored after a merge. |
| (fork) | `TableState::set_col_groups_from_delegate` (rebuild columns without `cx.notify()`). |
| ~~`b974dcf0`~~ | ~~`DataTable::header_size(Size)`~~ — **dropped 2026-10-21** for upstream's `DataTable::header_row_height(Pixels)` (#3420, same purpose, also fixes PageUp/PageDown by the header band). Elane calls `.header_row_height(Size::Medium.table_row_height())`. |
| `1c453d8e` | `TableDelegate::visible_rows_rendered(range, ..)` — called on **every** frame the rows render (Elane's thumbnail scheduler publishes from it). |
| (fork, 2026-10-23) | `TableDelegate::has_context_menu()` (default `true`): `false` leaves the table's `ContextMenu` wrapper out. Upstream `c8fdf332` (#3402, in with the 2026-10-21 merge) makes that wrapper `stop_propagation` the right-button press it opens on, so a host with a row menu of its own never saw the press — Elane's file-panel row menu went dark, an empty table menu took the focus instead. Test: `a_right_click_reaches_a_host_with_its_own_menu` (fails with the wrapper installed). |
| `5f336c79` (merge) | Fork row rendering kept; selection read through upstream's accessors; upstream's cell→row click escalation adopted. |
| ~~`d6370e31`~~ | ~~Filling tables: a drag trades with the neighbour~~ — **superseded 2026-10-21 by `0f7a6d8c`** (rejected: the user wants every edge on the right pushed along). Its `fill_room()` stays: `update_auto_widths` runs every frame against the flat header row less its measured trailing empty column (`head_row_width`, `last_empty_col_width`), not the table's whole width — 14 px more than flex had, so a drag's first frame pushed every edge outward. |
| `0f7a6d8c` | **A dragged edge pushes every column on its right** (2026-10-21), at the auto column's edge too, and the last column has a handle. `fill_slack` = room − Σ widths after a resize; the auto column is room − others − slack, so release (flex again) does not take the drag back; the slack is drawn as `ml()` before the trailing spacer, negative when columns are pushed out of view (clipped). A pane resize changes only the auto column, down to its `min_width`. Double-click on the auto column's handle → slack 0. Rows get the header's trailing spacer (they flexed 12 px wider without it). The drag keeps its grab offset (`resize_grab_offset`) instead of `pointer − 2`. A spanned header cell's minimum = auto column's `min_width` + the other columns' widths. Flat headers only (`keeps_fill_slack`); grouped headers unchanged. |

Elane: `TableEvent::RowClick` (`file_panel.rs:1682`), `header_colspan` (`file_table.rs`), `Column::flex_grow()`
(`file_table.rs:371`), `visible_rows_rendered` (`file_table.rs`, `file_table/cells.rs`), `has_context_menu`
(`file_table.rs`, returns `false`).
`last_clicked_col`, `set_col_groups_from_delegate`, `.auto_width()` not called by Elane directly.

**Must survive merges:** body columns not virtualised horizontally; header and body flex/pin decisions flip together
(`auto_columns_flex`); computed fill widths equal the flexed ones (`fill_room` measures the header row and its empty
column — a new chrome element in the row must be subtracted there too, and the rows need the same trailing spacer
as the header); a resize in a filling table updates `fill_slack`, drawn in header and rows alike; colspan handle →
last spanned column; `visible_rows_rendered` call inside the uniform_list processor. Tests:
`dragging_an_edge_pushes_every_edge_on_its_right`, `a_pane_resize_changes_only_the_fill_column` and
`double_clicking_the_fill_edge_fills_the_room_again` (each with and without spanned headers; they compare the first
row's cells with the header) lock the fill layout and the drag — they fail against `03a1988e` and `d6370e31`; the
other table patches still have none.
Upstream pressure: `state.rs` **13** upstream commits, `data_table.rs` 6, `delegate.rs` 2, `column.rs` 2 (see §13).

---

## 6. Resizable (`base/src/resizable/{mod,panel}.rs`)

| Commit | Change |
|---|---|
| `3a9527ee` | No cumulative drift: `update_panel_size` adopts Taffy's size only on first layout or during a drag; no `cx.notify()` in steady state. |
| `d744516c` | `set_container_bounds`: a real container size change rescales panels proportionally (a 70/30 split stays 70/30); sub-pixel jitter is not a resize; stored sizes clamped to the panel's range. |
| `c4a7d423` | `ResizablePanelGroup::snap_to(fractions, threshold)` — a dragged handle sticks to fractions (`SnapConfig`). `ResizableState::is_resizing()`. |
| `2b05f1cd` | The first layout counts as a resize (upstream test `a_dumped_split_writes_the_sizes_it_is_actually_drawn_at`). |
| `5f336c79` (merge) | `set_container_bounds` returns whether it changed sizes; drives upstream's deferred settling notify. Upstream tests moved to `mod upstream_tests`. |

Elane: `src/ui/components/panel_splitter.rs` uses `.snap_to([0.5], px(SPLITTER_SNAP_PX))` and `is_resizing()`.
Tests: `container_growth_keeps_the_panel_proportions`, `container_shrink_keeps_the_panel_proportions`,
`a_frame_without_a_size_change_leaves_the_sizes_alone`, `a_panel_never_shrinks_below_its_range`,
`a_boundary_inside_the_window_lands_on_the_fraction` (+5 more `SnapConfig` tests).
Upstream pressure: 3 commits each — `8173d61c` **`ResizablePanel::fixed`** ("keep a panel's size on container
resizes": semantics next to the fork's proportional rescale), `2ef99681` "stop notifying state that did not
change" (same lines as `3a9527ee`), `bd8ed21f` custom resize handles.

---

## 7. Breadcrumb (`component/src/breadcrumb.rs`, +772/−5; 0 upstream commits)

| Commit | Change |
|---|---|
| `37054ad9` | `CollapsibleBreadcrumb` + `CollapsibleBreadcrumbState` (own `Element`: measure, hide middle segments behind "…", `on_ellipsis_click`) and middle truncation (`middle_truncate`, `BreadcrumbItem::max_label_chars`). |
| `a556cb43` | `BreadcrumbItem::on_hover`. |
| `2eb8a060`, `396e3611` | Shorten labels only when the (collapsed) row still overflows; default 24 chars is a last resort. |
| `6c3c71aa` | Collapse in the frame the path changes (measure detached in `request_layout`). |
| `13966a45` | `ellipsis_tooltip`; truncated labels show the full text as tooltip. |

Elane: `src/ui/components/breadcrumb.rs` (`CollapsibleBreadcrumb`). Tests: 14 (`nothing_overflows_before_the_first_measurement`
… `middle_truncate_slices_on_character_boundaries`).

---

## 8. Smaller widget patches

| Area / file | Commit | Change | Elane use | Tests |
|---|---|---|---|---|
| Checkbox tri-state `checkbox.rs` | `3e2d5b55`, `167ae344` | `Checkbox::state(CheckboxState)`, `indeterminate(bool)`, `pub use gpui_base::CheckboxState`; dash mark cross-fades with the check; label has no own `line_height` (descenders were clipped). | `change_attributes_dialog.rs` | `indeterminate_is_a_third_value_checked_does_not_reach` |
| Popup menu `menu/popup_menu.rs`, `menu/mod.rs` | `0c108eca` | `MenuIcon { Vector(Icon), Raster(Arc<RenderImage>) }` in every icon slot (`impl Into<MenuIcon>`); raster sized like an xsmall icon. Also `space`→Confirm, `tab`/`shift-tab`→Cancel in `PopupMenu` (`1ea89009`, settings keyboard work). | `services/shell_menu/submenu.rs` (`MenuIcon::Raster`) | `button_icon.rs` test adjusted (`5a9f2632`) |
| App menu bar `menu/app_menu_bar.rs` | `7762cc1a` | `set_selected_index` and `has_activated_menu` public (☰-collapsed bar, F10). | `main_window/titlebar.rs`, `help_window.rs`, `shell_menu_palette.rs` | — |
| Notification `notification.rs` | `61ce2034` | `autohide_after(Duration)` (default 5s), `on_expire` (timer only, not dismissal). | `src/ui/notify.rs` | `on_expire_fires_for_the_timer_and_not_for_a_dismissal` |
| Kbd `kbd.rs` | `6404141f` | `Kbd::arrow_glyphs()` → ←→↑↓ on every platform (`format_keystroke`). | `fkey_bar.rs` | `arrow_glyphs_draw_arrows_everywhere` |
| Kbd numpad names `kbd.rs` | (this commit) | The keypad's operator keys (`multiply`, `add`, `subtract`, `divide`, `decimal` — named apart from the main row by the gpui fork) display as `Num *`, `Num +`, … instead of a capitalised "Multiply". | Elane's selection commands in the command palette and shortcuts tab | `numpad_keys_are_named_as_numpad_keys` |
| Label `label.rs` | `5587c7b9` | `HighlightsMatch::Ranges(Vec<Range<usize>>)` + `From<Vec<Range>>`; unsliceable ranges dropped. | used via `.highlights(vec![..])` (no direct `HighlightsMatch` mention) | `test_highlight_ranges_supplied`, `…_rejects_unsliceable` |
| HoverCard / Popup `base/popup.rs`, `base/hover_card.rs`, `component/hover_card.rs` | `0ba0ef20`, `3b9f0da6`, `0086f5a9` | `content_offset(Point<Pixels>)` shifts the popup incl. occlude area, applied on top of `anchor_position(.., offset)` in `Popup`; forwarded by both HoverCards. | `file_table/cells.rs` | — |
| Text selection `base/text_selection.rs` | `fa50328c` | A bare mouse move while a gesture is open ends it (release taken by the platform, e.g. title-bar window move). | indirect | `a_release_the_window_never_saw_ends_the_gesture` (`selectable_text.rs`) |
| Re-exports `lib.rs` | `7fcfbd86` | `SelectableText`, `TextSelection`, `TextSelectionHandle` from `gpui_base`. | `preview_window/info_box.rs`, `hex.rs` | — |
| Separator `separator.rs` | `9106b4f9` | The separator keeps a minimum of 1 px along its axis (`min_h` horizontal, `min_w` vertical). Its line is absolute, so upstream's separator took no space: neighbours closed up over it and the line, centred on a zero-size box, sat on half a pixel. | every 1 px divider in Elane (dialogs, popups, context menu icon bar, hex group gaps, markdown rule) | `a_horizontal_separator_takes_the_pixel_it_draws`, `a_vertical_…` |
| Command `command/{command,item,state}.rs` | `ddb20a1b` | `CommandItem::description` (a second, muted line under the label, also searched; it sits below the label line as a block child, so it wraps — text in the row itself never would), `CommandItem::key_context` (the keybinding hint from that context instead of the palette's focus path), `Command::dispatch_actions(false)` (confirm reports through `on_confirm` only; the host dispatches after moving focus back). | `command_palette.rs` | `matches_the_description`, `confirm_without_dispatch_only_reports_the_item`, `key_context_picks_the_hint_and_a_description_adds_a_line`, `a_long_description_wraps_inside_the_row` |
| StatusBar `status_bar.rs` | `973a1fc3` | The `right` region does not shrink (`flex_shrink_0`). Every region clips (`overflow_hidden`), so upstream let both ends give way in proportion to their width: a long message on the left cut into the indicators on the right. | `status_bar.rs` (panel), `editor_window.rs` | `a_long_left_side_gives_way_to_the_right_side` |
| ListItem `list/list_item.rs` | `93aa11bf` | A selected item (with `list.active_highlight`, the default) draws `list_active_border` around itself, as the table's active row does; upstream filled it with `list_active` alone, which the theme caps at 20 % alpha — too faint to find on light themes. | `editor_window/lang_select.rs` (`List`), `favorites_popup.rs`, `theme_picker_popup.rs`; every `List`, `Select` dropdown | measured in Elane (pixels; no layout effect to test) |
| TextView `text/compat.rs` | `71a16233` | `TextView::image_source(resolver)` forwarded to gpui-base. | `help_window.rs` | — |
| Switch `base/switch.rs` | `c660df8e` → `9ba70c79` | `gpui_base::Switch::track_focus` doc + styled Switch passes its handle so the ring and Tab land on the same handle. (Fork's own border ring superseded by upstream's in `5f336c79`.) | settings/dialogs | `tab_focuses_the_caller_owned_handle` |
| Input `base/input/base/movement.rs` | `3c93460b` | Up/Down `cx.propagate()` in a single-line input (reach the list/dialog). | `change_dir.rs`, `path_dropdown.rs`, `plugin_dialog.rs` rely on it | — |
| Input on iOS `base/input/base/state.rs` | `3e278b8a` | `text_length_utf16` answered; `gpui_ios` bounds native text edits by it and offered no soft keyboard while it was `None`. | iPadOS edition | measured in the iPad simulator |
| Apple keyboard conventions on iOS `base/input/base/state.rs`, `base/text/state.rs`, `base/root.rs`, `component/kbd.rs`, `component/inspector.rs`, `shell/root.rs` | `92d40db7` | Keybinding and `Kbd` display cfgs read `target_vendor = "apple"` instead of `target_os = "macos"`, so an iPad with a hardware keyboard gets Cmd-A/C/X/V/Z, ⌥-word moves and ⌘ glyphs, as on the Mac (it got the Ctrl set). AppKit-only code (`native.rs`, `set_text_content_type`) stays `macos`. Tests unchanged: they run on the Mac. | iPadOS edition | measured in the iPad simulator (Cmd-A then a key replaces the field's text) |
| Display map `base/.../display_map.rs` | `de8acc53` | `is_simple_layout()` O(1) fast path for offset↔display point when no wrap and no folds. | internal perf | — |
| Highlighter `highlighter/highlighter.rs` | `de8acc53`, `e9765056` | `styles()` split into `highlight_items()` + `styles_from_items(&self, ..)`, skipping items outside the range; `HighlightItem` `pub(crate)`. (`language()` hunk is a pure move — upstream has the same method.) | internal | — |
| Fold ranges `highlighter/input_adapter.rs` | `0086f5a9` | Iterative `TreeCursor` walk (`collect_foldable_nodes`) instead of upstream's recursion; prunes single-line subtrees. | internal | — |
| Theme `theme/schema.rs` | `52c51193` | `apply_config` resets font/radius/shadow to `Theme::default()` when a theme omits them (Matrix `radius: 0` leaked). | theme picker | — |
| ~~Webview `crates/webview/*`~~ | ~~`effc9776`~~ | **Dropped 2026-10-21**: upstream ships Linux X11 itself (#3395, crate now `gpui-webview`); taken wholesale. | not used by Elane | — |
| Misc | `9d5d9df8`, `6e200380` | `#[allow(unused_imports)]` on `virtual_list` re-export. | — | — |

---

## 9. Platform-aware control sizing (`sizing.rs`, `button/button.rs`)

| Commit | Change |
|---|---|
| `7606dd2b`, `0086f5a9` | `platform_control_{h,px,py}_boost()` (Windows +4/+2/+2 px, else 0) applied in `Size::input_px/input_py` and `StyleSized::input_h` (explicit px instead of `h_5()`…), and in `Button` sizes. |
| `2114aac5` | Button focus: inset solid `theme.ring` border instead of an outset alpha glow (`button.rs` today carries no visible remainder of this in the net diff — upstream's `eb6c13ad` reworked the focus line). |
| `5a9f2632` | Tests adapted (`dock/tab_panel.rs` close button `20px + boost`; settings click tests ignored on Windows). |
| `00cf4d81` | **Control scale** (2026-10-24): `set_control_scale` / `control_scale` / `scaled_px`, one process-wide factor (an `AtomicU32` — the size helpers run in builder chains without `cx`). Button, `input_h`/`input_px`/`input_py`, `table_row_height` + cell padding, PopupMenu item height, Tab/TabBar heights and paddings, Switch track (thumb derived), `title_bar_height()` (+ traffic-light position) multiply by it. `Size::{button_height, input_height, menu_item_height}` public. Rem geometry (Checkbox, Radio, icons) untouched. Same commit: `PopupMenu` and `Kbd` `Sizable`, mono font size derived from the font size (13:16) when a theme sets none. Elane plan: `docs/superpowers/plans/2026-10-23-ui-density-and-sizing.md`. |
| `babe321d` | `PopupMenu` follows the scale and the font (2026-10-24): item padding `scaled_px(8)`, default max height `scaled_px(450)`, default max width 31.25 rem (was 500 px) — it bounds labels. |
| `b983b083` | `Toggle` (2026-10-27): a tier is `Size::button_height()` tall (was rems, Large 36 px) with upstream's paddings scaled, so toggles line up with buttons; pressed is `primary`/`primary_foreground` instead of the accent hover colour, which in darker themes could not be told from off; `Toggle::tab_stop` / `ToggleGroup::tab_stop` (`false`: no Tab stop, no focus on click, as `Button`). |
| `bdee1af5` | `Alert` (2026-10-28): paddings and gap through `scaled_px`; the icon's first-line offset in rems (`0.3125`, upstream's 5 px at 16 px), since icon and text follow the font. |

**Note:** in `Button` the fork also changes horizontal padding on **all** platforms relative to upstream
(Small `px_2`=8 → 12 px, Medium `px_2p5`=10 → 16 px, Large `px_3`=12 → 16 px, compact paddings likewise). That
looks like a carry-over from an older upstream than the boost alone would explain — decide deliberately at merge
(upstream `4708780a` "Align control text sizes with Input and Select" touched both `sizing.rs` and `checkbox.rs`).

---

## 10. Dropped earlier (named so a merge does not resurrect them)

- `DockItem::activate_panel` / `TabPanel::activate_panel` (`5bd5fd98`, PR #1 `85b01431`): absent from the tagged tree; Elane does not use it.
- Async/bulk highlight deferral in the input editor (kept in `d211c2c3`): gone from the net diff.
- Input display-map longest-row bookkeeping and fast/slow row mapping: replaced by upstream's SumTree wrap cache (`d21ef144`).
- `InputState::set_selected_range` / `select_range` (`c6800a6e`, `e1349fbf`, `8429016a`): upstream has `set_selected_range` now (Elane uses it in `rename.rs`, `archive_dialog.rs`, `save_as.rs` — via upstream).
- Fork's own Switch focus-ring overlay (`c660df8e`): superseded by upstream's ring (`5f336c79`), only the handle fix `9ba70c79` remains.
- `macos_accessibility` module declaration (`83b04a61`): moved to gpui-base upstream.

---

## 11. Elane-facing public API added by the fork (breaks Elane if lost)

Confirmed by grep in Elane `src/` and `crates/` (2026-10-21):

| API | Elane call sites |
|---|---|
| `RootExt::{border_radius, background_fn}` (+ `bordered`, `window_shadow_size` unused) | `main.rs`, `editor_window.rs`, `help_window.rs`, `operations_center.rs`, … (10) |
| `mcp::{init_mcp, mcp_set_app_state_provider}` | `services/inspector.rs`, `main_window/mcp_state.rs` |
| `TitleBar::button_layout`, Windows `on_close_window` | 11 sites (all secondary windows) |
| `Settings::{content_style, force_page, on_page_change, auto_focus_search}`, `SettingPage::custom_content`, `SettingField::password` | `settings_view.rs`, `settings_view/plugin_settings.rs` |
| `CollapsibleBreadcrumb` (+State) | `components/breadcrumb.rs` |
| `CheckboxState` re-export, `Checkbox::state` | `dialogs/change_attributes_dialog.rs` |
| `AppMenuBar::{set_selected_index, has_activated_menu}` | `main_window/titlebar.rs`, `help_window.rs`, `shell_menu_palette.rs` |
| `MenuIcon::Raster` | `services/shell_menu/submenu.rs` |
| `Notification::{autohide_after, on_expire}` | `ui/notify.rs` |
| `Kbd::arrow_glyphs` | `fkey_bar.rs` |
| `Toggle::tab_stop`, `ToggleGroup::tab_stop` | `fkey_bar.rs`, `dialogs/hex_find.rs`, `dialogs/archive_dialog.rs` |
| `set_control_scale`, `scaled_px`, `title_bar_height()`, `Size::{button_height, input_height, menu_item_height}`, `Sizable` on `PopupMenu`/`Kbd` | `ui/constants.rs` (Phase 2 of the density plan adopts the rest) |
| `TableEvent::RowClick`, `Column::{flex_grow, header_colspan}`, `TableDelegate::{visible_rows_rendered, has_context_menu}` | `file_panel.rs`, `file_table.rs`, `file_table/cells.rs` |
| `ResizablePanelGroup::snap_to`, `ResizableState::is_resizing` | `panel_splitter.rs` |
| `HoverCard::content_offset` | `file_table/cells.rs` |
| `SelectableText`, `TextSelection`, `TextSelectionHandle` re-exports | `preview_window/info_box.rs`, `hex.rs` |
| `TextView::image_source` | `help_window.rs` |
| `CommandItem::{description, key_context}`, `Command::dispatch_actions` | `command_palette.rs` |
| Behavioural: single-line Up/Down propagate; settings keyboard nav; table auto-width/colspan; resizable proportional rescale | file panels, dialogs, settings, splitter |

---

## 12. Docs and repo files

`CLAUDE.md` (MCP section — still says `crates/ui/src/mcp.rs` and `docs/docs/mcp.md`; actual paths are
`crates/component/src/mcp.rs` and `website/docs/mcp.md`), `README.md`/`README.zh-CN.md` (Agent-Inspectable bullet),
`website/docs/mcp.md` (+270), `website/zh-CN/docs/mcp.md` (+239), `website/{,zh-CN/}component/title-bar.md`.

---

## 13. Merge hotspots (`f8cd4860..upstream/main`, 233 upstream commits, 836 files, +88,482/−10,125)

Upstream commits touching the same path as the fork's net diff (descending). Upstream renamed/added nothing
on top of a fork path except **`crates/webview/src/linux.rs` (add/add)**.

| Upstream commits | File | Notes |
|---:|---|---|
| 24 | `Cargo.lock` | regenerate, do not merge by hand |
| 16 | `Cargo.toml` | gpui-pre 0.3.7→0.3.9, `gpui-fast*` optional deps, crate rename `gpui-wry`→`gpui-webview`, `component-shell` removed, versions 0.8.0, `two-face`, `notify 8.2`, profile changes (codegen-units default, `gpui-pre-macros` opt-level removed, release `strip`). **Keep fork's gpui block + `[patch]`** (§0). |
| 13 | `crates/component/src/table/state.rs` | `7bb1b9b6` perf cell rendering, `cc839912` focus ring, `636e0888` sort button, `29399be4` visible ranges, `288767cc`, `37cad320` header row height, `ce09848c`, `7f1bf0c2`, `efd2e672` resize protection, `8d4907da` fixed-cols border, `af2f4c2c` **double-click handle fits to content** (vs fork's double-click reset of auto columns), `14c26d30` bounce scroll, `e929d983`. Expect the hardest conflict. |
| 10 / 7 / 7 / 5 / 4 | `crates/webview/{Cargo.toml, lib.rs, …}`, `examples/webview/*`, `linux.rs`, `README.md` | upstream did Linux X11 itself (`56c5eb49`) + gpui-fast composition. Fork patch `effc9776` is **obsolete** → take upstream. |
| 9 | `crates/component/Cargo.toml` | keep `mcp` feature, `gpui-mcp-protocol`, `image`, `async-channel`, `futures`, `uds_windows`, `Win32_Networking_WinSock`. |
| 9 | `crates/story/Cargo.toml` | keep `mcp` feature / optional `gpui-component` dep. |
| 7 | `crates/base/src/text/inline.rs` | only the `PaintQuad` effect fields. |
| 6 | `crates/component/src/table/data_table.rs` | `37cad320` adds `DataTable::header_row_height(Pixels)` — **overlaps fork `header_size(Size)`** (`b974dcf0`); also `focus_ring(false)`, `fixed_cols_border`. |
| 5 | `crates/component/src/root.rs` | tooltip/dialog/text-selection layers; re-seat `WindowAppearance`/`RootExt`. |
| 5 | `crates/component/src/highlighter/highlighter.rs` | `2769e656`, `6b7a07c0` perf, `4f861e8a`, `42fbb97a`, `9808ec1e` diff viewer; plus later `a0230dbf` syntect backend and `8b3631f0` app-supplied highlighter in nearby code. Re-check the `styles_from_items` split still pays. |
| 5 | `crates/component/src/lib.rs` | new modules (speech, diff, resize handle, gpui-fast). |
| 5 / 5 | `README.md`, `README.zh-CN.md` | one-line fork bullet. |
| 4 | `crates/base/src/text_selection.rs` | `ab7b54fd` stop writing selection state every frame, `2c113652`, `1af2d1f9` long-press, `dca2c369` — same move handler as `fa50328c`. |
| 4 | `crates/component/src/menu/popup_menu.rs` | `cc839912`, `114d7071`, `9c4b175c`, `74939dc7`; re-check `MenuIcon` everywhere `Icon` is newly taken. |
| 4 | `crates/shell/src/runtime.rs` | effect fields. |
| 3 | `crates/base/src/resizable/{mod,panel}.rs` | `8173d61c` `ResizablePanel::fixed`, `2ef99681`, `bd8ed21f`. |
| 3 | `crates/component/src/button/button.rs` | `eb6c13ad` focus line, `c0bebdc8`, `369f7a8f`. |
| 3 | `crates/component/src/checkbox.rs` | `4708780a` text sizes, `c0bebdc8`, `e949d3e5`. |
| 3 | `crates/component/src/text/compat.rs` | `a0230dbf`, `8d8cc671`, `dca2c369`. |
| 2 | `base/scrollable_mask.rs`, `base/scrollbar.rs`, `base/switch.rs`, `color_picker.rs`, `dock/tab_panel.rs`, `setting/page.rs`, `setting/item.rs`, `table/column.rs`, `table/delegate.rs`, `story/.../data_table_story.rs` | |
| 1 | `base/popup.rs`, `base/input/base/movement.rs`, `base/.../display_map.rs`, `component-macros/crate_path.rs`, `assets/default-icons.txt`, `highlighter/input_adapter.rs`, `kbd.rs`, `menu/app_menu_bar.rs`, `menu/mod.rs`, `notification.rs`, `setting/fields/string.rs`, `sidebar/menu.rs`, `sizing.rs`, `title_bar.rs`, `window_border.rs` (`1b6059c1` no resize for non-resizable windows), `CLAUDE.md`, `examples/webview/src/main.rs` | |
| 0 | `mcp.rs`, `breadcrumb.rs`, `setting/settings.rs`, `label.rs`, `component/hover_card.rs`, `base/hover_card.rs`, `native_menu/windows.rs`, `theme/schema.rs`, `setting/fields/mod.rs`, `setting/mod.rs`, `setting/tests.rs`, `base/selectable_text.rs`, `base/reduce_motion.rs`, `dock/panel.rs`, `virtual_list.rs`, `story/src/main.rs`, `checkerboard.rs`, icons, website docs | clean |

### Possibly obsolete / overlapping with upstream now

1. **Webview Linux support** (`effc9776`, `crates/webview/src/linux.rs`, `lib.rs`): upstream `56c5eb49` (#3395) ships Linux X11 and renames the crate to `gpui-webview`; later commits require/optionalise `gpui-fast`. Elane does not use the webview. Take upstream wholesale.
2. **`DataTable::header_size(Size)`** (`b974dcf0`, 2026-09-30) vs upstream **`DataTable::header_row_height(Pixels)`** (`37cad320`, #3420, 2026-10-09) — same purpose, also fixes PageUp/PageDown by the header band. Candidate to switch Elane (`file_panel.rs`) to upstream's API and drop the fork's.
3. **Resizable**: upstream `8173d61c` `ResizablePanel::fixed` keeps a panel's size on container resize, the opposite policy to the fork's proportional rescale — both can coexist, but the conflict is in the same `adjust_to_container_size` path; `2ef99681` overlaps the fork's "no notify in steady state".
4. **Table double-click on the resize handle**: upstream `af2f4c2c` fits the column to its content; the fork resets auto-width columns on the same gesture.
5. **Button padding** differs from upstream on every platform, not only the Windows boost (§9).
6. **`language()` hunk in highlighter.rs** is a pure move; resolve to upstream's position.
7. Upstream `TableState` docs still describe cell→row escalation which the fork's net diff deletes from the `row_header` doc comment, while the code (`should_escalate_to_row`) is still there — doc drift only.

---

## 14. The 2026-10-21 upstream merge (233 upstream commits, `f8cd4860..8b3631f0`)

Order: gpui-fork first (zed `5adae449`, 2026-10-21 — past `93d7456`, the zed revision upstream's
`gpui-pre 0.3.9` snapshot was cut from), then this repo. Resolutions and what changed for the fork:

| Area | Resolution |
|---|---|
| `Cargo.toml` | Fork's gpui-by-git block kept (gpui, gpui_platform, gpui_web, gpui_macros, reqwest_client, `zed-sum-tree`, zed-reqwest). Upstream's optional `gpui-fast*` deps (crates.io) **kept**: wired through base, assets, kit, shell, story-web; Elane never enables the feature and, not being a workspace member here, never resolves them. |
| `crate_path.rs` | `"gpui"` ahead of `"gpui-fast"` (§0). |
| Webview | Upstream wholesale; fork's `effc9776` dropped (§8). `CLAUDE.md`: upstream's (the fork section's stale `crates/ui` paths went with it). |
| Table `column.rs` | Default `max_width` back to upstream's **1200 px** resize cap (#3472). The fork's `f32::MAX` was not a fork decision — it was upstream's own value from #1831, which #3472 reverted. `auto_width` / `header_colspan` kept. |
| Table resize drag | Lower bound stays the fork's `original_min_width`; `max`/`min` instead of `clamp`, so an inverted range resolves to `max_width` without panicking (#3472). |
| Table resize handle double-click | One `on_mouse_down` handler: an **auto-width** column resets to its original min width (fork); any other column fits its content (upstream #3489, `fit_col`). |
| Table header | Fork's rendering kept (colspans, grouped header, non-virtualised scroll columns); the fixed-columns edge now comes from upstream's `render_fixed_cols_border` (honours `DataTable::fixed_cols_border`). Header height reads `TableState::header_row_height()` (upstream); `header_band_height()` for the selection overlay. |
| Table body | Fork's row rendering kept; upstream's `update_visible_range_if_need(range, rows_count, ..)` signature (#3397) and the vertical `ScrollBounce` wrapper (#3500, on by default on macOS) added around it. `visible_rows_rendered` still called from the uniform_list processor. |
| Table row click | `cx.stop_propagation()` moved here from `set_selected_row` (upstream #3469); `select_on_modifier_click` honoured (#3523); fork's `last_clicked_col` / `was_already_selected` kept. |
| `root.rs` | `WindowAppearance`/`RootExt` next to upstream's `next_dialog_id`. |
| `title_bar.rs` | Fork body kept; upstream's `.test_support()` (#3383) applied to `ControlIcon` and `WindowControls` (whose empty early return now carries the control's own id, `window-controls-left/right`, like the populated one). The title-bar container itself has no `test_support()` — the fork's body never had `on_double_click` there. |
| `resizable/mod.rs` | Fork's range clamp on rescale, on upstream's `available` (container minus handles). Fork tests pass upstream's new `fixed: bool` (`false`). |
| `kbd.rs`, `menu/mod.rs`, `sidebar/menu.rs`, `setting/{item,fields/string}.rs` | Both sides (`arrow_glyphs` + `keystroke()`; `MenuIcon` + `MenuItemElement`; roving-tabindex `tab_stop(false)` + caret accessibility label; `StringField::new(title, masked)`). |
| `highlighter.rs` | Upstream's `edit_injection_layers`; its `language()` copy dropped (the fork already has it, §8). |
| `notification.rs` | Fork's `autohide_after` + upstream's `autohide_ids` (#3279, no clock while only persistent toasts rest): `let autohide = notification.autohide; let timeout = autohide.then_some(notification.autohide_after);`. |
| `mcp.rs` | `PlatformInput::Scroll(ScrollEvent { .., source: ScrollSource::Pointer })` — zed #65805 renamed `ScrollWheelEvent`/`ScrollWheel` and added `source`. |
| `scroll_bounce.rs` tests | `PlatformInput::ScrollWheel` → `Scroll` (upstream's tests were written against gpui-pre 0.3.9, before the rename). |

**Left as is, deliberately:** upstream code still names `ScrollWheelEvent` / `on_scroll_wheel`, which the
fork's gpui keeps as deprecated aliases. Renaming them here would only widen the diff; upstream follows
once its gpui-pre snapshot passes zed #65805. The warnings do not reach Elane's clippy gate (not a member).

**Button padding (§9), decided 2026-10-21:** back to upstream's horizontal paddings on every platform (Small 8 / compact 6, Medium 10 / 8, Large 12 / 8 px). Only the Windows `platform_control_*_boost` remains a fork difference in `Button`.

Verified 2026-10-21 on macOS: `cargo test -p gpui-component -p gpui-base --lib --features
gpui-component/mcp,gpui-component/test-support` — 1450 + 792 passed.
