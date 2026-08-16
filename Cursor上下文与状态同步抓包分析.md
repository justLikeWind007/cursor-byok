# Cursor  上下文、Blob 与状态同步抓包分析

## 1. 范围与结论

本文分析本机 SQLite 抓包：

```text
/Users/leokun/Library/Application Support/cursor-byok/cursor-proxy-debugger.db
```

分析对象是 Cursor 客户端与服务端通过 `RunSSE + BidiAppend` 组成的 Agent 通信协议，重点包括 messages、tool call、usage、model、subagent、MCP、skill、rules、commands、checkpoint 和 Blob。分析时不依赖体积很大且不便阅读的 `bidi_append_request.data` 字段，而优先使用已经解码的消息和 RunSSE 原始流。

核心结论：

1. Cursor 没有在每一轮都上传一份扁平的 OpenAI `messages[]`。
2. 一次 Run 由 `RunSSE` 下行事件流和多个 `BidiAppend` 上行命令共同完成。
3. 会话状态采用 `checkpoint + 内容寻址 Blob 图`。
4. `BlobID` 是 `SHA-256(blob_data)`，本身没有类型信息。
5. `conversation_state` 是状态根；Turn、UserMessage、Step、模型消息以及 rules/skills/MCP/subagent 上下文可以独立成为 Blob。
6. 服务端生成和编排 Blob，客户端执行协议中明确可见的 Blob Store 读写；服务端是否还保留云端副本，仅凭抓包不能确认。
7. 对当前大量小 Blob、强引用关系和原子 checkpoint 更新而言，SQLite 比纯文件系统更清晰。
8. 一条 RunSSE 正常覆盖一个用户 Turn 内的全部 LLM 调用和工具等待；`turn_ended`、最终 checkpoint、EndStream 是三个不同结束边界。
9. Runtime tag 以 user role 投射给 LLM，但来源是 runtime；每个事件严格追加一次，随后只原位重放，不能每轮重新追加。
10. Todo、Plan 等当前业务状态从有序 messages/tool results 确定性推导，不需要第二份可变业务事实。
11. 不同模式的静态 prompt、tools 和 reminder 资产直接复用 `main` 的完整版。

## 2. RunSSE 与 BidiAppend

两条 RPC 的分工：

```text
RunSSE(request_id)
  客户端订阅服务端事件：
  interaction update / exec request / KV request / checkpoint / end stream

BidiAppend(request_id, append_seqno, data)
  客户端提交：
  run_request / heartbeat / exec result / KV result / control message
```

`RunSSE` 请求体只有 `request_id`。实际的 `run_request` 位于紧随其后的首个 `BidiAppend` 中或者之前

`append_seqno` 是同一 `request_id` 内的有序上行序号，用于排序、去重和重试处理。多个 BidiAppend HTTP 请求可能并发到达，服务端不能把 HTTP 到达顺序当成协议顺序。

需要区分的 ID：

| ID | 作用域 | 用途 |
| --- | --- | --- |
| `conversation_id` | 跨 Turn | 持久会话、最新 checkpoint、子会话 |
| `request_id` | 一次 Run | 关联 RunSSE 与 BidiAppend |
| `run_id` | 一次执行 | 当前样本中常与 `request_id` 相同，但不应假设永远相同 |
| `append_seqno` | 单个 request | Bidi 上行排序与去重 |
| KV `id` | 单个 request | 配对 KV request/result |
| Exec `id` | 单个 request | 配对本地执行 request/result |
| `tool_call_id` | 模型工具调用 | 关联 tool call 生命周期 |
| `model_call_id` | 模型调用 | 关联模型输出与工具调用 |

这些 ID 属于不同命名空间，不能相互替代。

## 3. 首包与会话样本

数据库中有 6 个参与 Agent 协议的非空 `conversation_id`：

| 会话 | 首个 RunSSE | 首个 BidiAppend | 判断 |
| --- | ---: | ---: | --- |
| `78790094…241d` | 372 | 373 | 主会话 |
| `f195869b…2653` | 1062 | 1063 | 已直接确认的子 Agent |
| `14ffaf61…7c08` | 1163 | 1164 | 符合子 Agent 流 |
| `7b1e45aa…3881` | 1167 | 1168 | 符合子 Agent 流 |
| `8d5bf4c5…81c5` | 1201 | 1203 | 符合子 Agent 流 |
| `579e0b93…9c9b` | 1202 | 1204 | 符合子 Agent 流 |

首个 BidiAppend 的 `AgentClientMessage.run_request` 包含：

- `conversation_id`、`run_id` 和初始 `conversation_state`；
- `action.user_message_action.user_message`；
- `action.request_context_parts` 中 rules、skills、subagents、MCP 的 BlobID 和字节长度；
- 本轮可用的动态 request context；
- `requested_model`、候选子 Agent 模型和模型覆盖；
- 客户端能力位。

主会话首轮模型为 `grok-4.6`，参数为 `effort=high`、`fast=true`。首轮引用的 rules 为 1,114 字节、skills 为 5,207 字节、MCP 为 28,089 字节；空 subagents 使用 SHA-256 空串地址：

```text
47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=
```

父会话中已直接观察到 Task tool call 返回子会话 `f195869b…2653`，并给出位于父会话目录下的 transcript 路径。其余四个会话的首条消息是并发调查任务，符合子 Agent 行为，但父子关系恢复应以 checkpoint 中的 subagent 映射为准，不能只靠时间推断。

## 4. BlobID 的数据结构

### 4.1 定义

```text
blob_id = SHA-256(blob_data)
```

BlobID 的逻辑结构只有 32 字节：

```rust
type BlobId = [u8; 32];

struct Blob {
    id: BlobId,
    data: Vec<u8>,
}
```

协议中的 `bytes` 经 ProtoJSON 展示为 Base64，因此同一个 ID 常见三种表示：

```text
原始：32 bytes / 256 bits
Hex：64 个十六进制字符
Base64：通常 44 个字符，包含末尾 =
```

真实示例：

```text
blob_data:
{"role":"system","content":"You are an AI coding assistant..."}

SHA-256 Hex:
3f784b31ca7e238c0a8f59718d49860c83cd75de09357bd97bfc457ee543c6c7

Base64 BlobID:
P3hLMcp+I4wKj1lxjUmGDIPNdd4JNXvZe/xFfuVDxsc=
```

### 4.2 BlobID 不包含什么

BlobID 不包含：

- 内容类型；
- Blob 长度；
- `conversation_id` 或 `request_id`；
- 创建时间；
- JSON/protobuf 编码标记；
- schema 版本。

因此，单独拿到 `blob_id + blob_data` 时，不能从 ID 本身可靠得知类型。

### 4.3 不可变和去重

Blob 内容发生一个字节变化，SHA-256 就会变化，因此 Blob 是不可变对象。所谓“更新消息”实际是：

```text
生成新内容
→ 生成新 BlobID
→ 写入新 Blob
→ 新 checkpoint 改为引用新 Blob
```

相同内容产生相同 BlobID，可以天然去重和校验完整性。

## 5. 什么是对象图

数据没有集中存在一个连续的 `messages[]` 中，而是拆成多个独立对象，通过 BlobID 相互引用：

```text
ConversationState
├─ root_prompt_messages_json[] → JSON model-message Blobs
└─ turns[] → ConversationTurnStructure Blob
              ├─ user_message → UserMessage Blob
              └─ steps[] → ConversationStep Blobs
                            ├─ ThinkingMessage
                            ├─ AssistantMessage
                            └─ ToolCall
```

之所以称为“图”：

- 一个对象可以引用多个对象；
- 同一个对象可以被多个 checkpoint 或分支复用；
- 从 checkpoint 根出发，可以沿引用访问所有可达对象；
- 它不要求是单链表，也不必严格是一棵树。

模型侧完整 transcript 还包括 `root_prompt_messages_json[]` 中的 `system/user/assistant/tool` JSON 消息。因此工具结果既可以作为模型 transcript 中的 `role=tool` JSON Blob 出现，Turn 的结构化 Step 则由 `ConversationStep` 的 Thinking、Assistant 和 ToolCall 分支表示。

## 6. KV 协议与客户端/服务端职责

KV 不是普通业务配置表，而是服务端通过 Agent 流调用客户端 Blob Store 的反向 RPC。

```text
服务端 → 客户端（RunSSE）
AgentServerMessage.kv_server_message
  ├─ get_blob_args(blob_id)
  └─ set_blob_args(blob_id, blob_data)

客户端 → 服务端（BidiAppend）
AgentClientMessage.kv_client_message
  ├─ get_blob_result(blob_data / error)
  └─ set_blob_result(error?)
```

职责划分：

| 操作 | 服务端 | 客户端 |
| --- | --- | --- |
| 写 Blob | 生成内容和 ID，下发 `set_blob_args` | 校验、持久化，返回 `set_blob_result` |
| 读 Blob | 下发 `get_blob_args`，保存 KV `id → BlobID/类型` | 查找并返回 `get_blob_result` |
| Checkpoint | 生成新的引用关系和快照 | 接收快照，下一轮随 `run_request` 传回 |
| 完整性 | 校验读取数据的 SHA-256 | 写入前校验 `SHA-256(data) == id` |
| 生命周期 | 维护 active run 和待完成调用 | Blob 跨 request、跨 Turn 保留 |

`set_blob_args` 的含义是“服务端命令客户端写入 Blob Store”，不是让服务端写自己的数据库。

写入时序：

```text
服务端生成 UserMessage / Step / Turn
→ SHA-256(data)
→ RunSSE set_blob_args
→ 客户端保存
→ BidiAppend set_blob_result
→ 服务端发布引用这些 Blob 的 checkpoint
```

读取时序：

```text
run_request 只携带上下文 BlobID
→ 服务端需要数据时发送 get_blob_args
→ 客户端从 Blob Store 读取
→ BidiAppend get_blob_result(blob_data)
→ 服务端校验并按预期类型解码
```

协议明确证明客户端承担 Blob Store 职责。服务端为了性能、恢复或多节点调度是否也缓存/持久化 Blob，抓包无法证明。

## 7. 首个 Bidi 到 `turn_ended` 的真实 Blob 生命周期

重新解码 exchange 372 的完整 RunSSE 原始流后得到：

| 指标 | 数量 |
| --- | ---: |
| RunSSE 帧 | 3,174 |
| `set_blob_args` | 154 |
| `get_blob_args` | 0 |
| 客户端 `set_blob_result` | 154 |
| checkpoint 更新 | 29 |
| `turn_ended` | 1 |
| 最终 Turn Steps | 66 |

先前 SQLite 中 `response.frames` 仅保留了 frame 1231–1999，这是调试视图帧数上限导致的截断；数据库 `response.rawHex` 保存了完整流。不能因为持久化 frames 视图缺少早期 KV 帧，就判断首轮没有 KV 操作。

### 7.1 首批对象

首轮最早的 KV SET：

| KV id | BlobID | 类型/内容 |
| ---: | --- | --- |
| 0 | `P3hL…xsc=` | system Prompt JSON |
| 1 | `ELlL…J9o=` | 环境、rules、skills、MCP 等注入上下文 JSON |
| 2 | `XXoI…tug=` | `ConversationStateStructure` |
| 3 | `ObvD…fy8=` | `UserMessage` |
| 4 | `LyfC…fJg=` | 实际用户消息的模型 JSON |
| 5 | `vJlr…J2M=` | Thinking `ConversationStep` |
| 6 | `Krvg…aHtU=` | Assistant `ConversationStep` |
| 7 | `AULh…IlY=` | `ConversationTurnStructure` |

第一次 checkpoint：

```text
Checkpoint
├─ roots[0] → P3hL…  system JSON
├─ roots[1] → ELlL…  注入上下文 JSON
├─ roots[2] → LyfC…  用户模型消息 JSON
└─ turns[0] → AULh…  ConversationTurnStructure
                ├─ user_message → ObvD… UserMessage
                │                  └─ conversation_state_blob_id → XXoI…
                ├─ steps[0] → vJlr… ThinkingMessage
                └─ steps[1] → Krvg… AssistantMessage
```

第一次 Turn 解码结果：

```text
request_id = 1f135cdf-3d41-4e2b-a324-b6632c745f94
user text  = 调查cursor.app 客户端，我发现他除了启动参数可以覆盖backend point之外，
             都把值写死了，有逃生通道吗？
steps      = ThinkingMessage + AssistantMessage
```

### 7.2 增量 checkpoint

每批模型输出或工具执行期间，服务端会穿插发布 checkpoint，而不是只在最终 `turn_ended` 时发布：

1. 写入新增的 Prompt JSON、Thinking、Assistant、ToolCall 等 Blob；
2. 生成包含更多 Step 引用的新 Turn Blob；
3. 发布引用新 Turn 的 checkpoint；
4. 保留旧 Blob，不原地修改旧 Turn。

抓包中一次四工具并发调用的实际顺序是：

```text
frame 66–69  SET User/Thinking/Assistant/Turn Blob
frame 70     第一个 tool_call_completed
frame 72     checkpoint：pending_tool_calls 非空，Turn 仍只有 2 个 Step
frame 73/75/76  其余三个 tool_call_completed
frame 79–88  SET assistant JSON、四个 tool result JSON、四个 Tool Step、新 Turn
frame 89     checkpoint：pending_tool_calls 清空，Turn 扩展到 6 个 Step
```

因此当前 Cursor 样本中的中间 checkpoint 能表达“存在尚未收口的工具批次”，但第一个工具完成时并没有立即把该工具结果加入 Turn。四个工具结果最终一起进入新的 Turn。它并未实现真正的单 Tool 结果回滚粒度。

最终 Turn：

```text
BlobID = tvMP+yn6+gVOQn4BzfwSJKRJ3CnSxsXxWKULFvMT4zs=
Steps  = 66
├─ ThinkingMessage：14
├─ AssistantMessage：10
└─ ToolCall：42
```

最终顺序：

```text
SET 最终 User/Step/Turn/Prompt Blob
→ interaction_update.turn_ended
→ 最终 checkpoint（roots=59, turns=1）
→ end_stream
```

首轮没有 KV GET，是因为该轮需要的 request context 已在 `run_request` 中可用。后续 Turn 才观察到服务端按引用读取 MCP、subagent 等客户端已有 Blob。

“消费 Blob”并不表示删除 Blob，而是读取、解码或让新的父对象/checkpoint 引用它。

## 8. 如何确定 Blob 类型

### 8.1 类型来自引用位置

BlobID 不自描述。最可靠规则是：

```text
引用字段 → 预期消息类型 → 读取 Blob → 校验哈希 → 按预期类型解码
```

常见映射：

| 引用字段 | Blob 内容类型 |
| --- | --- |
| `root_prompt_messages_json[]` | UTF-8 JSON model message |
| `turns[]` | `ConversationTurnStructure` |
| `AgentConversationTurnStructure.user_message` | `UserMessage` |
| `AgentConversationTurnStructure.steps[]` | `ConversationStep` |
| `UserMessage.conversation_state_blob_id` | `ConversationStateStructure` |
| `rules_blob_id` | `RequestContextRulesPart` |
| `skills_blob_id` | `RequestContextSkillsPart` |
| `subagents_blob_id` | `RequestContextSubagentsPart` |
| `mcps_blob_id` | `RequestContextMcpsPart` |

protobuf 字段本身通常只声明 `bytes`，不会直接写出“这是某种消息的 BlobID”。映射的验证过程是：

1. 从字段名和相邻 schema 提出候选类型；
2. 将字段中的 32 字节值与 KV `blob_id` 对齐；
3. 验证 `SHA-256(blob_data) == blob_id`；
4. 按候选 protobuf/JSON 类型解码；
5. 检查解码出的子 BlobID 是否继续精确匹配已知对象；
6. 检查业务字段是否合理，例如用户正文、request ID、Step oneof、MCP server 名称。

这使类型映射成为“schema 引导、抓包交叉验证”的可信协议语义，而不是仅凭 protobuf 解码成功进行猜测。

### 8.2 为什么不能遍历所有 protobuf 类型猜测

protobuf wire format 允许未知字段，不同消息也可能恰好使用相同字段号。因此“某个类型解码没有报错”不能证明它就是正确类型。

只有孤立的 `blob_id + blob_data` 时，可以做启发式检测：

1. 尝试 UTF-8 和 JSON；
2. 查找该 BlobID 在 checkpoint/request context 中的引用位置；
3. 按引用位置指定的消息解码；
4. 验证子引用、oneof 和业务约束。

引用位置是决定性证据。

### 8.3 服务端的 Pending Read

`GetBlobResult` 不再次携带 BlobID 和类型，只通过 KV `id` 配对。因此服务端发送 GET 时必须记录预期类型：

```rust
enum BlobKind {
    RootPromptJson,
    ConversationTurn,
    UserMessage,
    ConversationStep,
    ConversationState,
    Rules,
    Skills,
    Subagents,
    Mcps,
}

struct PendingBlobRead {
    blob_id: BlobId,
    kind: BlobKind,
}

// request_id 内：kv_id -> 待读取对象
type PendingReads = HashMap<u32, PendingBlobRead>;
```

收到结果后：

```text
按 request_id + KV id 找到 PendingBlobRead
→ 验证 SHA-256(blob_data)
→ 按 kind 解码
→ 删除 pending entry
```

## 9. `root_prompt_messages_json[]` 的真实含义

字段名容易误导。当前样本中数组元素不是内联 JSON，而是 32 字节 JSON BlobID。每个 Blob 的内容是 AI SDK 风格模型消息：

```json
{
  "role": "system | user | assistant | tool",
  "content": "string 或 content parts",
  "providerOptions": {}
}
```

因此它实际上保存完整模型 transcript，包括 system、环境注入、用户消息、assistant 的文本/工具调用以及 tool result。

用户给出的前 39 个 Hash 对应：

| # | Hash 前缀 | Role | 内容摘要 |
| ---: | --- | --- | --- |
| 1 | `P3hLMcp+` | system | Grok 4.6 身份、沟通规范、代码引用格式、终端说明 |
| 2 | `ELlLOnHq` | user | OS、workspace、git status、AGENTS.md、用户规则、skills、MCP 说明 |
| 3 | `LyfCaiSg` | user | 实际用户问题、最近文件和时间 |
| 4 | `52n788Ej` | assistant | 开始调查并调用 Read/Glob/Grep |
| 5 | `/aJ7CTfY` | tool | Cursor SDK Skill 内容 |
| 6 | `RGulDIiG` | tool | `协议消息参考.md` 内容 |
| 7 | `YtO9x4D/` | tool | 项目文件搜索结果 |
| 8 | `PsawfTKI` | tool | backend URL 项目搜索结果 |
| 9 | `lzA+DtLc` | assistant | 转向检查 Cursor.app |
| 10 | `yo7D20v+` | tool | 项目 URL/环境变量搜索结果 |
| 11 | `/WqUgT3v` | tool | Cursor.app 文件枚举 |
| 12 | `I6aByrAS` | tool | `cursor-backend/README.md` |
| 13 | `IPdh1rHG` | assistant | 核对 product.json、环境变量和启动参数 |
| 14 | `tpIiveZ5` | tool | 找到 Cursor.app `product.json` |
| 15 | `1WdnPQPA` | tool | Cursor.app backend/环境变量搜索结果 |
| 16 | `oAppncaI` | tool | Resources 下 product.json 搜索结果 |
| 17 | `JIJ54ISX` | assistant | 读取 product.json 和 URL 解析逻辑 |
| 18 | `wXb/KckF` | tool | `product.json` 内容 |
| 19 | `9rST0eEK` | tool | `testBackendUrl`、`CURSOR_API` 搜索结果 |
| 20 | `7JrovZ73` | tool | CLI/backend 参数搜索结果 |
| 21 | `Lu809FSq` | assistant | 改用精确字符串提取 |
| 22 | `HXzTKOyk` | tool | `--test-backend-url` 实现片段 |
| 23 | `DMsvuq7g` | tool | `CURSOR_API_BASE_URL`、`CURSOR_API_ENDPOINT` 实现片段 |
| 24 | `suq6AUbu` | tool | 大型 backend/env 搜索结果，约 2.2 MB |
| 25 | `AywrAO5b` | assistant | 分析覆盖入口生效范围 |
| 26 | `9SZKucWy` | tool | `getBackendEndpoint()` 实现 |
| 27 | `FjBwMVSf` | tool | `CURSOR_API_BASE_URL` 默认值逻辑 |
| 28 | `Fgfm+DUJ` | tool | `CURSOR_API_ENDPOINT` agent host 覆盖逻辑 |
| 29 | `99BIWrd5` | tool | `testBackendUrl` 主进程覆盖逻辑 |
| 30 | `W5M9yeDy` | assistant | 检查 cursorCreds、本地/staging 和正式包限制 |
| 31 | `AdzQyOH2` | tool | cursorCreds/local server 搜索结果 |
| 32 | `+5nzixtk` | tool | Cursor 环境变量与 agent worker 搜索结果 |
| 33 | `1QGiXmdG` | tool | 硬编码 Cursor URL 统计 |
| 34 | `wJee1lVb` | assistant | 检查 dev gate、命令面板和 argv.json |
| 35 | `JyILF2iY` | tool | bundle 常量位置、dev gate、argv 索引 |
| 36 | `Se4p8qnN` | tool | Cursor bundle 相关实现搜索结果 |
| 37 | `DP7BNc0R` | tool | Application Support 下未找到 argv.json |
| 38 | `P69E2Uha` | assistant | 发起更详细的 bundle 提取 |
| 39 | `Pn8LB3dA` | tool | URL 默认值、cursorCredsService、本地后端和正式包限制 |

真正的根 system Prompt 是第 1 条；第 2 条是 Cursor 注入上下文；第 3 条开始是对话和工具轨迹。

## 10. MCP Blob 实例

BlobID：

```text
EeyByQBjy9x5oG8FYietUSCLGFacxFTv13GIDHf0WG8=
```

它由 `request_context_parts.mcps_blob_id` 引用，因此预期类型是：

```text
agent.v1.RequestContextMcpsPart
```

抓包时序：

```text
RunSSE exchange 881, frame 1:
  get_blob_args(id=0, blob_id=EeyB...WG8=)

BidiAppend exchange 889:
  get_blob_result(id=0, blob_data)
```

解码结果约 28 KB，且重新计算 SHA-256 后精确得到原 BlobID。内容：

```text
RequestContextMcpsPart
├─ mcp_instructions
│  ├─ browser-use：完整使用说明
│  ├─ context7：文档查询使用规则
│  └─ codegraph：workspace 未索引说明
├─ mcp_file_system_options
│  ├─ browser-use
│  │  ├─ browser_exec
│  │  └─ browser_screenshot
│  ├─ context7
│  │  ├─ resolve-library-id
│  │  └─ query-docs
│  ├─ tuicommander
│  ├─ codegraph
│  └─ gmail
└─ mcp_meta_tool_options
   └─ 同类 MCP descriptors
```

大部分体积来自 browser-use 的完整 server instructions、工具描述、插件信息和本地描述路径。

## 11. Messages、Tool、Usage、Model、上下文与 Subagent

| 数据 | 输入/流式增量 | checkpoint/Blob 状态 |
| --- | --- | --- |
| Messages | `user_message_action`、`text_delta`、`thinking_delta` | Prompt JSON、UserMessage、Turn、Step Blob |
| Tool call | `tool_call_started/partial/delta/completed` | JSON transcript、ToolCall Step、`pending_tool_calls` |
| 本地命令/工具 | RunSSE `exec_server_message` | BidiAppend `exec_client_message`，再写入 transcript/Step |
| Usage | `token_delta` | `token_details`、breakdown、usage snapshot Blob |
| Model | `run_request.requested_model` | `model_call_id` 关联具体调用 |
| Runtime tag | 以 user role 投射的 `<system_reminder>` 等运行时消息 | 作为不可变模型消息严格追加一次 |
| Todo / Plan | TodoWrite、CreatePlan 等 message/tool result | 从有序历史确定性投影；checkpoint 字段只是视图 |
| Rules | `rules_blob_id` 或动态上下文 | `RequestContextRulesPart` Blob |
| Skills | `skills_blob_id` | `RequestContextSkillsPart` Blob |
| MCP | `mcps_blob_id` | `RequestContextMcpsPart` Blob |
| Subagent 定义 | `subagents_blob_id`、model overrides | `RequestContextSubagentsPart` Blob |
| Subagent 运行 | Task tool call 启动独立 conversation | checkpoint 的 states/threads/run 映射 |

样本 checkpoint 的 token breakdown 包含：system prompt、tools、rules、skills、MCP、subagents、summarized conversation 和 conversation。`token_delta` 是流式增量，不应当直接当成最终 usage 快照。

## 12. Rust 服务端状态边界

最小但完整的运行模型：

```text
conversation_id → latest checkpoint
request_id      → active run
(request_id, append_seqno) → Bidi 排序/去重
(request_id, kv_id)        → pending Blob read/write
(request_id, Exec, id)     → pending local execution
blob_id         → immutable bytes
```

恢复模型上下文：

```text
加载 latest checkpoint
→ 沿 turns / user_message / steps 读取结构化历史
→ 读取 root_prompt_messages_json 模型 transcript
→ 合并本轮 rules / skills / MCP / subagent 定义
→ 应用 requested_model 和能力参数
→ 运行 Agent loop
```

结束本轮：

```text
每次可恢复状态变化先写相关 Blob
→ 等待当前候选 checkpoint 引用闭包中的 set_blob_result
→ 发布引用它们的新 checkpoint
→ 最终 turn_ended / final checkpoint / end_stream
```

## 13. 持久化方案

### 13.1 为什么 SQLite 更适合当前数据

当前样本一个 Turn 产生 154 个 Blob，多数是几百字节到数十 KB，也存在约 2.2 MB 的工具结果。SQLite 的优势：

- 避免大量小文件和 inode 压力；
- Blob、引用边和 conversation head 可以一个事务提交；
- `INSERT OR IGNORE` 天然支持内容寻址幂等写入；
- 容易查询父子引用图；
- 可用递归查询做可达性 GC；
- 单文件便于备份、迁移和调试。

建议先实现 SQLite，不增加尚未被规模证明需要的对象存储抽象。

### 13.2 表结构

```sql
CREATE TABLE blobs (
    id          BLOB PRIMARY KEY CHECK(length(id) = 32),
    kind        INTEGER,
    data        BLOB NOT NULL,
    size_bytes  INTEGER NOT NULL,
    codec       INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE blob_edges (
    parent_id   BLOB NOT NULL,
    child_id    BLOB NOT NULL,
    kind        INTEGER NOT NULL,
    ordinal     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (parent_id, kind, ordinal),
    FOREIGN KEY (parent_id) REFERENCES blobs(id),
    FOREIGN KEY (child_id) REFERENCES blobs(id)
) WITHOUT ROWID;

CREATE INDEX blob_edges_child_idx
ON blob_edges(child_id);

CREATE TABLE conversation_heads (
    conversation_id TEXT PRIMARY KEY,
    checkpoint_id   BLOB NOT NULL,
    revision        INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    FOREIGN KEY (checkpoint_id) REFERENCES blobs(id)
);
```

要区分两个 kind：

```text
blobs.kind      = Blob 自身的解码类型，例如 ConversationStep
blob_edges.kind = 父对象中的引用语义，例如 TurnStep
ordinal         = 数组下标，例如 steps[17]
```

`blobs.kind` 可以为空，因为同一 opaque Blob 的固有类型有时未知；真正可靠的解码上下文仍来自引用边。

建议的边类型包括：

```text
CheckpointRootPrompt
CheckpointTurn
TurnUserMessage
TurnStep
UserConversationState
RequestRules
RequestSkills
RequestSubagents
RequestMcps
```

### 13.3 原子写入

应先写子对象，最后更新 head：

```text
put(UserMessage)
→ put(Steps...)
→ put(Turn)
→ put(Checkpoint)
→ 写 blob_edges
→ 更新 conversation_heads
```

推荐在一个 SQLite 写事务中完成：

```sql
BEGIN IMMEDIATE;
-- INSERT OR IGNORE blobs
-- INSERT checkpoint blob
-- INSERT blob_edges
-- UPDATE conversation_heads
COMMIT;
```

这样崩溃最多留下不可达 Blob，不会让 conversation head 指向缺失对象。

运行配置：

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
```

### 13.4 Blob Store 接口

```rust
#[async_trait]
trait BlobStore {
    async fn put(
        &self,
        expected_id: BlobId,
        data: Bytes,
    ) -> Result<PutOutcome>;

    async fn get(&self, id: BlobId) -> Result<Option<Bytes>>;

    async fn get_many(
        &self,
        ids: &[BlobId],
    ) -> Result<Vec<Option<Bytes>>>;
}
```

`put` 必须执行：

```text
计算 SHA-256(data)
→ 与 expected_id 比较
→ 不匹配则拒绝
→ 已存在则幂等成功
→ 不存在则插入
```

### 13.5 垃圾回收

不要依赖简单引用计数，因为 Blob 可能被多个 checkpoint、分支或 subagent 状态共享。使用 mark-and-sweep：

```text
GC Roots
├─ conversation_heads
├─ active runs
├─ pending KV operations
└─ pinned snapshots

从 Roots 沿 blob_edges 标记
→ 删除超过保留期且不可达的 Blob
```

## 14. 如果使用文件系统

FS 可用于单机 CAS，但不应只靠目录表达引用关系。推荐混合方案：文件系统保存 opaque bytes，SQLite 保存 metadata、edges 和 conversation head。

```text
data/
├─ blobs/sha256/3f/78/3f784b31ca7e...543c6c7
└─ metadata.db
```

文件名使用 Hex，不使用含 `/`、`+` 的 Base64。

安全写入：

```text
计算并校验 SHA-256
→ 在最终目录写临时文件
→ fsync 文件
→ rename 到最终路径
→ fsync 父目录
→ SQLite 事务写 edges/head
```

FS 合适于：

- 单实例和稳定本地磁盘；
- Blob 较大、数量可控；
- 希望单独迁移或检查 Blob 文件。

FS 不适合当前样本的主要原因：

- 一个 Turn 产生大量小 Blob；
- 数百万小文件会带来 inode、目录扫描和备份压力；
- 多节点/NFS 上原子语义和一致性更复杂；
- 引用图仍然需要额外数据库。

因此当前选择为：

```text
单机、大量小 Blob：SQLite BLOB
单机、Blob 明显偏大：FS + SQLite metadata
多节点：PostgreSQL bytea；规模证明需要后再考虑对象存储
```

## 15. 实现原则总结

1. 不要把 Cursor 状态强行压成单个 `messages[]`。
2. 把 Blob 当成不可变、内容寻址的 opaque bytes。
3. Blob 类型来自引用字段和运行时 pending context，不来自 BlobID。
4. 所有写入都校验 `SHA-256(data) == blob_id`。
5. 保存引用顺序，尤其是 roots、turns 和 steps 的 ordinal。
6. 先落 Blob，最后原子切换 conversation head。
7. 用 `append_seqno` 恢复 Bidi 顺序，不依赖 HTTP 到达顺序。
8. 独立维护 KV、Exec、tool call 和 model call ID 空间。
9. 用 checkpoint 作为恢复根，用引用图恢复模型 transcript 和结构化状态。
10. 当前版本优先 SQLite，等真实规模证明瓶颈后再扩展存储层。
11. 分开处理 `turn_ended`、final checkpoint 和 RunSSE EndStream 三个结束边界。
12. Runtime event 只生成一个 user-role/runtime-origin message，并与消费标记原子提交；重试只能重放，不能重复追加。
13. Todo、Plan 从有序 message/tool result 历史确定性推导，不维护第二份可变事实。
14. 静态 prompt/tools 按模式加载；动态 reminder 也必须先成为一次性追加的不可变 message，再参与 LLM 投射。

## 16. LLM Loop 与端点适配

### 16.1 LLM 无状态，Loop 有状态

LLM 本身不保存会话。Loop 引擎反复把当前完整上下文投射成一次模型请求，处理流式响应，并在需要工具时等待客户端执行：

```text
读取 committed conversation state
→ 投射 provider request
→ 流式调用 LLM
→ assistant 文本结束：完成 Turn
→ assistant 工具调用结束：等待客户端执行
→ 工具结果进入历史
→ 投射下一次完整 LLM request
→ 直到 stop / error / abort
```

一次 Cursor `RunSSE` 可以覆盖同一 Agent Turn 内的多次 LLM 调用和多次工具等待，不应把一次 provider 请求等同于一次 RunSSE。

用户可以用新的 `run_request` 随时打断旧 Run。服务端应取消旧 provider stream 和未继续执行的工作，并用 conversation generation/revision 防止旧 Run 的迟到事件更新新 conversation head。

### 16.2 Provider Adapter

不同模型端点的 SSE 事件、tool schema、stop reason 和 usage 字段不同。Loop 内部应只消费统一事件：

```text
Provider SSE
→ OpenAI Chat / Responses / Anthropic Adapter
→ Canonical ResponseEvent
→ Loop State Machine
→ Cursor AgentServerMessage
→ Connect RunSSE
```

建议的统一事件包括：

```text
start
text_start / text_delta / text_end
thinking_start / thinking_delta / thinking_end
toolcall_start / toolcall_delta / toolcall_end
done(stop | length | toolUse | error | aborted)
error
```

模型端点差异只留在 Adapter。Loop、Blob、checkpoint 和 Cursor transport 不依赖具体 provider。

### 16.3 RunSSE 的线格式

`RunSSE` 虽然使用流式 HTTP，但正文不是浏览器式文本 `data: ...\n\n`。它是 Connect 流式二进制 envelope：

```text
1 byte flags
4 bytes big-endian payload length
protobuf AgentServerMessage
```

结束帧使用 `flags & 0x02`，payload 是 Connect EndStream JSON。实现时应把每个 Cursor 事件编码为 protobuf 后写入 Connect envelope。

## 17. 前缀缓存与幂等投射

### 17.1 Blob 图满足稳定前缀的基础条件

`root_prompt_messages_json[]` 是有序 BlobID 列表，历史消息 Blob 不可变，新消息只追加到后缀。因此它适合作为前缀缓存的持久化基础：

```text
固定 system prompt
→ 稳定 rules / skills / MCP / subagent 定义
→ 已提交历史消息
→ 本轮实时追加的新消息与 Runtime tag
```

所有已经投射给 LLM 的消息都进入同一个只追加序列：

```text
M(n+1) = M(n) || Δmessages
```

Runtime tag 虽然是实时产生的，但首次追加后也立即成为不可变历史。下一轮只能在相同位置原样重放，不能重新生成、替换、删除、移动或重复追加。只要遵守这一点，相邻请求就能保留最大的共同前缀。

`model_call_id` 只是一次模型调用的关联 ID，不是 provider 前缀缓存条件。跨模型时正常构造新请求即可，不应为了复用 `model_call_id` 改写历史。

### 17.2 Tool call/result 的完整性约束

工具可以并行执行并乱序完成，但下一次模型调用不能看到悬空 tool call。内部状态应把每个调用和结果一对一关联：

```text
ToolBatch
├─ slot 0: call0 ↔ result0
├─ slot 1: call1 ↔ result1
└─ slot 2: call2 ↔ result2
```

只有当前模型产生的 Tool Batch 全部具有可投射结果后，才能构造下一轮 provider request。该约束保证相同 committed state 总能产生相同请求。

抓包中的模型 transcript 采用 AI SDK/OpenAI Chat 风格：

```text
assistant: [call0, call1, call2, call3]
tool: result0
tool: result1
tool: result2
tool: result3
```

真实执行完成顺序为 `result1、result3、result2、result0`，但最终持久化顺序恢复成原始 call 顺序。这里的“一对一”是语义配对和原子投射约束，不要求所有 provider 都使用字面上的 `call0,result0,call1,result1` 排列；具体线格式由端点 Adapter 决定。

## 18. Tool 流事件与客户端占位卡片

Cursor 专门提供 `partial_tool_call` 表示“工具类型已知，但参数尚未完整”：

```proto
message PartialToolCallUpdate {
  string call_id = 1;
  ToolCall tool_call = 2;
  string args_text_delta = 3;
  string model_call_id = 4;
}
```

抓包中第一个 partial 事件已经包含 `call_id`、`model_call_id` 和具体 `ToolCall.oneof`，例如 `read_tool_call {}`、`glob_tool_call {}`、`grep_tool_call {}`、`task_tool_call {}`，但参数可以为空。客户端可据此立即绘制具体类型的占位卡片。

正确映射：

| LLM 统一事件 | Cursor 事件 | 含义 |
| --- | --- | --- |
| `toolcall_start` | `partial_tool_call` | 创建已知工具类型的占位卡片 |
| `toolcall_delta` | `partial_tool_call.args_text_delta` | 追加模型正在生成的参数 JSON |
| `toolcall_end` | `tool_call_started` | 参数完整、解析和校验通过，开始客户端执行 |
| 客户端执行增量 | `tool_call_delta` | stdout、进度、编辑状态等执行结果增量 |
| 客户端最终结果 | `tool_call_completed` | 结束工具卡片并进入持久化流程 |

`tool_call_delta` 不是模型生成工具参数的 delta。抓包中的 shell `tool_call_delta` 出现在 `tool_call_started` 之后，携带 stdout 等执行输出。

同一个 call 可以出现多个 `partial_tool_call`：第一个只带空的 typed oneof，后续带 `args_text_delta` 或已经能够增量解析的结构化参数。客户端应以 `call_id` upsert，同一个 call 不能重复创建卡片。

服务端需要明确的工具注册表：

```text
tool name
→ Cursor ToolCall.oneof
→ empty placeholder constructor
→ incremental args decoder
→ complete args validator
→ ExecServerMessage variant
→ execution delta/result mapper
```

## 19. Checkpoint 的时机与单 Tool 回滚

### 19.1 Checkpoint 是可恢复提交，不只是 UI 快照

Checkpoint 是客户端历史回滚和下一次 `run_request` 恢复的依据。它只能引用客户端已经能够读取的 Blob。基本提交顺序是：

```text
生成不可变子 Blob
→ RunSSE set_blob_args
→ 客户端持久化
→ BidiAppend set_blob_result(success)
→ 生成/确认父 Turn 与 checkpoint 的完整引用闭包
→ RunSSE conversation_checkpoint_update
```

如果先发布 checkpoint，再等待它引用的 Blob 落盘，客户端一旦在两者之间重启，就会得到含悬空引用的历史头。

### 19.2 当前 Cursor 抓包的真实粒度

Checkpoint 确实穿插在 Loop 中，而不只位于最终 `done`：

```text
checkpoint pending=1
→ 一个或多个工具完成
→ tool/result/step/turn Blob SET 并确认
→ checkpoint pending=0
→ 下一轮 LLM
```

但抓包中的四工具样本没有把第一个完成结果单独写进 Turn。第一个 `tool_call_completed` 后发出的 checkpoint 仍只有 Thinking 和 Assistant 两个 Step；等四个工具全完成后，四个 Tool Step 才一起进入新 Turn。因此当前样本能恢复到“工具批次正在执行”，不能恢复到“其中某一个工具已经完成且结果已持久化”。

### 19.3 服务端目标：单 Tool 粒度

为了避免崩溃后重复执行写文件、shell、MCP 等有副作用的工具，服务端实现应提高到单 Tool 粒度。每个 call 使用固定 slot，完成状态可以和其他工具交叉：

```text
slot[0] = call0 pending
slot[1] = call1 completed(result1)
slot[2] = call2 pending
```

建议两个提交点：

```text
Tool 参数完整
→ 持久化 Tool Intent
→ Blob 确认
→ checkpoint(call=pending)
→ 才向客户端发 ExecServerMessage

Tool Result 返回
→ 服务端本地 SQLite 先记 working state
→ 生成 Result JSON Blob、Completed Tool Step Blob、新 Turn Blob
→ Blob 确认
→ checkpoint(call=completed)
```

Checkpoint 可以记录部分完成的 Tool Batch，但 LLM Projector 仍须等待整批 call/result 完整，不能把部分完成状态投射为下一轮模型请求。这样同时满足单 Tool 回滚与 LLM tool protocol 完整性。

## 20. Blob 确认、重试与 Checkpoint 送达

### 20.1 Blob SET 的确认语义

协议中的写入响应是：

```proto
message KvClientMessage {
  uint32 id = 1;
  oneof message {
    SetBlobResult set_blob_result = 3;
  }
}

message SetBlobResult {
  optional Error error = 1;
}
```

`SetBlobResult {}` 表示成功，带 `error` 表示失败。当前完整 exchange 372 中 154 个 `set_blob_args` 对应 154 个无错误 `set_blob_result`，成功样本没有发现缺失确认。

BlobID 是内容哈希，所以重发同一个 `blob_id + blob_data` 是幂等操作。KV `id` 用于匹配一次尝试；同一 Blob 可以在超时后以新的 KV `id` 重试，迟到或重复结果按 BlobID 合并为已确认状态。

服务端应区分：

```text
Working State：结果已经到达服务端，但客户端 Blob 是否持久化仍可能未知
Committed Checkpoint：只引用已经得到成功确认的 Blob
```

如果某个确认暂时没有返回，不应把它立即判为写入失败，也不能发布引用该 Blob 的 checkpoint。继续保持 working state、重试内容寻址写入，并保留上一个 committed checkpoint。

候选 checkpoint 不必等待与它无关的所有 Blob，只需要满足：

```text
Checkpoint C 可以发布
⇔ C 新增引用闭包中的每个 Blob 都已确认
```

### 20.2 为什么最终 checkpoint 会重复

抓包中 `turn_ended` 后可能先发送一个过渡 checkpoint，随后相同的稳定最终 checkpoint 连续发送两到三次，最后才发送 Connect EndStream。exchange 372 的尾部是 `pending=1` 的过渡 checkpoint，接着两次 `roots=59、pending=0` 的相同最终 checkpoint。正常未断流的 RunSSE 是有序可靠字节流：客户端如果收到了后面的 EndStream，就一定先收到了位于它之前的完整 checkpoint 帧。因此在正常完成路径上，可以断言最终 checkpoint 已经通过 RunSSE 送达客户端，不需要额外 checkpoint ACK 才结束。

需要严格区分“传输送达”和“应用层确认”：协议没有单独的 `checkpoint_ack`。重复帧说明客户端必须幂等接受相同 checkpoint，也增强了尾部发送的稳健性，但仅凭重复本身不能证明断线之后客户端已经持久化了哪一份状态。断流时应以客户端下一次 `run_request` 实际带回的 checkpoint 为恢复事实，并从服务端 SQLite working/outbox 状态继续同步。

## 21. Usage 与 Turn 收口

Cursor 支持细粒度 UI token 增量：

```proto
message TokenDeltaUpdate {
  int32 tokens = 1;
}
```

也支持 Turn 总量：

```proto
message TurnEndedUpdate {
  optional int64 input_tokens = 1;
  optional int64 output_tokens = 2;
  optional int64 cache_read_tokens = 3;
  optional int64 cache_write_tokens = 4;
  optional int64 reasoning_tokens = 5;
}
```

最小实现不需要生成 `token_delta`。权威 usage 只信任各 LLM Adapter 从 provider 最终事件读取到的值：不根据文本 delta 自己 tokenize，不推算 cache token，也不推算 reasoning token。provider 未返回的可选字段保持缺失。

`turn_ended` 表达整个 Cursor Run/Turn 的汇总，而不是一次 provider 调用。exchange 372 的最终值为：

```text
input_tokens       = 786003
output_tokens      = 10819
cache_read_tokens  = 625408
cache_write_tokens = 0
reasoning_tokens   = 5004
```

`input_tokens` 已明显超过单次 256K 上下文，证明它是同一 Run 内多次 LLM 调用的累计值。实现时只对 provider 返回的可信调用总量求和，然后在最终 `turn_ended` 一次汇报。

最终收口顺序建议为：

```text
最终 provider done(stop)
→ 最终 text_delta / step_completed
→ SET 最终 assistant JSON / Step / Turn Blob
→ turn_ended(整轮可信 usage 总量)
→ 等待所引用 Blob 成功确认并发送过渡 checkpoint（如果需要）
→ final checkpoint(pending=0)
→ 幂等重复 final checkpoint
→ Connect EndStream
```

## 22. RunSSE 与 Turn 的结束生命周期

### 22.1 三个不同边界

RunSSE 是 `request_id` 的下行传输，Turn 是 conversation 中一次用户交互的逻辑与持久化对象。正常情况下 RunSSE 包住整个 Turn，但二者不是同一个生命周期：

```text
RunSSE 建立
→ BidiAppend.run_request
→ Turn Active
→ 多轮 LLM / Tool
→ Turn Semantically Ended
→ 状态持久化收口
→ RunSSE EndStream
```

三个结束信号含义不同：

```text
turn_ended      = Loop 的推理和工具阶段结束
final checkpoint = Turn 的最终历史已经可恢复
EndStream       = request_id 对应的 RunSSE 传输结束
```

### 22.2 Turn 的开始与活动阶段

客户端通常先用 `request_id` 建立 RunSSE，服务端可以先发 heartbeat；真正创建或恢复 Turn 的是同 request 的 `BidiAppend.run_request`。抓包没有观察到独立 `turn_started` 事件。新 Turn 由以下事实共同表达：

- `run_request` 携带新的 `user_message_action`；
- 服务端创建 UserMessage 和初始 Turn Blob；
- checkpoint 的 `turns[]` 引用新 Turn。

同一 Turn 内可以有多次 provider 调用和工具等待：

```text
LLM call 0
→ done(toolUse)
→ 客户端执行 Tool Batch
→ Tool results / checkpoint
→ LLM call 1
→ ...
→ LLM call N
→ done(stop)
```

provider 的 `done(toolUse)` 只结束一次模型调用，不结束 Cursor Turn，也不结束 RunSSE。exchange 372 在整个 Run 中 `turns=1` 保持不变，但其 Turn Blob 被不可变新版本逐步替换，最终从 2 个 Step 增长到 66 个 Step。

### 22.3 正常尾部的抓包证据

数据库中 14 条完整 RunSSE 成功样本均只出现一次 `turn_ended`，并且全部遵守：

```text
turn_ended
→ 一个或多个 checkpoint
→ EndStream {}
```

exchange 372 的精确尾部：

```text
frame 3162  step_completed(step_id=66)
frame 3163–3166  SET 最终 Assistant / Step / Turn / model message Blob
frame 3167  turn_ended(input/output/cache/reasoning usage)
frame 3168–3169  heartbeat
frame 3170  过渡 checkpoint：roots=58，pending=1
frame 3171  最终 checkpoint：roots=59，pending=0
frame 3172  重复最终 checkpoint
frame 3173  Connect EndStream {}
```

因此客户端收到 `turn_ended` 后仍必须继续读取 RunSSE。它可以停止“模型生成中”的 UI，但不能在最终 checkpoint 之前关闭流。

成功 EndStream 的条件应为：

```text
provider 已 done(stop)
AND 没有正在执行的 Tool
AND 当前 Tool Batch 已完整
AND 最终历史 Blob 已确认
AND pending_tool_calls 已清空
AND final checkpoint 已发送
```

### 22.4 RunSSE 与 Turn 不是协议上的严格一对一

普通用户消息通常是一个 RunSSE/request 对应一个新 Conversation Turn，但实现不能依赖严格一对一：

- RunSSE 断线重连可能继续同一个未完成 Turn；
- `resume_action` 可以恢复已有状态；
- 用户打断可以结束旧 Run，但旧 Turn 不一定正常 `turn_ended`；
- 新用户消息使用新的 request，并产生新的 Turn。

身份边界：

```text
conversation_id → 多个 Turns
Turn            → 一次用户交互的持久历史
request_id      → 一次 Run/传输尝试
RunSSE          → request_id 的下行通道
```

当前抓包没有 abort/error 尾部，因此异常路径只能作为实现约束：用户打断时取消旧 provider 和尚未继续的工具，提交最后安全 checkpoint，以 canceled/aborted EndStream 结束旧 Run；不能伪造正常成功的 `turn_ended`。单纯的 RunSSE 断线也不等于 Turn 已结束，恢复事实应来自客户端下一次带回的 checkpoint。

## 23. Runtime tag：运行时产生、严格追加一次

### 23.1 角色与来源必须分离

Runtime tag 通常是 `<system_reminder>`、当前模式提醒、最新编辑保护、调试会话信息等。provider 端通常需要把它作为 `role=user` 消息发送，但它不是用户输入：

```rust
enum MessageOrigin {
    User,
    Runtime { kind: RuntimeTagKind },
    Assistant,
    Tool,
}
```

Runtime message 的约束：

- 投射给 LLM 时使用 user role；
- 内部 `origin=runtime`，不能冒充真实用户；
- 不创建新的 UserMessage action；
- 不开启新的 Conversation Turn；
- 不在 UI 中显示为用户发送的正文；
- 可以作为模型 transcript 中的不可变 message Blob 被 checkpoint 引用。

### 23.2 每个 Runtime event 恰好追加一次

Runtime tag 是实时产生的，但不是每次编译 prompt 时重新生成的临时后缀。正确流程：

```text
产生 runtime event
→ 创建一个 runtime user-role message
→ 原子追加到 messages
→ 标记该 runtime event 已消费
→ 调用 LLM
```

后续 provider 重试、下一轮 LLM、服务重启恢复都只能重放已有 message：

```text
首次：messages.push(runtime_tag)
以后：replay(messages)
禁止再次 push(runtime_tag)
```

建议使用稳定事件身份保证 exactly-once append：

```text
UNIQUE(conversation_id, runtime_event_id)
```

或由 `(conversation_id, runtime_sequence)` 形成唯一键。追加 message 和消费 runtime event 必须在同一个 SQLite 事务中完成。不能只按文本内容去重；决定是否追加的是新的业务事件/状态转换，而不是本轮又执行了一次 prompt 编译。

### 23.3 Runtime tag 与前缀缓存

假设 `runtime_1` 在请求 N 前首次产生：

```text
请求 N:   [A, B, C, runtime_1]
请求 N+1: [A, B, C, runtime_1, D, runtime_2]
```

`runtime_1` 在 N+1 中必须位于原位置且字节不变。新提醒只追加在末尾，因此：

```text
M(n+1) = M(n) || Δmessages
```

旧提醒不需要删除或改写。新状态产生的新 tag 位于更靠近结尾的位置，LLM 对尾部信息具有更高注意力，应以最后出现的相关状态为当前事实。通过追加解决状态变化，而不是回写历史；这既保留语义，又保留 provider 前缀缓存。

## 24. Todo 与 Plan：从 Messages 推导的业务投影

Todo、当前 Plan 等是 conversation 的当前业务状态，但不需要独立的权威存储。它们由不可变、有序的 message/tool result 历史确定性 fold 得到：

```text
ordered messages / tool results
→ deterministic reducer
→ DerivedConversationState {
     todos,
     current_plan,
   }
```

典型来源：

```text
TodoWrite 成功结果  → 更新 todos 投影
CreatePlan 参数/成功结果 → 更新 current_plan 投影
后续相关 message   → 以最后一次状态转换为准
```

关键不变量：

- messages/tool results 是唯一事实源；
- 相同有序历史必须得到相同 Todo/Plan；
- 不维护一套可能与 messages 分叉的可变 `runtime_state`；
- checkpoint 中的 `todos`、`plan`、`plans` 可以为客户端 UI 填充，但只是派生视图；
- 重启或回滚后从 Blob 图中的 messages 重新 fold，即可恢复当前业务状态；
- 旧 Todo/Plan 状态不从历史删除，最新状态因位于尾部而成为当前事实。

这样 messages 投射到 LLM、checkpoint 投射到 Cursor UI、服务端恢复三条路径共享同一来源，且天然幂等。

## 25. 多模式 Prompt 与 Tool 资产

`main` 分支已有完整的静态 prompt、模式工具定义和 reminder 模板。已将其中 18 个语言无关资产原样复制到当前分支的 `prompt/`：

```text
prompt/
├─ common_prefix.md
├─ agent/       prompt.md + tools.json
├─ ask/         prompt.md + tools.json
├─ plan/        prompt.md + tools.json + system_reminder.txt
├─ debug/       prompt.md + tools.json + initial/continuing reminder
├─ multitask/   prompt.md + tools.json
├─ subagent/    prompt.md + tools.json
├─ compaction/  prompt.md
└─ commit/      prompt.md
```

工具数量：

| 模式 | Tools |
| --- | ---: |
| Agent | 21 |
| Ask | 19 |
| Plan | 17 |
| Debug | 19 |
| Multitask | 21 |
| Subagent | 4 |

Rust 服务端应在启动时加载、解析并校验这些资产：

```rust
struct ModeAssets {
    system_prompt: Arc<str>,
    tools: Arc<[ToolDefinition]>,
    runtime_reminders: Arc<[RuntimeTemplate]>,
}
```

模式映射：

```text
AGENT_MODE_AGENT      → common prefix + agent prompt/tools
AGENT_MODE_ASK        → common prefix + ask prompt/tools
AGENT_MODE_PLAN       → common prefix + plan prompt/tools/reminder
AGENT_MODE_DEBUG      → debug prompt/tools + initial/continuing reminder
AGENT_MODE_MULTITASK  → common prefix + multitask prompt/tools
子 Agent conversation → subagent prompt +受限 tools
```

静态 prompt 和工具目录按 mode 选择；运行时 reminder 必须遵守第 23 节的 exactly-once append，不能因为每轮加载同一个模板而重复加入 messages。模式或工具集合切换可以形成新的 provider cache 边界，但已提交的模型 messages 仍然保持严格只追加。




----

现在已经足够实现一个端到端可运行的服务核心。
核心闭环已经明确：
BidiAppend.run_request
→ 加载 checkpoint / Blob 图
→ 编译 append-only messages
→ 选择 mode prompt + tools
→ 调用 LLM
→ 投射 RunSSE 流事件
→ 客户端执行 Tool
→ BidiAppend 返回结果
→ 单 Tool checkpoint
→ 下一轮 LLM
→ turn_ended
→ final checkpoint
→ EndStream
必须坚持的核心不变量也已经齐全：
Messages 是 LLM 上下文和 Todo/Plan 的唯一事实源。
历史严格只追加，下一次请求保持旧请求的完整前缀。
Runtime event 恰好追加一个 runtime-origin/user-role message。
Provider 重试只重放，不能重复追加任何 message。
Tool call/result 必须一对一完整，不能向 LLM 投射悬空调用。
Tool 可以乱序完成，但下一轮 LLM 必须等待整个 Tool Batch 完整。
每个 Tool 单独持久化和 checkpoint，避免有副作用工具被重复执行。
Blob 先确认，checkpoint 后发布。
turn_ended、final checkpoint、EndStream 是三个独立边界。
Usage 只信任 provider，最终按整个 Turn 汇总。
旧 Run 的迟到事件不能更新新 conversation revision。
Todo/Plan 由 messages 确定性 fold，不维护第二份状态。
建议按最小闭环分层实现：
第一层：SQLite + Blob CAS + append-only messages
第二层：RunSSE/BidiAppend actor 与 append_seqno
第三层：单 provider Adapter + 文本响应
第四层：Tool start/exec/result + 下一轮 LLM
第五层：checkpoint + turn_ended + interrupt/recovery
第六层：多 provider、MCP、subagent、skills 和全部模式
第一个可验收版本只需要做到：
真实用户消息
→ LLM 返回一个客户端工具调用
→ Cursor 显示占位卡片并执行
→ 结果返回服务端
→ 第二次 LLM 调用
→ 最终文本
→ 客户端可重启并从 checkpoint 恢复
剩余未知项，如个别低频 Tool variant、断线重连的重复次数，都不阻塞核心实现，可以在已有闭环上逐层补齐。异常 EndStream 的精确错误形状和生命周期已经在第 26 节确认。

## 26. RunSSE 结构化错误与终结生命周期

### 26.1 错误不属于 AgentServerMessage

`agent.v1.AgentServerMessage` 的 oneof 只有：

```text
interaction_update
exec_server_message
exec_server_control_message
conversation_checkpoint_update
kv_server_message
interaction_query
```

它没有通用的 run error variant。`InteractionUpdate` 也只有 text、thinking、tool、usage、`turn_ended` 等业务事件；`TurnEndedUpdate` 只包含 usage，没有失败状态或错误字段。

因此 provider、协议或服务内部错误不能伪装成 assistant `TextDelta`。否则 Cursor 会把服务错误当成模型正文渲染，并可能进一步写入会话上下文。

### 26.2 错误是 Connect EndStreamResponse

RunSSE 是 Connect 流式 RPC。流建立后无论成功或失败，HTTP 响应都是 200；RPC 的最终结果由最后一个 Connect envelope 表达：

```text
+------------+----------------------+-------------------------------+
| flags: u8  | length: u32 big-end  | payload: UTF-8 JSON           |
+------------+----------------------+-------------------------------+
| 0x02       | JSON 字节长度        | EndStreamResponse             |
+------------+----------------------+-------------------------------+
```

成功 payload：

```json
{}
```

失败 payload：

```json
{
  "error": {
    "code": "unavailable",
    "message": "provider error",
    "details": [
      {
        "type": "aiserver.v1.ErrorDetails",
        "value": "<无 padding 的 base64 protobuf>"
      }
    ]
  }
}
```

关键点：即使普通消息使用 protobuf，`EndStreamResponse` 的 payload 仍然是 JSON。必须设置 `0x02`；如果错误 JSON 使用普通消息标志 `0x00`，Cursor 会把 JSON 当 `AgentServerMessage` protobuf 解码，可能得到 `invalid wire type`。

错误 EndStream 必须是流中最后一个 envelope；发送之后立刻关闭该 RunSSE 输出。BidiAppend 只负责有序接收并 ACK `run_request`、KV/Exec 结果等上行消息。异步 provider 错误发生在 BidiAppend 已成功返回之后，只能通过配对的 RunSSE 终结，不能再从 BidiAppend 返回。

### 26.3 Cursor ErrorDetails

Cursor 使用 `aiserver.v1.ErrorDetails` 为 Connect error 附加可渲染、可判断重试的结构化信息：

```text
ErrorDetails
├─ error
├─ details: CustomErrorDetails
│  ├─ title
│  ├─ detail
│  ├─ is_retryable
│  ├─ show_request_id
│  └─ should_show_immediate_error
└─ is_expected
```

`main` 分支已有 provider error 的实现，其字段为：

```text
Connect code                 = unavailable
ErrorDetails.error           = ERROR_PROVIDER_ERROR
CustomErrorDetails.title     = "Server Error"
CustomErrorDetails.detail    = 原始错误文本
is_retryable                 = true
show_request_id              = true
should_show_immediate_error  = false
is_expected                  = false
```

`details[].value` 是 `ErrorDetails` protobuf 的标准 base64、无 `=` padding 编码；`debug` JSON 是可选调试信息，客户端不能依赖它。`should_show_immediate_error=false` 用于避免立即弹出全局错误提示，不会把结构化错误降级为 assistant 文本；Composer 仍可根据 ErrorDetails 展示内联错误和重试入口。

### 26.4 成功、失败、取消三条生命周期

成功路径：

```text
业务消息
→ Blob SET / ACK barrier
→ turn_ended + final checkpoint
→ EndStream {}
→ 关闭 RunSSE 输出
```

真实抓包的客户端可见尾序列是 `turn_ended → 重复 final checkpoint → EndStream {}`。`main` 分支现有 Go 实现是 `Blob ACK → checkpoint → turn_ended → EndStream {}`；两者的最终语义相同，但 Rust 的协议兼容测试应固定所采用的客户端可见顺序。

Provider 失败路径：

```text
停止 provider
→ 保存已经收到并确认的部分 assistant 输出
→ 保存 provider 已汇报的 usage 与失败元数据
→ 从当前已提交 messages 构造 checkpoint
→ Blob SET / ACK barrier
→ 发布 checkpoint
→ Error EndStream
→ 关闭 RunSSE 输出
```

失败路径不发送 `turn_ended`，不发送错误 `TextDelta`，也不把错误字符串追加为 assistant message。已经作为正常 provider delta 发出的部分内容可以保留；错误本身只存在于 run 元数据和 Connect error 中。若在 checkpoint Blob 同步失败时采用超时策略，可以跳过未获确认的 checkpoint，但仍必须发送 Error EndStream，不能让流永久悬挂。

用户取消或新 Run 打断旧 Run：

```text
取消 provider
→ 对活动客户端 Exec 逐个发送 ExecServerAbort
→ 丢弃尚未发布的 checkpoint，并忽略迟到 ACK
→ EndStream error(code = canceled)
→ 关闭旧 RunSSE 输出
```

取消不发送 `turn_ended`，也不发布一个代表成功完成的新 checkpoint。Cursor 对 Connect `canceled` 有专门处理，不应将它显示为普通错误。已在更早的单 Tool checkpoint 中确认的副作用和消息保持有效；未完成工具不能投射进下一轮 LLM。

### 26.5 统一终结不变量

服务端应只有三个显式终结入口：

```text
finish_success()
finish_error(ConnectError + ErrorDetails)
finish_canceled()
```

它们共同保证：

- run 终态只提交一次；
- EndStream 是最后一帧；
- 成功仅使用 `{}`，失败必须包含 `error`；
- error/canceled 不发送 `turn_ended`；
- 终结后关闭输出 channel，使 HTTP body 和 RunSSE 订阅真正结束；
- 终态 backlog 可供同一 request_id 重连回放，但不能继续接受新的业务输出；
- 新 Run 打断旧 Run 时，旧 Run 的迟到 provider、KV、Exec 事件不能污染新 revision。

### 26.6 运行期 Protocol 错误的回报和日志

`Protocol` 不只表示 HTTP 请求刚进入时的解码错误，也可能在 Run 已经建立后发生。修复 Exec 关联前曾实际观测到：

```text
Protocol("unknown tool result call_id: ")
```

这是 RunSSE 流建立后的运行期错误，不能再通过 BidiAppend 的 HTTP 响应回报，也不能发成 assistant `TextDelta`。固定生命周期为：

```text
Loop 返回 Error::Protocol
→ runs.status = failed
→ 服务端输出 error 日志（必须包含 request_id 和完整错误）
→ RunSSE 发送 Connect Error EndStream
   code = invalid_argument
   message = "protocol error: ..."
→ 关闭 RunSSE 输出
```

该路径不发送 `turn_ended`，不把错误追加到 messages，也不用普通 protobuf 帧承载错误 JSON。错误必须使用 `flags=0x02` 的 Connect EndStreamResponse，否则 Cursor 会将 JSON 当成 `AgentServerMessage` 解码。

日志是服务端定位根因的依据，Connect error 是客户端可见的协议结果，两者必须同时发生。即使更新 run 终态或编码终结帧再次失败，原始运行期错误也必须已经被记录。

### 26.7 Exec ID 的作用域和内存关联

Exec 的数字 `id` 由服务端在 RunSSE `ExecServerMessage.id` 中分配，客户端在 BidiAppend 的 `ExecClientMessage.id`、`ExecClientStreamClose.id` 和 `ExecClientThrow.id` 中回传。`exec_id` 不是结果关联键：已分析的 156 条 `ExecClientMessage` 中没有一条回传 `exec_id`。

抓包中的作用域为：

```text
(request_id, 消息族, id)
```

`id` 不全局唯一，也不是 `(conversation_id, id)` 唯一。同一 conversation 的不同 request 会重新从 `id=1` 开始。一个长 request 的样本则按 `1..41` 分配 Exec ID，跨越多轮 LLM 工具批次而不重置。同一 `id` 可以出现在 start、多个 stdout/stderr、exit/result 和最后的 stream_close 中；它唯一标识一次 Exec，不唯一标识一个上行包。

因为 `RunRegistry` 已先按 `request_id` 将 BidiAppend 路由到唯一 `RunActor`，Actor 内只需要数字 `id` 作为 HashMap key：

```text
RunRegistry
└─ request_id → RunActor
                 └─ PendingExecRegistry
                     └─ id → PendingExec
                              ├─ call_id
                              ├─ state: Running | ResultReceived
                              ├─ stdout
                              └─ stderr
```

下发 Exec 前在内存中建立 `id → call_id`，客户端结果到达时用 `message.id` O(1) 查找，不查 SQLite。数字 ID 在整个 request 内单调递增，条目在 result/exit 到达后标记为 `ResultReceived`，在随后的 `stream_close` 到达时删除。Run 取消或失败时对仍为 `Running` 的 ID 发送 abort，然后清空全部条目。

这个映射是运行期协议状态，不是上下文事实源。SQLite 只保存已经关联成功的 `run_tool_results` 和 checkpoint，不参与每个 Shell 流片段的实时查找。

客户端会在 result/exit 之后紧接着发送 `stream_close`。因此 `run_tool_results` 的持久化不能使用“事务内先 SELECT completion_seq，再将 deferred transaction 升级为写事务”的方式，它会和 Bidi `append_seqno` 的并发更新产生 `SQLITE_BUSY_SNAPSHOT`。completion_seq 的计算和 ToolResult 插入必须合并为单条原子 `INSERT ... SELECT`。

### 26.8 ToolResult 向 LLM 的字符串投射

Canonical `ToolResult.output` 允许保存任意 JSON Value，因为 Todo/Plan fold、checkpoint 和调试都需要保留工具结果的结构。但是投射到 LLM 请求时，ToolResult content 必须始终是字符串，不能把 JSON object、array、number、boolean 或 null 直接放入 message content。这是所有 provider adapter 的共同输入不变量，不应由 OpenAI Chat、Responses 或 Anthropic 各自补救。

已观测的失败是 `TodoWrite` 将对象结果持久化后，projector 直接生成：

```json
{
  "role": "tool",
  "content": { "merge": false, "todos": [] }
}
```

OpenAI Chat 因此拒绝 `messages[7]`，报错 `content should be a string or a list`。正确投射为：

```json
{
  "role": "tool",
  "content": "{\"merge\":false,\"todos\":[]}"
}
```

统一规则：

```text
output 是 JSON string → 直接使用原字符串，不二次加引号
output 是其他 JSON 类型 → serde_json::to_string(output)
最终 ProviderMessage.content → 始终 Value::String
```

字符串化只发生在 `CanonicalMessage → ProviderMessage` 边界；SQLite、Blob 和 derived state 仍保留原始结构化 JSON。这样既满足 LLM 端点约束，又不破坏幂等状态投影。

### 26.9 Thinking 历史的端点投射

Canonical assistant message 将可见 `text` 和模型 `thinking` 分开保存。两者不能在公共 projector 中拼成一个 `content`：这会改变可见文本的语义，并且丢失端点要求的 reasoning 字段。

已观测的 OpenAI Chat 失败发生在工具批次后的第二轮 LLM 请求：第一轮返回了 thinking、assistant text 和 tool call，ToolResult 也已成功持久化；但历史 assistant message 只回传了拼接后的 `content`，上游因此拒绝请求：

```text
The `reasoning_content` in the thinking mode must be passed back to the API.
```

公共投射必须保持中性结构：

```text
ProviderMessage
├─ content  = assistant text
└─ thinking = assistant thinking
```

具体 provider adapter 再负责端点字段映射。OpenAI Chat 必须生成：

```json
{
  "role": "assistant",
  "content": "可见回答",
  "reasoning_content": "原始 thinking",
  "tool_calls": []
}
```

DeepSeek 官方文档进一步明确了这里不是“字段存在即可”的校验：

- 未调用工具的 assistant thinking，在后续请求中可以不回传；即使回传也会被忽略。
- 只要 assistant 调用了工具，该次模型响应的 `reasoning_content` 就必须完整、原样参与后续请求。
- 官方示例直接追加完整的 `response.choices[0].message`，即同一条 assistant message 同时包含 `content`、`reasoning_content` 和该次响应的全部 `tool_calls`。
- 用 `reasoning_content: ""` 给拆分出的 assistant tool-call message 补字段不是正确修复；它仍然丢失了原始思维内容。

官方说明：[DeepSeek 思考模式与工具调用](https://api-docs.deepseek.com/zh-cn/guides/thinking_mode)。

这暴露出两个不同视图，不能混成一个数据形状：

```text
Cursor 持久化与 checkpoint 视图
assistant(call 1) → tool result 1 → assistant(call 2) → tool result 2
                  单工具完成、单工具 checkpoint

LLM provider 请求视图
assistant(
  content,
  完整 reasoning_content,
  tool_calls = [call 1, call 2]
)
→ tool result 1
→ tool result 2
```

服务端继续按已完成工具保存 1:1 pair，因此中断时不会把尚未得到结果的 tool call 投射给下一次 LLM。每条 canonical assistant tool message 额外保存：

```text
model_call_id   同一次 provider 响应的分组键
tool.index      provider 返回的原始 tool-call 顺序
```

公共 projector 在编译模型请求时，按 `model_call_id` 合并同一响应的 assistant pair，取唯一的非空 `text` 和完整 `thinking`，按 `tool.index` 恢复全部 tool calls，再按同一顺序投射 ToolResult。这样 SQLite/Blob 与 Cursor 仍保留单工具粒度，而 OpenAI Chat 端点看到的是其要求的原始 assistant 响应形状。

关键不变量：

```text
不复制 reasoning_content
不以空字符串替代原始 reasoning_content
不依赖工具结果抵达顺序
ToolBatch 未完整时不发起下一轮 LLM 请求
完成后的 provider messages 中不存在悬空 tool_calls
```

普通模型从未返回 thinking 时，请求形状不变。OpenAI Responses 和 Anthropic 不能直接复用 `reasoning_content` 字段，应由各自 adapter 按端点原生结构处理；公共层不将 thinking 降级为普通文本。

旧版本已写入的 assistant tool pair 没有 `model_call_id` 和 `tool.index`，无法无歧义恢复原始 provider 响应。服务端不猜测旧分组；验证本修复应新建对话。新数据不需要额外迁移。

### 26.10 Tool 完成事件与 UI 生命周期

最新对话中 ToolBatch 并没有越过工具结果继续调用 LLM：27 个 tool call 均找到了对应结果，Shell 也确实等待到 exit/abort。UI 中多个工具长期显示 loading 的根因在下行完成协议，而不是 Loop barrier。

抓包中的 `ToolCallCompletedUpdate.tool_call` 不只是“同一份 args 加 completed 标记”，它还包含：

```text
ToolCallCompletedUpdate
└─ tool_call
   ├─ started_at_ms       真实开始时间
   ├─ completed_at_ms     真实完成时间
   ├─ args                原始工具参数
   └─ result              对应工具的 typed protobuf result
```

旧实现调用 `render_tool_call(call, true)`，只填 args，并将两个时间固定为 `1`；`ShellToolCall.result`、`ReadToolCall.result`、`LsToolCall.result` 等始终为 `None`。服务端内部虽然已经消费结果、写入 messages 并继续 Loop，Cursor UI reducer 却没有收到可将卡片归约到终态的数据，因此卡片继续 loading。

修复后的结果在同一个完成值中同时建立两个视图：

```text
客户端 typed result → ToolCompletion
                       ├─ canonical ToolResult
                       │  → messages / checkpoint / 下一轮 LLM
                       └─ 完整 typed ToolCall
                          → ToolCallCompletedUpdate（UI 终态）
```

关键规则：

- `ExecClientMessage.message = None` 不是完成结果，只表示尚无载荷；必须保持 Pending，随后等待 typed result、Shell exit、throw 或异常 stream close。
- `PendingExecRegistry` 在下发 Exec 时保存完整 `ToolCall`、真实 `started_at_ms` 和 Shell 流缓冲；数字 ID 只在当前 Run 内用于关联。
- Shell、Delete、Grep、Ls、ReadMcpResource、WriteShellStdin 等可直接复用上行 typed result；Read、Write、Diagnostics、MCP、Subagent、PiEdit 按 Cursor ToolCall 所需结果类型做无损或语义等价转换。
- `TodoWrite` 这类服务端本地工具必须构造明确 typed success。`ExecClientThrow` 不是工具结果，直接进入统一 Error 生命周期，不伪造成某个 typed result。
- Canonical `ToolResult` 继续用于 LLM 和持久化；不能用它替代 UI 所需的 typed protobuf result。
- 成败不能通过完整 protobuf `Debug` 字符串搜索 `Error`、`Failure` 等单词判断。成功写入的文件内容可能恰好包含这些词，从而产生假失败。已知 oneof 必须按具体 success/error variant 判定。
- typed terminal result 通过 `take(id)` 原子取得并删除 PendingExec；随后到达的正常 `stream_close` 无事可做。若 Running 状态先收到 close，则同样 `take(id)`，并报告 `Exec stream closed before result` 协议错误。

因此一次正常工具生命周期固定为：

```text
ToolCallStarted(args, started_at_ms)
→ ExecServerMessage(id)
→ ExecClientMessage(id, typed result) / Shell exit
├→ take(id) 消费 PendingExec 的唯一所有权
└→ 同时生成 Canonical ToolResult 与完整 typed ToolCall
→ Blob 存储确认与单 Tool checkpoint
→ ToolCallCompleted(args + typed result + timestamps)
→ ToolBatch 全部完整后进入下一轮 LLM
```

客户端通常在 typed result 后立即发送 `stream_close`。终态 result 已经消费 PendingExec，因此 close 只是幂等尾包；不能再维护额外的 Finished/Closed 状态。`ToolCallCompleted` 必须在该工具的消息与 checkpoint 已经提交后发布。

### 26.11 Tool completion 的模块边界

首次实现虽然修正了协议，但将 pending registry、结果通道和 typed protobuf 转换同时塞进了 `run/tool_batch.rs`、`cursor/exec.rs` 与 `cursor/interaction.rs`，使运行期关联、Exec 解析、UI 投射互相穿插。整理后的职责为：

```text
cursor/pending.rs
└─ id → ToolCall/timing/stream buffer；存在即 Running，take 即终态

cursor/tools.rs
└─ 工具批次的 Cursor step index、唯一 transport 和本地立即完成工具

cursor/exec.rs
└─ 解析 ExecClientMessage/ShellStream，产生 ToolCompletion

cursor/tool_result.rs
├─ ToolCompletion 与结果 channel
├─ typed Exec/Interaction result → canonical 字符串结果 + typed ToolCall
└─ 本地 TodoWrite/CommunicateUpdate 的明确终态

cursor/interaction.rs
└─ text/thinking/tool started/completed/usage 的事件外壳与 args 渲染

run/loop_engine.rs
└─ 只决定何时持久化、checkpoint、发布 completion 和进入下一轮
```

原 `run/tool_batch.rs` 和 `model::ToolBatch` 均已删除。Loop 已经持有有序 `ordered_calls`，只需一个 `completed call_id` 集合判断 barrier；再复制一份 calls/results 容器没有增加信息。Cursor 数字 ID、protobuf typed result 和 UI 卡片状态也不再伪装成 Loop 领域状态。

`ToolCompletion` 对 Loop 是一个需要延迟发布的不透明完成信封：Loop 读取其中的 canonical `ToolResult` 完成消息和 checkpoint，Cursor adapter 读取 presentation payload 生成 UI typed result。Loop 不匹配 protobuf oneof，也不决定某种工具在 Cursor UI 中如何展示。

### 26.12 自然状态与无 fallback 约束

本轮整理删除了几类会掩盖协议错误的级联和猜测：

- 工具不会再依次尝试 `Exec → Interaction → Local`。名称在 `cursor/tools.rs` 映射到唯一 transport，Loop 不持有 `ToolRoute`；未知工具立即报 Protocol error。动态 MCP 只有在本轮定义表中存在时才走 Exec。
- Pending 项不存在 `Running/Finished/Closed` 并行标志。存在于 map 就是 Running；terminal result、throw、提前 close 都通过 `take(id)` 消费唯一所有权。
- `ToolCompletion` 不允许只有 canonical result、没有 UI presentation 的半成品。构造成功即同时拥有可稳定投射给 LLM 的结果与完整 typed ToolCall；结构化本地结果只在 provider 边界字符串化。
- `ToolCall.name` 与 `ExecClientMessage` oneof 必须精确匹配。Read 对上 WriteResult 等组合直接报 Protocol error，且已经消费该 terminal ID，不能继续复用。
- 不再通过 protobuf `Debug` 文本生成 tool result 或判断成败；每个支持的 oneof 都显式读取。
- LLM 返回的工具参数必须是合法 JSON；不再在解析失败时降级成普通字符串。
- BidiAppend 按抓包协议只接受 `data` 的 hex 编码；不再猜测 base64、原始字符串或 `data_binary`。
- prompt 启动时使用完整的编译期嵌入资产。显式 `PromptAssets::load(path)` 则只读取该目录；两种来源不能逐文件混合。`tools.json` 只接受仓库当前的 OpenAI function 数组格式，不兼容历史别名。
- 三个 provider adapter 分别校验各自事件的必需字段；缺少 tool `index/id/name`、未知 finish reason 或非法历史 tool call 时立即返回 Provider/Protocol error，不合成默认值。

这不是“更严格但更复杂”。相反，核心状态只剩下三条直线：

```text
tool name → 唯一 transport
pending id → take → ToolCompletion
ToolCompletion → persist/checkpoint → publish
```

默认值只保留在协议本身定义为 optional 的字段上；它不能用来掩盖缺少必需字段、未知消息类型或不匹配的生命周期。

### 26.13 Tool 无关边界与特殊生命周期

本节只使用 SQLite 抓包与 `agent_v1.proto`，不引用其他分支实现。

结论不是“所有工具使用完全相同的代码”，而是把工具差异限制在 Cursor 协议适配器：

```text
Loop
└─ start_batch(calls) → ToolCompletion
   ├─ 不认识 Read、Shell、Task 等名称
   ├─ 不匹配 protobuf oneof
   └─ 只负责 persist → checkpoint → completed → batch barrier

Cursor tool dispatcher
├─ tool name → 唯一 Exec / Interaction / Local transport
├─ request/result typed oneof 转换
└─ 仅为协议明确表现为多阶段的工具维护阶段状态
```

抓包中的 ToolCallStarted 类型为 `Read ×14、CommunicateUpdate ×6、Shell ×2、Grep ×2、Glob ×2、Task ×1`。其中：

- `Read`、`Grep` 是普通一次性 Exec。
- `Glob` 的 UI 是 `glob_tool_call`，实际执行却使用 `grep_args / grep_result`。它需要特殊 codec，但不需要特殊 Loop。
- Shell 抓到 `23 start、27 stdout、23 exit`。`start/stdout/stderr/hook_context` 不是终态；`exit/backgrounded/rejected/permission_denied/sandbox_unsupported` 才能消费 PendingExec。
- `CommunicateUpdate` 的 `started` 与 `completed` 相邻，中间没有 Exec 或 Interaction。它是服务端本地工具。
- Task 返回 `agent_id` 与 `background_reason` 后，父 Tool 即完成；子代理随后通过独立 conversation/RunSSE 继续。父 Loop 不等待子 Run 结束。
- `request_context` 和 `execute_hook` 虽然也使用 ExecServer/ClientMessage，但不是 LLM Tool，不能追加 assistant/tool pair。

`AwaitShell` 不是该协议中的工具。proto 中的 `AwaitToolCall / SubagentAwaitArgs` 属于 Task/Subagent 语义，不能因为字段形状相近就把 `shell_id` 填入 `agent_id`。服务端不暴露 `AwaitShell`，也不保留该错误映射；后台 Shell 只使用实际存在的 Shell、ForceBackgroundShell 与 WriteShellStdin 消息。

`CommunicateUpdateSuccess.message_index` 是当前 Turn 中该 ToolCall 对应的 `ConversationStep` 一基位置，不是本地调用次数。抓包第一组事件依次产生 thinking、assistant text、CommunicateUpdate，因此结果为 `message_index = 3`；同一 Turn 后续样本累计为 `6`。服务端按已提交步骤数、当前 thinking/text 和本批 call 位置确定该值。

proto 还明确区分了三种 transport：

- `ExecServerMessage / ExecClientMessage`：文件、搜索、Shell、MCP、Subagent 等客户端执行型操作。
- `InteractionQuery / InteractionResponse`：AskQuestion、CreatePlan、SwitchMode、WebSearch、WebFetch、GenerateImage 等用户交互。
- 没有 Exec/Interaction variant 的 ToolCall，例如 TodoWrite、CommunicateUpdate，只能在服务端直接形成 typed result。

InteractionResponse 不能一律视为 ToolResult。AskQuestion、CreatePlan 和 SwitchMode 的 response 自身包含可结束工具的结果；WebSearch、WebFetch、GenerateImage 的 response 只有 `approved/rejected`。其中 rejection 可以形成 typed terminal result，approval 只是允许服务端继续执行，不能写入 messages 或发布 ToolCallCompleted。

WebFetch 的第二阶段可以由 proto 无歧义确定：approval 后将同一个 canonical ToolCall 从 PendingInteraction 转移到 PendingExec，下发 `FetchArgs(url, tool_call_id)`；客户端返回 `FetchResult` 后再转换为 `WebFetchResult` 和 canonical 字符串结果。该转移不创建第二个 LLM ToolCall，也不提前 checkpoint。WebSearch 和 GenerateImage 没有对应的 Cursor Exec variant，抓包也没有给出服务端执行端点；它们被批准时明确进入 Protocol Error，而不是伪造结果、保持 loading 或 fallback 到其他 transport。

实现后的固定不变量为：

```text
Cursor adapter 识别 typed terminal
→ ToolCompletion(canonical result + typed UI result)
→ Loop 统一持久化
→ Blob ACK barrier
→ 单 Tool checkpoint
→ ToolCallCompleted
→ 整批完整后下一轮 LLM
```

具体落点：

- `cursor/tools.rs` 独占工具路由和本地工具启动，并计算 Cursor step index。
- `cursor/exec.rs` 独占 Shell 流阶段与 Exec wire event。
- `cursor/tool_result.rs` 独占 typed result 到 `ToolCompletion` 的转换。
- `run/loop_engine.rs` 不再包含 `ToolRoute` 或工具名称表。
- 未知工具与不匹配 oneof 立即返回 Protocol Error；不尝试 Exec → Interaction → Local fallback。

## 27. Run 进度观测：provider_call_index 必须随 Loop 更新

对运行中的 `4468e12f-4f90-4bd9-90ed-d57c9c2bc7a9` 复核后，最初看到的状态并不是卡在第一个 Ls：Ls 的 typed result、messages 和 checkpoint 都已经完成，Run 随后继续完成 Write、ReadLints、Shell 等调用，最终形成 8 轮 provider call 并正常结束。此前数据库始终显示 `provider_call_index = 0`，只是该字段从未被 Loop 更新，因而给出了错误的观测结果。

`append_seqno` 只表示 BidiAppend 上行序号推进，不能回答当前正在执行第几轮 LLM。`run_tool_results` 在批次完成后会被清除，outbox ACK 也只能说明 checkpoint 已确认；它们都不能替代 provider 调用进度。

固定规则为：

```text
进入第 N 轮 Loop
→ 编译确定性的 ModelRequest
→ 持久化 runs.provider_call_index = N
→ 记录 provider call started
→ 消费 provider stream
→ 记录 provider call completed(finish/tool_count/usage)
→ Tool 或 Turn 后续生命周期
```

持久化必须发生在发起 HTTP 请求之前。这样进程在请求中挂起、超时或崩溃时，SQLite 仍能指出准确的当前轮次。索引采用与 `model_call_id` 一致的零基语义：第一次为 `0`，第二次为 `1`。

结构化日志包含 `request_id、conversation_id、call_index、model、message_count`；完成日志再包含 `finish、tool_count、input_tokens、output_tokens`。这能直接区分“等待 provider”与“从未进入下一轮”，无需增加猜测性的 fallback 或复制一套 Run 状态机。

工具完成 wire 不因此改变。当前实现的 `ToolCallCompletedUpdate` 已与 proto 和抓包一致，包含 `call_id、model_call_id、typed ToolCall、真实 result、started_at_ms、completed_at_ms`；不能因为旧的进度字段错误而再次修改工具协议。

## 28. ThinkingCompleted 与思考耗时

此前服务端虽然在 `ThinkingEnd` 时发送了 `ThinkingCompletedUpdate`，但把 `thinking_duration_ms` 固定为 `0`，等价于没有实现耗时。抓包中的消息结构只有一个字段：

```proto
message ThinkingCompletedUpdate {
  int32 thinking_duration_ms = 1;
}
```

抓包时序稳定为：

```text
thinking_delta × N
→ thinking_completed(thinking_duration_ms >= 1)
→ text_delta / partial_tool_call / 其他下一阶段事件
```

样本既有 `15295、6520、3103、884ms`，也有流数据集中到达时的 `1、2、3ms`。因此不能用 token 数估算，也不能把整轮 provider 请求时间当作思考时间。

当前实现以每轮 provider stream 中的 `ThinkingStart` 为起点，以 `ThinkingEnd` 为终点，使用单调时钟 `Instant` 独立测量每个思考段。耗时转换为 proto 的 `int32` 时限制在 `1..=i32::MAX`；极快的测试流和同批到达事件也发送 `1ms`，不再出现无意义的 `0`。

`ThinkingDeltaUpdate.thinking_style` 同时按抓包填写 `THINKING_STYLE_DEFAULT`。状态约束为：

- `ThinkingStart` 不能在已有活跃思考段时重复出现。
- `ThinkingDelta` 必须位于 Start 与 End 之间。
- `ThinkingEnd` 必须消费唯一的开始时间，并立即发送 `ThinkingCompleted`。
- provider 在思考中报错时，先用已经经过的时间关闭思考段，再进入 checkpoint 和统一 Error 生命周期，避免 UI 保持 thinking 状态。

思考正文仍按原样累计到 canonical assistant message，并由 provider projector 在下一轮回传；计时只服务 Cursor UI 生命周期，不进入 messages，也不影响 LLM 前缀缓存。

## 29. Shell 前台输出与后台进程

`request_id = 797286ed-9c81-40f2-b8ed-ed5770f9ff77` 的失败不是 Python 不存在。数据库时间线为：

```text
Shell("python3 -m http.server 8000", block_until_ms=3000)
→ shell completed without output, is_error=true
→ curl localhost:8000
→ HTTP 000
→ which python3 && python3 --version
→ /Users/leokun/.pyenv/shims/python3, Python 3.12.12
```

根因是旧实现读取不存在的 `timeout` 和 `is_background` 模型参数，却忽略 prompt 工具定义中的 `block_until_ms`。因此发给 Cursor 的 `ShellArgs` 实际是 `timeout=0、TIMEOUT_BEHAVIOR_UNSPECIFIED`，客户端立即结束了常驻进程。

官方抓包中的 ShellArgs 则稳定包含：

```text
timeout = block_until_ms，缺省 30000
timeout_behavior = TIMEOUT_BEHAVIOR_BACKGROUND
hard_timeout = 86400000
file_output_threshold_bytes = 40000
description = 模型参数
close_stdin = true
conversation_id = 当前对话
admin_command_denylist = 当前 RequestContext
```

Shell 有两条输出路径：

```text
前台阶段
ShellStream stdout/stderr
→ ToolCallDelta(call_id + model_call_id + content)
→ Cursor 当前工具卡片

后台阶段
ShellStream Backgrounded(shell_id + pid)
→ 当前 ToolCallCompleted
→ 后续输出由 Cursor 写入 RequestContextEnv.terminals_folder
```

官方抓包的 `ToolCallDeltaUpdate` 同时携带 `call_id` 与非空 `model_call_id`。旧实现只发送 call_id，并把 model_call_id 固定为空，导致 stdout/stderr 不能稳定归属到对应卡片。

修复后的不变量为：

- `block_until_ms` 只编译为 ShellArgs 的前台等待时间；`0` 表示立即后台化，缺省为协议定义的 30000ms。
- timeout behavior 固定为 BACKGROUND；后台化由客户端返回 `ShellStreamBackgrounded`，服务端不自行猜测进程状态。
- PendingExec 保存当前 conversation、terminals folder 和 command denylist；WebFetch 从 Interaction 转入 Exec 时也保留同一上下文。
- 每个 stdout/stderr delta 使用 PendingExec 中原始 ToolCall 的 call_id 与 model_call_id。
- Backgrounded 终态生成成功的 canonical 字符串结果，其中包含 shell_id、pid、terminals_folder 和后台化前已收到的输出；typed ShellResult 同时保留这些字段供 Cursor UI 使用。
- Runtime environment prompt 明确追加 terminals folder，使下一轮 LLM 可以按 Shell 工具规则读取后台日志。
- Backgrounded 已是当前 ToolCall 的终态。后台输出不重新打开 ToolCall，也不引入不存在的 AwaitShell。
