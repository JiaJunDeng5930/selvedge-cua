# Selvedge CUA MCP

## 项目做什么

Selvedge 是面向模型的 macOS 计算机使用服务器。它通过 stdio 暴露一个 MCP 工具 `run({code})`，在内嵌的 QuickJS 中运行一段异步 JavaScript。脚本可以查找窗口、读取无障碍树、截图、发送输入，以及监听指定进程的播放音频并读取转写。工具返回 JSON 和截图附件。脚本接口以 [run-api.md](crates/script-server/src/run-api.md) 为准，安装和打包见 [README.md](README.md)。

项目要让模型在与用户共用桌面时，尽量针对后台窗口完成任务，并凭观察判断应用是否产生了预期结果。后台能力取决于 macOS 的路由和目标应用。输入回执记录分发情况，应用是否响应由后续观察确认。

## 代码结构

| 位置 | 职责 |
| --- | --- |
| [src/](src/) | 可执行程序和装配层。`main.rs` 启动服务器并驱动 macOS 主运行循环。`desktop.rs` 连接脚本操作与原生适配器，管理会话时钟和调用收尾。`input.rs` 管理输入计划、分发顺序和按住状态。`locked_use.rs` 管理与外部授权代理的会话。 |
| [crates/script-server/](crates/script-server/) | MCP stdio、QuickJS 绑定、单次 `run` 的调度和取消、结果与附件封装。 |
| [crates/rules/](crates/rules/) | `formal/` 保存工程需求、可执行规则和证明。`src/` 把提取出的定义接到 Rust 的类型和调用路径。 |
| [crates/macos-ax/](crates/macos-ax/) 和 [crates/macos-desktop/](crates/macos-desktop/) | 前者负责 Accessibility 树、窗口匹配、元素身份和操作。后者负责应用发现、启动和剪贴板。 |
| [crates/macos-input/](crates/macos-input/)、[crates/macos-screen/](crates/macos-screen/) 和 [crates/macos-audio/](crates/macos-audio/) | 分别负责原生事件分发、截图与画面观察、按进程采集音频并交给本地转写后端。 |
| [platform/](platform/) | macOS 应用打包、Codex 插件打包、Apple 语音转写辅助程序和 Locked Use 桥接。Protected 会话的授权与恢复约定见 [Locked Use 文档](platform/locked-use/README.md)。 |
| [tests/](tests/) 和 [docs/adr/](docs/adr/) | 前者从 MCP stdio 入口验收协议和原生行为。后者记录窗口发现、截图和桌面工具的设计决策。 |

## 工程方法

1. **从用户可观察的结果推导要求。** 先明确脚本要区分哪些结果、什么证据足以判断成功、哪些外部条件由系统或应用提供。以 [Requirement.v](crates/rules/formal/Requirement.v) 和对应功能的规则模块为入口。涉及失败、取消、权限变化或并发时，把这些情况纳入同一行为契约。
2. **让 Rocq 规则进入实际执行。** 变更准入、状态转移、顺序、结果选择或结束条件时，先核对已有模型，在所属 Rocq 模块中更新可执行规则和关键证明。将运行所需的函数列入 [ExtractRules.v](crates/rules/formal/ExtractRules.v)，将对外保证依赖的定理列入 [ProofAssumptions.v](crates/rules/formal/ProofAssumptions.v)，再让实际 Rust 调用路径使用提取结果。Cargo 构建会检查 Rocq 对象、审计证明假设并生成绑定。修改规则时从 Rocq 源文件重新生成。
3. **用原生适配器落实外部边界。** Rust 和平台代码负责取得系统事实、管理资源并执行副作用，例如读取单调时钟、核验窗口和元素身份、分发输入、停止采集和等待工作结束。macOS 的路由、采集来源、响应时序和转写输出依赖外部实现，用端到端验收检查。只涉及原生接线且行为契约未变时，复用已有模型，并说明依赖的外部条件。
4. **沿公开入口验证。** 新增或修改脚本操作时，更新 [run-api.md](crates/script-server/src/run-api.md)，从 MCP 客户端跑到实际适配器。证明检查、Rust 构建、模拟协议场景和真实桌面场景各自提供不同的证据。交付时说明实际跑过的范围。

音频是一个例子。[AudioRules.v](crates/rules/formal/AudioRules.v) 定义监听准入、停止后排尽尾段和分批交付的规则。Rust 调用提取结果，Core Audio 和转写后端提供有序的外部事件，通过实际播放样本检查整条链路。

## 验证入口

本地构建需要 macOS、Rust、Xcode 工具链、CMake、libclang 和与 Rocq2Rust 配套的 Rocq 9.2。默认使用相邻的 `rocq2rust` checkout 中已构建的提取插件。使用其他位置时设置 `ROCQ2RUST_PLUGIN`，细节见 [README.md](README.md#build-and-run)。

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --locked
python3 tests/acceptance.py target/debug/selvedge-cua-mcp
python3 tests/locked_use_acceptance.py target/debug/selvedge-cua-mcp
```

- 桌面和多客户端截图验收用已授权的 macOS 桌面和打包后的可执行文件，按需运行 `tests/acceptance.py` 的 `--desktop` 和 `--concurrent-capture`。
- 音频验收用 `--audio-sample` 和已知语音，按需用 `--audio-backend` 指定要验证的后端。检查转写的交付、时间戳和停止时的收尾。
- `tests/locked_use_acceptance.py` 用模拟辅助程序检查授权协议。实际签名、受保护会话、本地接管和重新上锁在已配置的测试 Mac 上验收。
