# cursor-server

Cursor Agent 的 Rust 服务端。它实现 `RunSSE + BidiAppend` 通信、无状态 LLM loop、客户端工具执行、Blob/KV 同步和可恢复 checkpoint。服务只接管已经实现的 Cursor 接口；其他 backend 请求原样流式转发到固定上游 `https://api2.cursor.sh`。

## 启动

首次运行需要安装 Rust stable 工具链。macOS 使用 Homebrew：

```bash
brew install rustup
export PATH="$(brew --prefix rustup)/bin:$PATH"
rustup default stable
cargo --version
```

`rustup` 是 keg-only；若要让后续 zsh 会话也能找到 `cargo`，将下面一行加入 `~/.zshrc`，然后重新打开终端：

```bash
export PATH="$(brew --prefix rustup)/bin:$HOME/.cargo/bin:$PATH"
```

先构建管理台：

```bash
cd console
npm install
npm run build
```

进入 `cursor-server` 后启动：

```bash
cd ../cursor-server
CURSOR_DATABASE_URL=sqlite://cursor-server.db \
cargo run
```

默认监听 `127.0.0.1:3000`。打开 `http://127.0.0.1:3000/console/` 配置 Provider、拉取并启用模型。Provider URL、API Key 和模型不再从环境变量隐式覆盖；SQLite 是唯一运行时配置源。Anthropic 模型必须在模型配置中设置最大输出 token，服务不会猜测默认值。完整启动环境变量见 `src/config.rs`。

## 不变量

- 不可变 messages 与 revision 父链共同构成上下文事实源；消息只追加，不原地修改，回滚只选择旧 revision 并建立新分支。
- 每个携带用户语义的 RunRequest 生成一条 user-role/runtime-origin message；当前请求有什么上下文就加入什么，没有则省略。它使用稳定事件 ID，事务内 exactly-once 追加。
- 同一 PromptSpec/ModelSpec/Provider route 内，每轮投射结果可复现，后一轮 messages 严格以前一轮为前缀；新 Run 切换模型或模式时只替换 Cursor system root。
- canonical tool pairs 的 typed history 折叠属于 `model/projection.rs`；Cursor checkpoint 与各 Provider 都依赖它，彼此不反向依赖。
- 工具每完成一个，就按实际完成顺序原子追加一组 `assistant(tool_call) → tool(result)`；投射给 LLM 的上下文没有悬空 tool call，整批完整后才继续调用 LLM。
- Blob 是 `SHA-256(data)` 的不可变 CAS；Blob 类型来自引用字段，不编码在 BlobID 中。
- 引用 Blob 的 checkpoint 只有在全部新 Blob 得到 KV SET ACK 后才能发布。
- 每个新 Blob 只对应一次 KV SET 和一个配对 ACK；拒绝、超时或同步 worker 失败直接结束当前 Cursor Run，不定时制造新 id 重试。
- checkpoint 以完整 assistant 为 staged/settled 边界：工具批次开始时 stable roots 不变、`pending_tool_calls` 内联一条完整 assistant JSON；全部工具结果提交后才进入 stable roots。最终文本 assistant 同样走 staged/settled，并在 `turn_ended` 后重发同一 settled checkpoint；staged 与 settled 复用同一个已确认 Turn，presentation delta 不得消费两次。
- Provider 未报告 usage 时不伪造零值；`TurnEndedUpdate` 的 token 字段保持缺省。
- 每次真实 Provider 请求对应一条 `llm_calls`；时间使用 UTC 时间点与单调时钟耗时，usage 只保存 Provider 报告值。详细模式额外保存脱敏后的最终请求和原始 SSE 字节块。
- 用户模型公开 ID 是规范化 `URL + NUL + provider type + NUL + modelId` 的 SHA-256 前 4 bytes，表示为 8 位小写 hex；API Key 和 displayName 不参与身份。
- `AvailableModels` 与 `GetUsableModels` 在官方响应原始 protobuf 后追加用户模型字段，不解码重编码未知字段；`requested_model.model_id` 使用公开 ID 解析 Provider 路由。
- 新 Run 通过 conversation revision 使旧 Run 的迟到事件失效。
- 每种工具只对应一个 Exec、Interaction 或 Local 通道；不存在级联 fallback。
- Loop 不保存工具名称路由；`cursor/tools/dispatch/` 是 transport dispatcher，`cursor/tools/runtime.rs` 唯一拥有当前 Cursor Run 的 Exec/Interaction wire-id 和 terminal tombstone。
- Interaction approval 不是 ToolResult；只有 typed terminal result 才能进入持久化与 checkpoint。
- Exec 与 Interaction 共用当前 Run 唯一、单调且不复用的 wire-id 空间；typed terminal result 一次消费，大 payload 在核心 commit 后释放，完成墓碑保留到 ToolRound settled。
- prompt 资产编译进二进制并在启动时整体校验，不与运行时目录逐文件混用。
- `prompt/cursor/tools.json` 是 Cursor 工具 schema 唯一事实源，`prompt/cursor/modes/*.json` 只定义有序工具名或明确 variant。每个模式显式维护 `{prompt.md,runtime.md}`，不使用别名或缺失资产 fallback。
- 当前 `UserMessage.mode` 同时选择 system prompt、runtime 模板和工具集；子代理由 `subagent_type_name` 明确选择 subagent 资产，使用无 Cloud 字段的 Task variant 并增加 `UpdateCurrentStep`。
- `GetMcpTools` 必须等待客户端 `McpStateExecResult` 的实时 MCP 状态，不能从初始 descriptor 快照本地完成。
- 本地精确路由优先；未匹配的 method、path/query、headers 和 body 流式转发到固定 Cursor 上游，上游 status、headers 和 body 流式返回。
- 反向代理只改写目标 authority，并剥离不能逐跳转发的 hop-by-hop headers；不存在的本地路由不能直接返回 404。
- `cursor/checkpoint/worker.rs` 独占可推进的 checkpoint builder；`cursor/session.rs` 只提交 staged/settled/final job 并等待相应 barrier。
- `cursor/projection/`、`cursor/interaction/` 和 `cursor/tools/codec/` 分别按 JSON 编解码、UI 消息方向和 Exec wire 方向组织，不共享运行期状态。
- Provider 的取消同时覆盖等待 HTTP 响应头和读取 SSE，旧 Run 不会卡在尚未建立的流上。
- Ctrl-C/SIGTERM 先停止接受新连接并取消所有 Run/工具、关闭 RunSSE；HTTP graceful shutdown 最多等待 10 秒，随后强制释放服务。

模块边界和目录是实现约束，必须与仓库根目录 README 保持一致。
