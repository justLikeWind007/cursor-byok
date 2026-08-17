# cursor-byok

`cursor-byok` 是一个基于真实 Cursor Agent 流量与 protobuf 实现的自托管服务端，用于把 Cursor 客户端接入用户指定的 LLM Provider。

当前 Rust 服务 `cursor-server` 已实现：

- Cursor `RunSSE + BidiAppend` 双向协议与 Connect envelope。
- OpenAI Chat、OpenAI Responses、Anthropic 三种无状态流式端点。
- `LLM → 客户端工具 → 结果提交 → 下一轮 LLM` 的通用 Loop。
- append-only canonical messages、不可变 revision 分支和同对话 Run 抢占。
- Cursor typed tool UI、Exec/Interaction、多阶段编辑、MCP 与子代理。
- Blob CAS、KV GET/SET ACK、两阶段 checkpoint 和 pending ToolRound 恢复。
- 未匹配 Cursor backend 路由原样流式转发到上游。
- React + Vite + TypeScript + Tailwind 管理台，支持 Provider 配置、模型发现和调用明细。
- 每次 Provider 调用的时间、模型快照、状态和 authoritative usage；详细模式保存脱敏请求与原始流响应。

启动方式见 [cursor-server/README.md](./cursor-server/README.md)。协议证据见 [Cursor上下文与状态同步抓包分析.md](./Cursor上下文与状态同步抓包分析.md)，当前实现约束见 [一次性重构计划计划.md](./docs/一次性重构计划计划.md)。

## 核心数据流

```text
HTTP / Connect
    ↓
Cursor adapter
    ↓ ClientCommand / ClientEvent
RunEngine
    ↓
canonical messages + selected revision
    ↓
typed ModelRequest
    ↓
Provider adapter → HTTP/SSE → ModelEvent
```

Loop 不依赖 Cursor protobuf、Blob、checkpoint、数字 wire id 或具体 Provider JSON。Cursor adapter 和 Provider adapter 只在各自边界做协议投射。

## 状态与 checkpoint

- SQLite 中的 immutable messages 与 revision 父链是对话事实源；回滚只选择旧 revision 并建立新分支。
- ToolRound 保存完整 assistant、原始 call 顺序和真实 result 完成顺序；结果未齐时不会把悬空 tool call 投给下一轮模型。
- BlobID 是 `SHA-256(data)`；Blob 类型来自引用字段，不编码在 ID 中。
- checkpoint 引用的新 Blob 必须先收到对应 KV SET ACK。协议中不存在 checkpoint ACK，也不保存跨流 outbox。
- staged checkpoint 内联完整 pending assistant；ToolRound 全部结果提交后才折叠进 stable roots。抓包没有单 ToolResult checkpoint，因此实现也不制造该状态。
- settled checkpoint 必须先于下一轮 LLM 调用。最终文本轮严格发送 `turn_ended → staged → settled → settled 重发 → EndStream`。
- Cursor.app 只把 `turn_ended` 前的 checkpoint 作为自动恢复候选；恢复 pending assistant 时先继续工具，不重复调用 LLM。

## 目录边界

```text
cursor-server/src/
├── control/                # 客户端无关的 Provider、模型、调用观测 HTTP API
├── client/                 # 所有客户端共用的最小 command/event port
├── model/                  # canonical message、revision、ModelSpec、typed history
├── run/                    # 协议无关 Loop、ModelCycle、ToolRound 和 RunRegistry
├── provider/               # 各 LLM 端点请求与流事件适配
├── cursor/
│   ├── request/            # RunRequest → PreparedRun/CursorRunContext
│   ├── prompting/          # Cursor prompt、工具 catalog 和 mode manifest
│   ├── projection/         # Cursor AI-SDK stable/pending JSON 编解码
│   ├── interaction/        # UI 更新、InteractionQuery、typed ToolCall 渲染
│   ├── tools/              # Cursor 工具 transport、runtime、dispatch 和 result
│   └── checkpoint/         # root/Turn/derived/recovery 与串行 worker
└── store/                  # SQLite messages、revision、ToolRound、Run 和 Blob CAS

console/                    # React 管理台；只依赖 control API，不依赖 Cursor protobuf
```

详细到文件的目标目录和验收项只在重构计划中维护，README 不复制第二份易漂移的完整文件清单。

## 工程原则

- 不保留旧路径、兼容层或失败后的隐式 fallback。
- 同一状态只有一个所有者；协议层不做 Loop 决策。
- PromptSpec、ModelSpec 和 selected revision 决定可重放的 ModelRequest；request id、时间和 model call id 不进入模型输入。
- 前缀稳定限定在相同 PromptSpec/ModelSpec/Provider route；新 Run 切换模型或模式时只替换 Cursor system root，其他历史 message roots 继续复用。
- Provider replay state 只回传给产生它的端点；可展示 thinking 不是跨端点 reasoning 字段。
- Provider usage 只采用端点报告的单轮最终值，不自行估算。
- 同一个取消信号覆盖等待 HTTP 响应头和读取 SSE 两段。

## 验证

```bash
cd cursor-server
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```
