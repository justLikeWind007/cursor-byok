# KV 协议详细分析

本文专门分析 Agent 协议中的 `KvServerMessage` / `KvClientMessage`。分析依据包括当前 `agent_v1.proto`、Connect 帧结构和本地 SQLite 抓包。

本文中的 KV 不指普通业务配置表，而指服务端通过 RunSSE 调用客户端 Blob Store 的协议。

## 1. 一句话结论

KV 是一个**客户端参与的内容寻址 Blob RPC**：

- 服务端请求客户端按 `blob_id` 保存或读取二进制内容。
- `blob_id` 是 Blob 内容的稳定地址，而不是随机数据库主键。
- Blob 主要用于 conversation checkpoint、prompt context 和其他大型上下文。
- KV 操作发生在 Agent 流内部，不是独立的 HTTP KV 服务。

更准确的技术名称是：

> Client-side content-addressed Blob Store over an application-level reverse RPC.

## 2. 协议分层

KV 不是直接出现在 HTTP body 顶层，而是嵌套在两条 Connect RPC 中。

### 2.1 服务端到客户端

```text
Connect server stream
  -> AgentServerMessage
      -> KvServerMessage
          -> GetBlobArgs / SetBlobArgs
```

对应的 protobuf：

```protobuf
message KvServerMessage {
  uint32 id = 1;
  optional SpanContext span_context = 4;
  oneof message {
    GetBlobArgs get_blob_args = 2;
    SetBlobArgs set_blob_args = 3;
  }
}
```

### 2.2 客户端到服务端

```text
Connect unary BidiAppend
  -> BidiAppendRequest
      -> data: hex(AgentClientMessage)
          -> KvClientMessage
              -> GetBlobResult / SetBlobResult
```

对应的 protobuf：

```protobuf
message KvClientMessage {
  uint32 id = 1;
  oneof message {
    GetBlobResult get_blob_result = 2;
    SetBlobResult set_blob_result = 3;
  }
}
```

因此，KV 的“请求方向”是 RunSSE，下行；KV 的“响应方向”是 BidiAppend，上行。这是应用层反向 RPC，不是客户端直接向某个 `/kv` HTTP endpoint 发请求。

## 3. 消息和参数

### 3.1 `GetBlobArgs`

```protobuf
message GetBlobArgs {
  bytes blob_id = 1;
}
```

功能：要求客户端返回指定 Blob。

`blob_id` 是二进制字段。当前抓包中长度为 32 字节，显示为 Base64 时通常是 44 个字符。

### 3.2 `GetBlobResult`

```protobuf
message GetBlobResult {
  optional bytes blob_data = 1;
  optional Error error = 2;
}
```

成功时返回 `blob_data`；读取失败时返回 `error.message`。协议没有单独定义 `not_found` 枚举，缺失、损坏和存储错误都需要通过 Error 文本表达。

### 3.3 `SetBlobArgs`

```protobuf
message SetBlobArgs {
  bytes blob_id = 1;
  bytes blob_data = 2;
}
```

功能：要求客户端按指定地址保存一段完整的 Blob。

KV 本身没有分片字段。一个 Blob 必须在一条 `SetBlobArgs` 中完整传输；大型内容依靠 Connect 的压缩和多个 Blob 拆分，而不是依靠 KV 内部的 chunk 序号。

### 3.4 `SetBlobResult`

```protobuf
message SetBlobResult {
  optional Error error = 1;
}
```

没有 `error` 表示写入成功；有 `error` 表示客户端拒绝或无法保存。

### 3.5 `SpanContext`

`KvServerMessage.span_context` 可携带 `trace_id`、`span_id`、`trace_flags` 和 `trace_state`。它用于分布式追踪，不参与 Blob 寻址、版本控制或响应关联。

## 4. 三个 ID 的区别

KV 运行时同时存在三种容易混淆的 ID：

| ID | 所属 | 作用 | 生命周期 |
| --- | --- | --- | --- |
| `request_id` | Bidi / RunSSE | 绑定一条 Agent 活动流 | 一次 turn 或运行实例 |
| `KvServerMessage.id` | KV 操作 | 关联服务端操作和客户端结果 | 当前 `request_id` 内的一次操作 |
| `blob_id` | Blob 内容 | 内容寻址和引用 | 只要内容或 checkpoint 仍可达就有效 |

此外还有 `BidiAppendRequest.append_seqno`：

- `KvServerMessage.id` 解决“哪个 KV 响应对应哪个 KV 请求”。
- `append_seqno` 解决“所有客户端上行消息应按什么顺序处理”。
- 两者不能互相替代。

本地样本中每个新的 `request_id` 都将 KV 操作 ID 从 0 重新开始，而 Bidi 上行序号还会被 heartbeat、Exec 和其他客户端消息占用。

## 5. 内容寻址规则

当前样本明确验证出：

```text
blob_id = SHA-256(blob_data)
```

验证结果：

| 检查项 | 结果 |
| --- | ---: |
| 三个 turn 中观察到的 `set_blob_args` | 30 |
| `blob_id == SHA-256(blob_data)` | 30 / 30 |
| 已观察的 `get_blob_result` | 2 |
| 读取结果通过请求 ID 的 SHA-256 校验 | 2 / 2 |

协议字段本身没有声明哈希算法或版本字段，因此 SHA-256 是根据实际数据推断出来的协议约定。实现时仍应把算法视为可配置或保留版本扩展空间，而不应只依赖“32 字节”这一表象。

内容寻址带来三个直接性质：

1. 相同内容得到相同 ID，可以去重。
2. 内容变化必然得到新 ID，Blob 可以视为不可变对象。
3. 客户端和服务端都能通过重新计算哈希校验传输是否损坏。

空内容也有对应的内容地址。样本中的空 rules 和 subagents 使用 SHA-256 空串值：

```text
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

## 6. Blob 引用的两类主要用途

### 6.1 Conversation checkpoint

`ConversationStateStructure` 的多个 bytes 字段实际可以承载 Blob 引用，例如：

- `turns[]`
- `root_prompt_messages_json[]`
- `conversation_state_blob_id`
- `prompt_context_usage_snapshot_blob_id`

在当前样本中，Turn Blob 可以解码为 `ConversationTurnStructure`，其结构为：

```text
ConversationTurnStructure
  └─ AgentConversationTurnStructure
       ├─ user_message: blob_id
       ├─ steps[]: blob_id
       └─ request_id
```

UserMessage Blob 可以解码为 `UserMessage`，其中又包含 `conversation_state_blob_id`。这个状态 Blob 继续引用根 Prompt Blob 和其他 checkpoint 数据。

因此 checkpoint 不是一个扁平 JSON，而是一个由多个 protobuf Blob 组成的引用图。

### 6.2 Request context

`ConversationAction.request_context_parts` 使用专门的引用结构：

```protobuf
message RequestContextPartReferences {
  bytes rules_blob_id = 1;
  uint32 rules_byte_length = 2;
  bytes skills_blob_id = 3;
  uint32 skills_byte_length = 4;
  bytes subagents_blob_id = 5;
  uint32 subagents_byte_length = 6;
  bytes mcps_blob_id = 7;
  uint32 mcps_byte_length = 8;
  RequestContext dynamic_context = 9;
}
```

这些 Blob 用来传输较大的 rules、skills、subagents 和 MCP 定义；小型、动态字段继续放在 `dynamic_context` 内。

样本中第二、第三个 turn 的 `get_blob_args` 分别读取：

| Turn | 引用类型 | Blob 大小 |
| --- | --- | ---: |
| 2 | `request_context_parts.mcps_blob_id` | 29,974 字节 |
| 3 | `request_context_parts.mcps_blob_id` | 59,145 字节 |

因此，KV 不只服务于 conversation history，也服务于每轮模型调用需要的大型上下文。

## 7. 当前会话的真实时序

### 7.1 第一个 turn

- 发送 9 个 `set_blob_args`。
- 客户端返回 9 个 `set_blob_result`。
- 其中包括 UserMessage、ConversationStep、ConversationTurn 和 Prompt/State 相关 Blob。
- 最终 checkpoint 的 `turns[]` 引用本轮的 Turn Blob。

### 7.2 第二个 turn

- `run_request` 携带上一轮 conversation state 和新的 request context 引用。
- 服务端读取 1 个 MCP context Blob，返回数据 29,974 字节。
- 服务端发送 7 个新 Blob，包括本轮消息、步骤、turn 和新的 context 状态。
- 服务端发布新的 checkpoint。

### 7.3 第三个 turn

- 服务端读取新的 MCP context Blob，返回数据 59,145 字节。
- 服务端发送 14 个新 Blob。
- 该 turn 还出现了 Exec 请求和结果，说明 KV 与本地工具协议可以在同一个 request actor 中并行存在。

一个重要结论是：当前样本中的 `get_blob` 不应简单解释为“服务端从客户端读取上一轮对话历史”。实际观察到的 `get_blob` 是 MCP request context。历史 Turn Blob 可能由服务端缓存，也可能在其他未捕获的路径同步；本样本不足以证明其读取路径。

## 8. 并发、顺序和幂等

### 8.1 多个 KV 请求可以并发

服务端可以在一条 RunSSE 中连续发送多个 `set_blob_args`。客户端随后并发发起多个 BidiAppend。

当前样本中，KV 操作 ID 和 HTTP 到达顺序不一致。例如一个 turn 中操作 ID 3、4、5 的响应在抓包记录里并非严格按 3、4、5 排列。这说明服务端不能按 HTTP 请求到达顺序匹配 KV 结果，必须按 `KvClientMessage.id` 关联。

### 8.2 `append_seqno` 是全局上行顺序

KV 结果的 BidiAppend 还会与 heartbeat、Exec 结果共享同一个 `append_seqno` 序列。因此：

- KV 操作 ID 只在 KV 子协议中使用。
- append 序号覆盖所有 `AgentClientMessage`。
- 服务端需要先按 append 序号处理上行消息，再按 KV ID 将结果交给对应的等待状态。

### 8.3 写入幂等和 ACK

一次成功的 KV 写入有两层确认：

1. HTTP/Connect 层返回 `BidiAppendResponse`，表示上行 append 被接收。
2. `KvClientMessage.set_blob_result` 没有错误，表示客户端 Blob Store 确实完成写入。

只有第二层确认才代表 Blob 可被后续 checkpoint 引用。重复发送同一个 `blob_id` 不会改变内容，但服务端仍需要处理重复的操作 ID、过期结果和客户端重试。

## 9. 失败语义和边界

KV 协议没有独立的错误枚举、删除、列举、TTL 或批量操作。当前可表达的失败主要是：

- `get_blob_result.error`：客户端找不到或无法读取 Blob。
- `set_blob_result.error`：客户端无法保存 Blob。
- BidiAppend 本身失败：上行 append 未被服务端接受。
- RunSSE 断开或 EndStream 失败：下行 KV 请求可能尚未完成。

因此服务端需要维护 pending KV 操作表：

```text
(request_id, KvServerMessage.id)
        -> blob_id
        -> waiting checkpoint / turn completion
```

当必要 Blob 写入失败或超时，服务端不能发布引用该 Blob 的成功 checkpoint；应选择重试、降级为未完成状态或结束当前 turn。

## 10. 安全与存储含义

KV 内容通过 HTTPS/Connect 传输，但协议本身没有声明 Blob 的存储加密、租户命名空间或访问权限。生产实现至少应考虑：

- 按用户、workspace 或 conversation 做访问隔离，不能只依赖公开的 SHA-256 值。
- 对 `blob_data` 做大小限制和哈希校验。
- 不把 Blob 正文写入普通请求日志。
- 对未知或过期 `KvClientMessage.id` 做幂等处理。
- 防止通过任意 `get_blob` 探测其他会话的内容。
- 明确客户端 Blob 的持久化、清理和迁移策略。

由于协议没有 delete 或 garbage-collection RPC，Blob 生命周期很可能由客户端本地存储策略、checkpoint 可达性或服务端外部存储策略负责。具体实现无法从当前抓包确认。

## 11. 对重写服务的直接启示

KV 不应被建模成一个简单的 `map[string][]byte` API。更合适的抽象是：

```text
BlobStore
  Put(content) -> content_hash
  Get(content_hash) -> content
  Has(content_hash) -> bool
```

上层再增加一次 request-scoped 的 RPC 编排：

```text
BlobOperation
  operation_id
  request_id
  blob_id
  kind: get | set
  status: pending | succeeded | failed | timed_out
```

Checkpoint 只保存 Blob 引用和小型元数据；Blob 本体由可替换的客户端存储适配器或共享存储适配器负责。KV 操作完成后，必须通过明确的 barrier 通知 checkpoint/turn 状态机继续收口。

## 12. 最终结论

KV 是 Agent 协议中的状态同步层，承担三个角色：

1. **checkpoint 的内容存储**：把历史消息、步骤和 turn 拆成不可变 Blob。
2. **大型上下文传输**：通过引用传递 Rules、Skills、Subagents 和 MCP 数据。
3. **客户端能力桥接**：云端通过 RunSSE 请求本地客户端保存或读取 Blob，再通过 BidiAppend 获得结果。

所以它不是普通的 KV 缓存，而是连接云端 Agent workflow、客户端本地状态和可恢复 conversation 的关键协议层。
