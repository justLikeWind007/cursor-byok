# cursor-server

Cursor Agent 的 Rust 服务端。它实现 `RunSSE + BidiAppend` 通信、无状态 LLM loop、客户端工具执行、Blob/KV 同步和可恢复 checkpoint。

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

进入 `cursor-server` 后启动：

```bash
CURSOR_DATABASE_URL=sqlite://cursor-server.db \
CURSOR_PROVIDER=openai-chat \
CURSOR_PROVIDER_BASE_URL=http://127.0.0.1:8317/v1 \
CURSOR_PROVIDER_API_KEY=123456 \
CURSOR_MODEL=deepseek-v4-flash \
cargo run
```

默认监听 `127.0.0.1:3000`。完整环境变量见 `src/config.rs`。

## 不变量

- `store/messages.rs` 是上下文唯一事实源；消息只追加，不原地修改。
- Runtime tag 使用稳定事件 ID，事务内 exactly-once 追加。
- 每轮投射结果可复现，后一轮 messages 严格以前一轮为前缀。
- 工具每完成一个，就按实际完成顺序原子追加一组 `assistant(tool_call) → tool(result)`；投射给 LLM 的上下文没有悬空 tool call，整批完整后才继续调用 LLM。
- Blob 是 `SHA-256(data)` 的不可变 CAS；Blob 类型来自引用字段，不编码在 BlobID 中。
- 引用 Blob 的 checkpoint 只有在全部新 Blob 得到 KV SET ACK 后才能发布。
- checkpoint 以单个工具为恢复粒度；最终 checkpoint 在 EndStream 前可重复发送。
- 新 Run 通过 conversation revision 使旧 Run 的迟到事件失效。
- 每种工具只对应一个 Exec、Interaction 或 Local 通道；不存在级联 fallback。
- Loop 不保存工具名称路由；`cursor/tools.rs` 是唯一 transport dispatcher。
- Interaction approval 不是 ToolResult；只有 typed terminal result 才能进入持久化与 checkpoint。
- Pending 项存在即 Running，终态通过 `take(id)` 一次消费；不维护重复的 finished/closed 标志。
- prompt 资产编译进二进制并在启动时整体校验，不与运行时目录逐文件混用。

模块边界和目录是实现约束，必须与仓库根目录 README 保持一致。
