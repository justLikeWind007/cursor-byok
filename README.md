# Cursor Rust 服务端实施与验收计划

## 项目说明

`cursor-byok` 是一个兼容 Cursor Agent 客户端协议的自托管服务端项目，用于把 Cursor 客户端接入用户指定的 LLM Provider。项目根据真实客户端流量和提取出的 protobuf 协议实现，不依赖 Cursor 原服务保存对话状态。

当前 Rust 服务 `cursor-server` 实现以下完整链路：

- 通过 `RunSSE + BidiAppend` 组成的双向协议与 Cursor 客户端通信。
- 将 OpenAI Chat、OpenAI Responses 和 Anthropic 的流式响应统一为内部 `ResponseEvent`。
- 运行无状态 LLM Loop：`LLM → 客户端工具执行 → 结果追加 → 下一轮 LLM`，直到 Turn 完成或被新 Run 打断。
- 以 append-only messages 作为上下文唯一事实源，保证相邻 LLM 请求的稳定前缀和可重复投射。
- 支持文本、thinking、tool start、参数增量、tool result、usage、model、rules、commands、skills、MCP 和 subagent 上下文。
- 使用 SQLite 持久化 messages、Run 状态、Blob CAS、引用边和 outbox。
- 使用不可变 Blob 对象图表达 Conversation、Turn、UserMessage 和 Steps；BlobID 为原始内容的 `SHA-256`。
- 在客户端确认 Blob 已存储后发布 checkpoint，并提供单工具粒度的历史回滚和未确认操作恢复。

运行时职责划分如下：Cursor 客户端负责真正执行本地工具并保存服务端同步的 Blob；`cursor-server` 负责 Loop 决策、上下文投射、Provider 调用、状态持久化和 checkpoint 构造。Todo/Plan 等业务状态不单独维护，而是从 messages 确定性推导。

协议与状态模型的抓包结论见 [Cursor上下文与状态同步抓包分析.md](./Cursor上下文与状态同步抓包分析.md)。Rust 服务的启动方式和运行配置见 [cursor-server/README.md](./cursor-server/README.md)。

## 目录硬约束

下列目录、文件名和职责是实现验收条件，不是建议。首版只允许一个 `cursor-server` crate；代码必须落在对应文件，不得用 `core.rs`、`service.rs` 等总入口替代，也不得提前创建 MCP/subagent 空模块。新增文件必须说明为何现有职责无法容纳；删除、改名或移动下列文件必须先同步修改本计划。

```text
cursor-byok/
├── cursor-server/                 # 新 Rust 服务
│   ├── Cargo.toml
│   ├── build.rs                   # 从 cursor-proto/proto 生成 prost 类型
│   ├── README.md                  # 启动方式、架构和核心不变量
│   │
│   ├── migrations/
│   │   └── 0001_initial.sql       # Blob、messages、runs、outbox
│   │
│   ├── src/
│   │   ├── main.rs                # 进程入口
│   │   ├── lib.rs                 # 模块出口
│   │   ├── app.rs                 # 依赖组装、启动和关闭
│   │   ├── config.rs              # 地址、数据库、provider 配置
│   │   ├── error.rs               # 服务统一错误
│   │   │
│   │   ├── model/                 # 纯领域类型，不依赖 Cursor/provider
│   │   │   ├── mod.rs
│   │   │   ├── message.rs         # CanonicalMessage、Role、Origin
│   │   │   ├── runtime_tag.rs     # RuntimeEvent、exactly-once 约束
│   │   │   ├── conversation.rs    # Conversation、Turn、revision
│   │   │   ├── tool.rs            # ToolCall、ToolResult
│   │   │   └── usage.rs           # provider usage 与 Turn usage
│   │   │
│   │   ├── run/                   # Loop 引擎和一次 request 的状态机
│   │   │   ├── mod.rs
│   │   │   ├── registry.rs        # request_id → RunHandle
│   │   │   ├── actor.rs           # 每个 Run 一个 actor
│   │   │   ├── command.rs         # run_request、exec/KV result、abort
│   │   │   ├── inbox.rs           # append_seqno 排序、去重
│   │   │   ├── loop_engine.rs     # LLM → Tool → LLM 主循环
│   │   │   └── lifecycle.rs       # turn_ended/checkpoint/EndStream
│   │   │
│   │   ├── cursor/                # Cursor 协议适配器
│   │   │   ├── mod.rs
│   │   │   ├── proto.rs           # include prost 生成代码
│   │   │   ├── connect.rs         # 5-byte Connect envelope
│   │   │   ├── handlers.rs        # Axum 路由入口
│   │   │   ├── bidi_append.rs     # 上行 AgentClientMessage
│   │   │   ├── run_sse.rs         # 下行 AgentServerMessage
│   │   │   ├── interaction.rs     # 交互事件、Tool args 和 usage 投射
│   │   │   ├── exec.rs            # Exec 上行/下行解析
│   │   │   ├── pending.rs         # Exec/Interaction 的运行期 ID 关联
│   │   │   ├── tools.rs           # 唯一工具路由、本地工具和 Cursor step index
│   │   │   ├── tool_result.rs     # typed result、UI completion 和结果通道
│   │   │   ├── blob_sync.rs       # KV GET/SET、ACK、重试
│   │   │   └── checkpoint.rs      # Blob 图和 checkpoint 构造
│   │   │
│   │   ├── provider/              # LLM 端点适配器
│   │   │   ├── mod.rs             # Provider trait
│   │   │   ├── event.rs           # Canonical ResponseEvent
│   │   │   ├── openai_chat.rs     # 第一条可运行链路
│   │   │   ├── openai_responses.rs
│   │   │   └── anthropic.rs
│   │   │
│   │   ├── prompting/             # 模型请求编译
│   │   │   ├── mod.rs
│   │   │   ├── assets.rs          # 校验并嵌入根目录 prompt/
│   │   │   ├── compiler.rs        # messages + mode + tools
│   │   │   ├── projector.rs       # CanonicalMessage → provider 格式
│   │   │   └── derived_state.rs   # 从 messages fold Todo/Plan
│   │   │
│   │   └── store/                 # SQLite 持久化
│   │       ├── mod.rs
│   │       ├── sqlite.rs           # pool、事务、PRAGMA
│   │       ├── messages.rs         # append-only messages
│   │       ├── blobs.rs            # CAS 与引用边
│   │       ├── conversations.rs    # conversation head/revision
│   │       ├── runs.rs             # 活动 Run 和恢复信息
│   │       └── outbox.rs           # KV/checkpoint 待确认操作
│   │
│   └── tests/
│       ├── support/
│       │   ├── fake_provider.rs
│       │   ├── fake_cursor.rs
│       │   └── fixtures.rs
│       ├── text_turn.rs            # 纯文本完整 Turn
│       ├── tool_loop.rs            # LLM → Tool → LLM
│       ├── runtime_tag_once.rs      # Runtime tag 不重复追加
│       ├── prefix_stability.rs      # M(n) 是 M(n+1) 前缀
│       ├── checkpoint_recovery.rs   # 单 Tool 回滚
│       ├── interrupt.rs             # 新 Run 打断旧 Run
│       └── connect_wire.rs          # Connect 二进制兼容性
│
├── cursor-proto/                   # 现有 protobuf 提取和源文件
├── cursor-backend/                 # 现有 Go 抓包调试器
├── prompt/                         # 已复制的完整模式资产
└── docs/
```

## 按文件实施顺序与通过条件

1. `Cargo.toml`、`build.rs`、`src/{main,lib,app,config,error}.rs`：服务能加载配置、迁移数据库、生成 Cursor protobuf 并启动/优雅关闭。
2. `src/model/*.rs`、`src/store/*.rs`、`migrations/0001_initial.sql`：实现纯领域消息、runtime tag、tool/usage，以及 append-only messages、Blob CAS/引用边、revision、run 恢复与 outbox；`tests/runtime_tag_once.rs` 和 `tests/prefix_stability.rs` 必须通过。
3. `src/cursor/{proto,connect,handlers,bidi_append,run_sse}.rs`：实现抓包一致的 5-byte Connect envelope、二进制 RunSSE、BidiAppend 解码和 `append_seqno` 排序去重；`tests/connect_wire.rs` 必须通过。
4. `src/provider/{event,openai_chat,openai_responses,anthropic}.rs`、`src/prompting/*.rs`：三个端点统一为 canonical `ResponseEvent`；所有 mode 的 prompt/tool 资产可加载；messages 投射幂等且保持严格前缀。
5. `src/run/*.rs`、`src/cursor/{tools,interaction,exec,pending,tool_result}.rs`：一个 request 一个 RunActor；Loop 只处理统一 `ToolCompletion`，工具名称到 Exec/Interaction/Local 的唯一映射只存在于 `cursor/tools.rs`；完成 toolstart 占位、参数增量、客户端执行、打断与 usage。每个完成的工具必须原子追加 `assistant(tool_call) → tool(result)`，整批完成前不得进入下一次 LLM。
6. `src/cursor/{blob_sync,checkpoint}.rs`、`src/store/{blobs,outbox}.rs`：构造不可变 Blob 对象图，KV SET 未 ACK 前不得发布引用它的 checkpoint；checkpoint 达到单工具粒度，最终状态重复发布后再 EndStream；`tests/checkpoint_recovery.rs` 必须通过。
7. `tests/{text_turn,tool_loop,interrupt}.rs` 与 `tests/support/*.rs`：覆盖纯文本 Turn、完整工具循环、新 Run 打断旧 Run和恢复路径。最终验收命令固定为 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`。


模块依赖方向固定为：

HTTP/Connect
    ↓
cursor adapter
    ↓
run actor
    ↓
model + prompting
    ↓
provider / client tools
    ↓
store + checkpoint

几个关键决定：
model/ 不引用 Cursor protobuf，也不引用具体 provider。
cursor/ 只负责协议转换，不能包含 Loop 业务决策。
provider/ 只把不同端点转换为统一 ResponseEvent。
prompting/derived_state.rs 只 fold messages，不持久化 Todo/Plan。
store/messages.rs 是上下文唯一事实源。
store/outbox.rs 保存尚未确认的 Blob/checkpoint 操作。
MCP 和 subagent 暂时不建空目录：MCP 先作为动态 Tool 接入；subagent 复用 RunActor，需求落地时再加入 run/subagent.rs。

依赖建议：
```
tokio                异步运行时
axum + hyper         Connect HTTP 服务
prost + prost-build  protobuf
protoc-bin-vendored  避免系统 protoc 依赖
sqlx/sqlite          持久化和事务
reqwest              provider HTTP
eventsource-stream   provider SSE
serde/serde_json     模型和工具 JSON
sha2 + base64        BlobID
bytes                二进制载荷
tokio-util           CancellationToken
thiserror + tracing  错误和日志
include_dir          编译期嵌入 prompt/ 资产
```

Cursor上下文与状态同步抓包分析.md 来告诉你很多信息，你需要一次性读他

Users/leokun/Library/Application Support/cursor-byok/cursor-proxy-debugger.db 是cursor的原服务抓包信息，内容由/Users/leokun/Documents/cursor-byok/cursor-backend产生
