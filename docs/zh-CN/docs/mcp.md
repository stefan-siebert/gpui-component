---
title: MCP Inspector
description: 让 AI 编程助手查看并操作正在运行的应用。
order: 0
---

启用 `mcp` feature 后，应用内部会启动一个小型服务，AI 助手（Claude Code、Claude
Desktop，以及任何支持
[Model Context Protocol](https://modelcontextprotocol.io) 的客户端）可以通过它
查看界面并进行操作：读取屏幕上的内容、点击、输入文本、发送按键、派发具名
action、截图，以及读取应用自定义的状态快照。

这样做的意义在于：助手可以像人一样通过实际使用应用来验证界面改动，而不是仅凭
代码猜测。

::: warning 仅限 fork
该功能位于
[stefan-siebert/gpui-component](https://github.com/stefan-siebert/gpui-component)
分支，并且依赖对应的 gpui fork，因为它用到了上游 gpui 尚未公开的 inspector API。
:::

## 整体结构

```
 AI 助手
     │  基于 stdio 的 MCP 协议
     ▼
 gpui-mcp-server                 ← 独立的二进制程序，每台机器一个
     │  每个连接一条 JSON 请求，走 Unix domain socket
     │  {temp_dir}/gpui-mcp-{app}-{pid}.sock
     ▼
 你的应用
   gpui_component::mcp           ← 这个 feature
```

服务端本身不保存状态，每次调用都会重新查找 socket，因此应用可以随意重启，助手的
会话不需要跟着重启。

## 启用方式

请把它放在你自己的 feature 后面，避免发布版本中带上 IPC 服务：

```toml
[features]
mcp = ["gpui-component/mcp"]

[dependencies]
gpui = { git = "https://github.com/stefan-siebert/zed", branch = "gpui-mcp-patches-v2" }
gpui-component = { git = "https://github.com/stefan-siebert/gpui-component", branch = "main" }
```

gpui-component 使用的是同一个 gpui git 源，因此两者会解析成**同一个** gpui。如果
出现第二份拷贝（例如同时存在 path 依赖，或从 crates.io 引入上游 gpui），所有类型
都会对不上。

然后在 `gpui_component::init` 之后启动一次：

```rs
app.run(|cx| {
    gpui_component::init(cx);

    #[cfg(feature = "mcp")]
    {
        // 这个名字用于让服务端在多个应用中找到你的应用。
        gpui_component::mcp::init_mcp(cx, "my-app");

        // 可选：决定 `get_app_state` 返回什么。
        gpui_component::mcp::mcp_set_app_state_provider(|cx| {
            serde_json::json!({ "rows": 0, "selected": null })
        });

        // 可选：定义什么是「已知的初始状态」，供 `reset_app` 使用。
        // 这样录制好的脚本就能从一个确定的状态开始，而不是从上一次
        // 会话留下的任何状态开始。
        gpui_component::mcp::mcp_set_reset_hook(|_arguments, cx| {
            // 关闭文档、清除选择、回到第一个标签页
            Ok(())
        });
    }

    // ... windows, views ...
});
```

`init_mcp` 会绑定 `{temp_dir}/gpui-mcp-my-app-{pid}.sock` 并启动监听线程。请求都在
GPUI 主线程上处理，因此能看到一致的状态，也能派发真实输入。
`gpui_component::mcp::mcp_log(..)` 会写入一个 500 条上限的日志缓冲，助手可以读取。

开发时启用该 feature，发布时关闭。服务端二进制的构建方式以及如何注册到助手，见
[gpui-mcp README](https://github.com/stefan-siebert/gpui-mcp)。

## 助手能做什么

| 工具 | 作用 |
|---|---|
| `gpui_guide` | 服务端自带的说明文档，让助手一开始就知道怎么用 |
| `ui_snapshot` | 把窗口渲染成一行一个有意义元素的简短列表 |
| `a11y_audit` | 无法命名的控件、指向多个元素的 id、小于 24px 的点击目标 |
| `a11y_tree` | AccessKit 树：真实的角色、朗读出的标签、输入框的值、节点支持的操作 —— 仅包含已标注的元素 |
| `inspect_ui_tree` | 完整的元素树，适合排查布局问题 |
| `get_element` | 单个元素及其子树 |
| `get_windows`、`get_app_state` | 窗口列表，以及状态提供者返回的快照 |
| `get_focus_info` | 焦点句柄和当前生效的 key context |
| `list_actions`、`execute_action` | 应用的 GPUI action 列表与派发 |
| `send_key`、`type_text`、`click_element` | 经过焦点链的真实输入 |
| `wait_for` | 在应用内部按帧等待，直到条件满足 |
| `batch` | 把上面多个调用合成一个请求 |
| `take_screenshot` | 由 GPUI 自己渲染窗口或单个元素 |
| `get_logs` | `mcp_log` 缓冲区 |
| `set_viewport` | 把窗口调整到确定的内容尺寸，使布局可复现 |
| `reset_app` | 通过 `mcp_set_reset_hook` 把应用恢复到已知的初始状态 |
| `replay_script` | 回放录制好的会话 —— 既可用于恢复到某个状态，也可作为测试 |

## 助手看到的内容

`ui_snapshot` 是查看窗口最省成本的方式：

```
ui_snapshot 4 — WindowId(1v1), 9 of 96 painted elements
- button #github @e1
- menu #gallery-sidebar @e2
  - listitem "Accordion" #item @e3
- textbox #search @e4
```

一个元素能占据一行，是因为它具备**角色**、**开发者写的 id**，或者**文本**。其余的
都是布局脚手架：它们会被去掉，子元素顶上来。对真实窗口来说，这比完整元素树小得
多——而助手读到的内容是要付费的。

### 角色从哪里来

来自渲染该元素的源文件。gpui-component 一个文件一个组件，所以
`button/button.rs` 渲染出的是 `button`，`list/list_item.rs` 渲染出的是
`listitem`。你的应用不需要任何标注就能得到这套语义词汇。

有两条规则保证这种推导不会说错话：

- 表示**区域**的角色（`banner`、`list`、`dialog` 等）只在元素确实包含内容时才使
  用。同一个文件既画标题栏也画它的关闭按钮，把那个按钮称作 banner 比什么都不说
  更糟。
- **你自己写的组件目前没有角色**，它们会以 id 和文本的形式出现，通常这已经足够
  定位和点击了。

### 给元素起个好名字

当 id 片段看起来像人写的（小写、带连字符或下划线）时，快照会把它打印成 `#name`。
自动生成的片段（`view-4294967734`、`1-0-0`、类型名）会被忽略，因为它们会变。

```rs
div().id("results").child(..)     // 显示为 #results
```

所以，为了让助手更好地操作你的应用，最有价值的一件事就是给需要被操作的元素显式
指定稳定的 id。id 的组合方式见 [ElementId](./element_id)。

每行末尾的 `@e3` 是一个 **ref**，意思是"我刚给你看的那份快照里的那一行"。它可以
用在任何接受元素 id 的地方。每次快照都会替换掉整组 ref，因此旧的 ref 会明确报错
提示重新获取快照，而不会解析到现在恰好排在那一行的元素上。

## 检查无障碍

`a11y_audit` 报告推导层能够看到的问题，而这些问题恰好同时困扰屏幕阅读器用户和
AI 助手：

| 检查项 | 严重程度 | 含义 |
|---|---|---|
| `unnamed-control` | serious | 交互元素没有绘制任何文本 —— 无法朗读，也无法按名称定位 |
| `duplicate-id` | 控件为 serious，其他为 warning | 一个 id 指向多个元素；后缀匹配只会取第一个 |
| `target-too-small` | warning | 边长小于 24px（WCAG 2.2 的最小值）|
| `zero-size-control` | serious | 交互元素完全没有面积 |
| `unstable-id` | warning | id 以每次启动都会重新生成的数字结尾，例如 `#input-4294967299` —— 看起来像名字，其实不是 |

首次运行时最常见的结果，是纯图标按钮以及列表行共用同一个 id —— 任何应用都是如此，
这只是「还没有人有理由为它们命名」的正常状态。

修复方式对两类使用者同时有效：给元素加上标签和独立的 id，它就既可被朗读也可被
定位。对比度无法检查，因为颜色不会传到 MCP 这一侧。

把审计写进录制好的脚本，审计失败就会让回放失败，从而保持长期有效：

```json
{ "method": "a11y_audit", "params": { "fail_on": "serious" } }
```

## 真正的无障碍树

审计读的是推导层。GPUI 同时会构建真正的 AccessKit 树——也就是交给屏幕阅读器的那
一棵——`a11y_tree` 返回的正是它：真实的角色（`Button`、`MenuBar`、`TextInput`）、
控件即使只绘制了图标也会朗读出的标签、输入框当前的值，以及每个节点支持的操作。

它比窗口稀疏得多，因为只有被标注过的元素才会生成节点。在本仓库自带的画廊里，是
96 个已绘制元素对应 11 个节点；返回结果会同时给出这两个数字，以免这棵树被当成整个
界面。

多数时候并不需要这个工具本身。`ui_snapshot` 已经把这棵树折进了它的每一行：元素有
节点时，声明的角色优先于从源文件推导出来的角色，标签会在没有绘制文本时补上名字，
状态会附在后面（`checked`、`selected`、`value="…"`），行尾以 `✓` 结束——这样声明
出来的事实不会被误当成推导出来的。审计用的是同一套叠加：它在 `checked` 旁边报告
`announced`，并且 `unnamed-control` 现在会区分「缺标签」和「先缺角色」。

对应关系是精确的，而不是近似的：gpui 用与检查器相同的 `GlobalElementId` 推导节点
id，而 `InspectorElementInfo` 会把它带上。仅凭节点自带的末段 element id 和源码位置
是不够的——本仓库标题栏上的四个按钮这两项完全相同。

GPUI 只在辅助技术连接时才构建这棵树。读取它会让窗口开始构建并等待一帧，因此对某个
窗口的第一次 `ui_snapshot`、`a11y_audit` 或 `a11y_tree` 都会多花一帧。这需要本
crate 所依赖的补丁版 gpui 中的 `Window::set_a11y_force_active`。

目前已自行标注的组件是 Button、ToggleButton、Checkbox、Radio、Switch、Tab、List、
MenuItem 和 PopupMenu。应用中的其他元素需要先加上 `.role(...)` 和
`.aria_label(...)` 才会出现。

## 返回结果的含义

助手读到的一切——元素树、快照、截图——都来自**最后一次绘制的帧**。这会导致点击的
返回结果描述的是点击*之前*的界面，因此输入类工具会等到展示其效果的那一帧绘制完成
后才返回，随附的应用状态和焦点信息也来自那一帧。

- `settled: true`：那一帧已经到达。
- `settled: false`：等待期间没有任何帧被绘制。窗口被最小化、被遮挡，或所在平台
  不再绘制不可见窗口，此时返回内容描述的是更早的一帧。

应用自己发起的工作——异步加载、防抖、动画——不在此列，这正是 `wait_for` 的用途：
它在应用内部按帧检查，直到某个元素、某段文本、某个 key context 或应用状态中的某个
值满足条件。

## 开销

`mcp` feature 会启用 gpui 的 `inspector`，它在每一帧记录 hitbox 和绘制的文本。这
是开发期的开销，也正是它应该放在 feature flag 后面的原因。

空闲时 MCP 服务不做任何事：请求到达时由监听线程唤醒主线程，而不是主线程轮询。

## 安全

任何能访问该 socket 的程序都能完全控制界面并读取其状态。它是开发工具：请放在
feature flag 后面，永远不要在发布版本中启用；socket 位于当前用户的临时目录中，
除此之外没有任何认证。
