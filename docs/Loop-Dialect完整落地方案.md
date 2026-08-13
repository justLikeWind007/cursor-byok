# Loop + Dialect 完整落地方案

## 1. 设计目标

这是一个从零设计的服务端方案，不依赖当前项目的服务端实现。

目标只有四个：

1. Loop 的状态决策是纯函数，外层运行器用递归驱动它，直到得到最终答案。
2. LLM 使用原生请求和原生流事件，不再造一套平行的模型消息结构。
3. Cursor 的 Bidi、RunSSE、protobuf 只存在于 Dialect 和 Transport 中。
4. `messages` 永远是完整、顺序固定、只追加的历史，保证前缀缓存稳定。

模型调用是无状态的。每一次调用都发送完整的 `RequestMessages`，而不是向模型发送“上一次请求的差异”。

## 2. 顶层结构

代码目录只保留四个模块：

```text
server/
  loop/        纯函数状态转换、消息历史和下一步决定
  llm/         原生请求、原生响应流、供应商适配器
  transport/   Connect、Bidi、RunSSE、CursorDialect、运行器
  store/       SQLite 状态、调用记录、输入和工具去重
```

`CursorDialect` 是 `transport/` 里的协议翻译文件，不单独形成目录。客户端是远端能力：服务端向它发工具请求，它经 Bidi 返回工具结果；因此也不在服务端拆出 `client/` 模块。

`store/` 只是基础设施适配器：它保存状态和提交记录，不决定下一步动作。

依赖方向固定为：

```text
transport -> loop
transport -> llm
transport -> store

CursorDialect (inside transport) -> loop / llm
```

`transport/runner` 是很薄的组装代码：它执行 `Command`，把外部结果再送回 `loop`。`loop` 不依赖 protobuf、HTTP、SSE、连接对象、Store、客户端或具体 LLM 供应商。

## 3. 三类核心数据

### 3.1 模型请求

直接使用 `internal/backend/cursor/llm/request.go` 中的结构：

```text
RequestMessages {
  SystemPrompt
  Messages []Message
  Tools []ToolDefinition
}
```

这里有一个重要边界：

- `Messages` 是会话历史，必须只追加。
- `SystemPrompt` 和 `Tools` 是本次请求构建出来的请求部分。
- 前缀缓存约束只针对 `Messages`。
- 不能通过合并、去重、重排或“修正上一条消息”来构建历史。

每次请求的模型上下文都是：

```text
RequestMessages {
  SystemPrompt: buildPrompt(input, state)
  Messages:     state.messages
  Tools:        buildTools(input, state)
}
```

`buildPrompt` 和 `buildTools` 可以每次重新计算，但不能修改 `state.messages`。

### 3.2 模型响应

直接使用 `internal/backend/cursor/llm/response.go` 中的结构：

```text
ResponseEvent {
  Start
  TextStart / TextDelta / TextEnd
  ThinkingStart / ThinkingDelta / ThinkingEnd
  ToolCallStart / ToolCallDelta / ToolCallEnd
  Done
  Error
}
```

完整响应使用 `AssistantMessage`。工具结果使用 `ToolResultMessage`。用户输入使用 `UserMessage`。

`internal/backend/cursor/llm/stream.go` 中的接口是 LLM 边界：

```text
ResponseStream.Recv(context) -> (ResponseEvent, error)
```

LLM 适配器可以将 OpenAI、Anthropic、Gemini 或其他供应商的响应转换为这些原生中间结构，但不能把供应商私有的流格式泄漏到 Loop。

### 3.3 Loop 输入

Loop 只接收有语义的输入，不接收网络数据：

```text
Input =
  Start {
    userMessage: UserMessage
    context:     []ContextSupplement
  }
| LLMEvent {
    callID: string
    event:  ResponseEvent
  }
| ToolResult {
    message: ToolResultMessage
  }
| UserMessage {
    message: UserMessage
  }
| Cancel {
    reason: string
  }
```

`BidiAppend` 解码后只能生成这些输入。Loop 不需要知道输入原来来自 Bidi、HTTP 还是测试代码。

上下文补充如果要被模型看到，必须转换成新的消息追加到历史；不能回写旧消息：

```text
旧 messages + 新 UserMessage(context supplement)
```

## 4. 状态：已提交部分与正在生成部分

运行中的状态分为两部分：

```text
RuntimeState {
  committed       ConversationState
  pendingResponse *PendingResponse
}

ConversationState {
  conversationID
  turnID
  messages           []llm.Message
  waiting           *WaitingClient
  status             Ready | WaitingLLM | WaitingClient | Final | Failed | Canceled
  lastCommitID       string
}
```

### 4.1 `messages`

`messages` 是唯一的模型历史：

- 只能在完整的 `UserMessage`、`AssistantMessage` 或 `ToolResultMessage` 完成后追加。
- 已经追加的消息永远不变。
- 顺序永远按照发生顺序排列。
- 不使用 map 作为模型消息容器。
- 不在重放时重新生成时间戳、随机 ID 或不稳定字段。
- 工具调用的签名、思考签名和供应商响应 ID 原样保留。

### 4.2 `pendingResponse`

`pendingResponse` 是本次 LLM 流的临时聚合器，不属于模型历史：

```text
PendingResponse {
  callID
  partialMessage
  openContentBlocks
  openToolCalls
  usage
}
```

它只接收流中的增量事件。只有 `Done` 才能把完整的 `AssistantMessage` 追加到 `messages`。

`pendingResponse` 只存在于内存。每个 delta 都可以被实时发送给 RunSSE，也可以写入诊断日志，但不会被提交到 `ConversationState`。

如果进程在 `Done` 前重启：

```text
丢弃 pendingResponse
保留本次 callID、requestHash 和调用状态
从最后一份已提交的 ConversationState 重试相同请求
```

不恢复半截文本，不把旧 delta 与新响应拼接，也不把旧 delta 当作模型消息。这样即使上游流不可续传，模型历史仍然一致。

## 5. 纯函数转换接口

Loop 的核心函数固定为：

```text
transition(state, input) -> Transition
```

返回值：

```text
Transition {
  state
  emit      []llm.ResponseEvent
  command   Command
}
```

`emit` 使用 LLM 原生 `ResponseEvent`，不创建 `AssistantTextDelta`、`ToolCallOutput` 等第二套事件。

`Command` 只有几种：

```text
Command =
  ContinueLLM
| CallLLM {
    callID
    request RequestMessages
    messagesHash string
  }
| CallClient {
    operationID
    toolCall ToolCall
  }
| WaitInput
| Final {
    message AssistantMessage
  }
| Failed {
    message AssistantMessage
  }
| Canceled {
    reason string
  }
```

`Command` 是普通数据，不能携带闭包、连接、channel 或函数指针。这样它可以记录、重放和比较。

## 6. Loop 的递归规则

核心判断是纯函数：

```text
transition(runtimeState, input) -> Transition
```

它不执行 I/O，也不自行取得下一条输入。递归发生在外层运行器：

```text
run(state, input) {
  result = transition(state, input)
  publish(result.emit)
  commitWhenNeeded(result)

  if result.command is Final or Failed or Canceled or WaitInput {
    return result
  }

  return runCommand(result.state, result.command)
}
```

`run` 是递归入口，`transition` 是唯一的状态判断函数。网络和客户端调用不能放进纯函数，因此 `runCommand` 是外层执行器：

```text
runCommand(state, command) {
  switch command {
    case CallLLM:
      return consumeLLM(state, command)

    case CallClient:
      sendClientCommand(command)
      return { state, emit: [], command: WaitInput }

    case WaitInput, Final, Failed, Canceled:
      return { state, emit: [], command }

    case ContinueLLM:
      return invalidState("ContinueLLM without stream")
  }
}
```

`CallClient` 不能同步等待工具返回。它被编码为外层协议消息后，`runCommand` 立即结束本次调用；之后客户端通过 BidiAppend 提交 `ToolResultMessage`，Transport 再次调用 `run(loadedState, ToolResult)`。

这里不是从旧调用栈继续等待。工具结果是一个新的外部输入，也是递归的下一层。

实现时可以使用异步尾递归、trampoline 或任务调度器避免实际调用栈无限增长，但不能把业务逻辑改成一个可随意修改历史的可变状态循环。

## 7. LLM 流的处理

### 7.1 启动一次调用

`CallLLM` 携带完整请求和请求快照信息：

```text
CallLLM {
  callID
  request
  messagesHash
}
```

`messagesHash` 是发送前按消息顺序对完整 `Messages` 序列做的稳定哈希，用来确认重试时没有改变历史。重试实际使用已保存的 `exactRequest`，不重新 build prompt、tools 或 messages。

执行器：

```text
consumeLLM(state, command) {
  stream = llm.call(command.request)
  return readLLM(state, command.callID, stream)
}
```

### 7.2 逐个接收事件

```text
readLLM(state, callID, stream) {
  event = stream.Recv()
  result = transition(state, LLMEvent(callID, event))
  publish(result.emit)
  commitWhenNeeded(result)

  switch result.command {
    case ContinueLLM:
      return readLLM(result.state, callID, stream)

    case CallClient, CallLLM, WaitInput, Final, Failed, Canceled:
      return runCommand(result.state, result.command)
  }
}
```

上面的 `CallClient` 分支会发送一个客户端请求并返回 `WaitInput`；它不会占用 LLM 流或阻塞 HTTP handler。下一个 Bidi 输入是另一次 `run(loadedState, input)` 调用。

### 7.3 各事件的状态变化

```text
Start
  -> 创建 pendingResponse
  -> 原样发布 ResponseEvent.Start

TextStart / ThinkingStart / ToolCallStart
  -> 打开对应内容块
  -> 原样发布事件

TextDelta / ThinkingDelta / ToolCallDelta
  -> 追加到 pendingResponse
  -> 原样发布事件

TextEnd / ThinkingEnd / ToolCallEnd
  -> 关闭对应内容块
  -> 原样发布事件

Done(stop)
  -> 校验完整 AssistantMessage
  -> 将它追加到 messages
  -> 清空 pendingResponse
  -> command = Final

Done(toolUse)
  -> 追加完整 AssistantMessage
  -> 清空 pendingResponse
  -> 取第一项未完成工具调用
  -> command = CallClient

Error / Aborted
  -> 丢弃未完成 pendingResponse
  -> 保存错误记录
  -> 不把半截响应追加到 messages
  -> command = Failed 或 Canceled
```

无论模型返回多少个 delta，`messages` 最终只追加一条完整的 `AssistantMessage`。

`ResponseEvent.Partial` 是 LLM 适配器提供的当前累计视图。Loop 可以用它校验 `pendingResponse` 或供 RunSSE 重连时显示，但不能用它覆盖、修改或合并任何已提交的 `messages`。唯一允许提交到 `messages` 的助手响应来自 `ResponseEvent.Done.Message`。

## 8. 外层流协议的对接

外层协议分两层：

```text
Transport
  负责连接、读写、framing、断开、heartbeat

Dialect
  负责 protobuf 消息与原生语义结构之间的翻译
```

Loop 只产生 `ResponseEvent` 和 `Command`，不直接写 RunSSE。

### 8.1 输入方向

```text
BidiAppend request
  -> Transport 解 Connect body
  -> Dialect.decodeClientMessage
  -> Input
  -> transition(state, input)
```

`Dialect.decodeClientMessage` 的映射：

```text
run_request.user_message
  -> Input.Start 或 Input.UserMessage

exec_client_message.tool_result
  -> Input.ToolResult

interaction_response
  -> Input.UserMessage 或对应 ClientInput

conversation_action.cancel
  -> Input.Cancel
```

Bidi 的 `request_id`、`append_seqno`、`conversation_id` 属于 Transport/Dialect 的关联信息，不进入模型消息文本。

### 8.2 输出方向

```text
transition.emit: ResponseEvent
  -> Dialect.encodeServerEvent
  -> AgentServerMessage
  -> RunSSE writer
```

推荐映射：

```text
ResponseEvent.Start
  -> 不写协议消息；只初始化本次流的内部关联状态

ResponseEvent.TextStart / ResponseEvent.TextEnd
  -> 不写协议消息；Cursor 由 text_delta 表达可见文本

ResponseEvent.TextDelta
  -> interaction_update.text_delta

ResponseEvent.ThinkingDelta
  -> interaction_update.thinking_delta

ResponseEvent.ThinkingEnd
  -> interaction_update.thinking_completed

ResponseEvent.ToolCallStart
  -> interaction_update.tool_call_started

ResponseEvent.ToolCallDelta
  -> interaction_update.tool_call_delta

ResponseEvent.ToolCallEnd
  -> interaction_update.tool_call_completed

Command.CallClient
  -> exec_server_message

ResponseEvent.Done(stop)
  -> interaction_update.turn_ended
  -> RunSSE end-stream

ResponseEvent.Error
  -> 协议错误消息或 RunSSE 结构化错误
  -> RunSSE end-stream
```

这里的映射只是协议表达方式改变，事件的文本、工具调用 ID、工具名、参数、停止原因和响应 ID 都必须保留。

### 8.3 LLM 流与 RunSSE 的时序

```text
RunSSE 建立
  -> 注册 request_id
  -> 接收 Start
  -> 写出 TextDelta / ThinkingDelta
  -> 写出 ToolCallDelta
  -> 写出工具请求
  -> 等待 BidiAppend 工具结果
  -> 继续下一次 LLM 流
  -> 写出 Done
  -> 关闭 RunSSE
```

RunSSE writer 必须顺序写出事件。不能让多个 goroutine 直接写同一个连接；所有输出先进入一个有序发送队列。

heartbeat 属于 Transport，不属于 LLM `ResponseEvent`，也不进入 `messages`。

## 9. Dialect 的边界

Dialect 只包含三类代码：

### 9.1 解码

将 Cursor protobuf 转换为内部输入：

```text
decodeBidiAppend(request) -> InputEnvelope
decodeExecClientMessage(message) -> ToolResult
decodeInteractionResponse(message) -> ClientInput
```

### 9.2 编码

将原生 LLM 事件和客户端命令转换为 Cursor protobuf：

```text
encodeResponseEvent(event) -> AgentServerMessage
encodeClientCommand(command) -> ExecServerMessage / InteractionQuery
```

### 9.3 协议关联

Dialect 可以补充协议必需的：

- `request_id`
- `conversation_id`
- `interaction_id`
- `turn_seq`
- `exec_id`
- `tool_call_id`
- Bidi 的 `append_seqno`

Dialect 不可以做以下事情：

- 拼接或修改模型历史。
- 根据文本猜测工具调用。
- 决定是否重试 LLM。
- 执行工具。
- 保存 Loop 状态。
- 把 delta 合并成另一套公共事件。

如果以后增加 WebSocket 方言，只需新增一个编码/解码实现，Loop、LLM 和 Client 不变。

## 10. 工具调用和客户端等待

模型完成一次响应并返回 `StopReasonToolUse` 时：

```text
AssistantMessage(ToolCall)
  -> append 到 messages
  -> command = CallClient
```

`CallClient` 是一个可持久化的普通数据：

```text
CallClient {
  operationID
  toolCall {
    id
    name
    arguments
  }
}
```

Dialect 将其变成 `exec_server_message`，Transport 发送给客户端。此时 Loop 状态是 `WaitingClient`。

客户端返回结果后：

```text
ToolResultMessage
  -> append 到 messages
  -> 当前工具调用标记完成
  -> 仍有未完成工具调用时，command = CallClient(下一项)
  -> 全部完成时，清空 waiting，重新 build RequestMessages，command = CallLLM
```

工具结果只能通过 `ToolCallID` 关联，不能根据消息顺序猜测对应关系。

一条 `AssistantMessage` 可以包含多个 `ToolCall`。第一版固定按该消息中 `Content` 的顺序逐个派发；一个工具结果提交完成后才派发下一个。这样工具结果追加到 `messages` 的顺序是确定的，连续 LLM 请求的前缀也稳定。未来若必须并行执行，也必须等全部结果完成后按原始工具调用顺序统一追加，不能按到达顺序追加。

## 11. 幂等和重试

Loop 的幂等规则如下：

### 11.1 输入去重

每个输入带有 `inputSeq` 或外部稳定 ID：

```text
inputID = requestID + appendSeqno
```

已经提交过的输入再次到达时，返回之前记录的 Transition 结果，不重复执行工具或追加消息。

### 11.2 工具调用去重

`operationID` 由 `conversationID + turnID + toolCallID` 生成。

执行前查询提交记录：

```text
已完成 -> 直接返回已保存的 ToolResultMessage
执行中 -> 等待原操作结果
未执行 -> 执行一次
```

### 11.3 LLM 重试

LLM 重试必须使用：

```text
同一个 callID
相同的 messagesHash
完全相同的 RequestMessages 序列化结果
```

不合并两次响应，不把第一次的半截文本和第二次的文本拼接起来。只有一个完整、合法的 `Done` 结果可以提交到 `messages`。

如果某次响应已经提交，再收到同一 `callID` 的重复流，整次流丢弃，不追加第二条助手消息。

### 11.4 同一会话的顺序

同一个 `conversationID` 的输入和 LLM 流事件必须串行进入 `transition`。这是执行顺序，不是另一套业务状态机：

```text
conversation_id
  -> 一条顺序执行链
  -> transition
  -> SQLite version compare-and-swap
```

可在进程内用按 `conversationID` 的短锁或任务队列减少竞争；SQLite 的 `version` 是最终裁决。任何提交发现版本已变化，就重新加载状态并重新处理尚未提交的输入。不能让两个 LLM 流同时向同一个会话追加消息。

## 12. 前缀缓存保证

每次 LLM 请求满足：

```text
request[n].Messages = request[n-1].Messages + newlyCommittedMessages
```

禁止：

- 修改历史消息内容。
- 合并相邻消息。
- 把多条 tool result 重排。
- 在旧消息中插入新的 context。
- 每次重放重新生成随机 ID 或时间戳。
- 把流式 delta 直接写入历史。

动态 prompt 和 Tools 每次可以重新 build，但 `Messages` 的字节序列必须只增加，不回退、不重写。

这里的“前缀”指每个已存在消息的语义内容和确定性序列化都不变，新增消息只排在末尾。完整 HTTP JSON body 本身不要求是字节前缀，因为 `SystemPrompt` 和 `Tools` 可以在本次请求重新 build；供应商适配器的责任是确保既有消息对应的请求片段不发生变化。

建议在每次 `CallLLM` 记录：

```text
messagesHash
messageCount
lastMessageHash
serializedRequestHash
```

测试必须确认连续请求满足前缀关系，而不是只比较消息数量。

## 13. 持久化边界

Store 至少提供以下能力：

```text
load(conversationID) -> ConversationState
loadInputResult(inputID) -> PreviousCommit?
commitInput(inputID, beforeVersion, nextState) -> CommitResult
saveLLMCall(callID, exactRequest, requestHash, status)
saveClientOperation(operationID, request, result)
```

提交顺序固定：

```text
1. transition 得到新状态和 command
2. 对会话状态有变化时，在一个事务中保存 state、inputID 和调用记录
3. 对 CallLLM，先保存 exactRequest 和 callID，再打开上游流
4. 对 CallClient，先保存 waiting 和 operationID，再写出客户端请求
5. LLM delta 实时写入 RunSSE，但不提交到 ConversationState
6. 外部结果作为新的 Input 再进入 transition
```

流中的 delta 默认不落入会话历史，也不需要进入 SQLite outbox。可以单独保存为诊断日志，但不能把诊断日志当作下一次 LLM 的 `Messages`。

`AssistantMessage`、`ToolResultMessage`、`UserMessage`、未完成的 `CallLLM` 和未完成的 `CallClient` 必须在进程重启后可恢复。RunSSE 连接和未完成 delta 不需要持久化；重连时可以重新打开当前 turn 的 RunSSE，恢复调用后重新流式展示。模型历史不受影响，因为旧 delta 从未提交。

### 13.1 SQLite 最小表结构

第一版不需要事件溯源库。五张表足够：

```text
conversations
  conversation_id  primary key
  version          integer       -- 每次已提交状态递增
  status           text
  turn_id          text
  messages_json    blob          -- 按顺序的 llm.Message 数组
  waiting_json     blob nullable -- 未完成 CallClient
  updated_at_ms    integer

input_commits
  conversation_id
  input_id
  committed_version
  result_json      blob          -- 重复 Bidi 输入的返回结果
  primary key (conversation_id, input_id)

llm_calls
  call_id          primary key
  conversation_id
  request_json     blob          -- exactRequest
  request_hash     text
  messages_hash    text
  status           text          -- planned, streaming, committed, failed, canceled
  assistant_hash   text nullable

client_operations
  operation_id     primary key
  conversation_id
  turn_id
  tool_call_id
  tool_index       integer
  request_json     blob
  result_json      blob nullable
  status           text          -- planned, sent, completed, canceled

stream_diagnostics
  call_id
  event_index
  event_json       blob
  primary key (call_id, event_index)
```

`stream_diagnostics` 是可选表，只用于调试和抓包分析。它绝不能被读取后回填成 `messages`。

每次提交使用 SQLite 事务和乐观版本条件：

```text
update conversations
set version = version + 1, ...
where conversation_id = ? and version = ?
```

没有更新到一行说明发生竞争；重新加载后再处理。`input_commits` 的唯一键负责 Bidi 重放去重，`llm_calls.call_id` 和 `client_operations.operation_id` 分别负责 LLM 与工具调用去重。

## 14. 取消、断线和错误

### 14.1 用户取消

```text
conversation_action.cancel
  -> Input.Cancel
  -> transition 返回 Canceled
  -> cancel LLM stream / client operation
  -> 发布协议取消事件
  -> 关闭 RunSSE
```

### 14.2 RunSSE 断线

RunSSE 断开不等于用户取消。只停止当前发送连接，Loop 继续运行一段重连宽限时间。Bidi 仍可提交工具结果或取消命令。

### 14.3 LLM 流错误

```text
Recv error
  -> 生成 ResponseEvent.Error
  -> 丢弃 pendingResponse
  -> 保存 call failure
  -> 根据策略 Failed 或重新发起同一 callID
```

不得把网络错误文本写成正常 `AssistantMessage`。

### 14.4 客户端工具错误

工具失败仍然生成 `ToolResultMessage{IsError: true}`，追加后交给下一次 LLM。只有协议连接错误、取消或系统不可恢复错误才终止 Loop。

## 15. 推荐执行时序

```text
1. Transport 收到 RunSSE 或 BidiAppend
2. Dialect 验证 request_id、seqno 和 protobuf oneof
3. Store 加载 conversation 的 ConversationState
4. Dialect 将客户端消息解码成 Input
5. transition(state, input)
6. Store 在同一事务中提交新的 state、inputID 和必要的调用记录
7. 将 `ResponseEvent` 编码后按顺序写入 RunSSE；delta 不写入模型历史
8. 执行 command；CallClient 发出请求后返回等待态
9. LLM 流逐事件回到第 5 步
10. 客户端工具结果回到第 4 步
11. Done(stop) 后写出 turn ended，并关闭 RunSSE
```

## 16. 最小接口集合

实现第一版只需要这些接口：

```text
type LLM interface {
  Call(context, RequestMessages) -> ResponseStream
}

type Dialect interface {
  DecodeBidi(bytes) -> InputEnvelope
  EncodeResponse(ResponseEvent, ProtocolContext) -> AgentServerMessage
  EncodeClientCommand(Command, ProtocolContext) -> AgentServerMessage
}

type Store interface {
  Load(conversationID) -> ConversationState
  FindInputCommit(conversationID, inputID) -> PreviousCommit?
  CommitInput(inputID, expectedVersion, nextState) -> CommitResult
  SaveLLMCall(callID, exactRequest, hashes, status)
  SaveClientOperation(operationID, request, status)
}

type Transport interface {
  ReceiveBidi()
  OpenRunSSE()
  Send(AgentServerMessage)
}
```

客户端工具结果由 Bidi 适配器解码并再次送入 `run`，不需要一个阻塞式的 `Client.Execute` 服务端接口。接口名称可以调整，但职责不能跨层移动。

## 17. 测试要求

### 17.1 Loop 纯函数测试

给定相同的 `state + input`，必须得到完全相同的：

- 新状态。
- `emit` 顺序。
- `command` 内容。
- `messagesHash`。

覆盖：文本流、思考流、工具调用流、正常完成、长度停止、错误、中断、重复输入。

### 17.2 前缀测试

连续三次调用的 `Messages` 必须满足：

```text
M1 是 M2 的严格前缀
M2 是 M3 的严格前缀
```

测试序列化后的消息字节，而不是只比较对象字段。

### 17.3 Dialect 测试

每一种协议消息都测试：

```text
protobuf -> Input
Input/ResponseEvent -> protobuf
```

重点验证 ID、seqno、工具参数、错误码、停止原因和 oneof 分支没有丢失。

### 17.4 流集成测试

使用假的 `ResponseStream` 依次返回：

```text
Start -> TextDelta* -> ToolCall* -> Done
```

断言：

- 每个 delta 都按顺序发到 RunSSE。
- 只有 Done 后才追加 AssistantMessage。
- 工具结果到达后才启动下一次 LLM。
- 重复 Done 不产生第二条消息。

## 18. 第一版落地顺序

1. 固定 `llm.RequestMessages`、`ResponseEvent`、`ResponseStream` 为核心契约。
2. 实现 `ConversationState`、`Input`、`Command` 和纯函数 `transition`。
3. 实现 `consumeLLM`，验证流事件和 `pendingResponse` 聚合。
4. 实现 `CallClient` 命令、工具请求发送和 `ToolResultMessage` 回传。
5. 实现 Dialect 的 Bidi 解码和 RunSSE 编码。
6. 加入 Store 的状态提交、输入去重和工具操作去重。
7. 最后接入真实 Connect transport、heartbeat、重连和取消。

完成后，新增一种客户端协议只需要新增 Dialect；新增一种 LLM 供应商只需要新增 LLM 适配器；新增一种工具只需要新增工具能力描述和对应的客户端协议映射。Loop 本身不需要增加状态分支。
