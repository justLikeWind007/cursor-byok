# Cursor 协议调试器

[中文](README.md) | [English](README.en.md)

这是一个独立运行的本地 Cursor API 调试服务。除 `__debuger__` 调试命名空间外，进入服务端口的 HTTP 请求都会保留方法、路径、查询参数、请求头和请求体，并转发到固定上游 `https://api2.cursor.sh`。服务同时记录 `BidiAppend`、`RunSSE`、Fork Chat 和模型发现等流量。

它不是通用 HTTP 代理，不处理 `CONNECT`，不需要 CA 证书，也不会修改系统代理。

## 启动

首次构建前先生成相邻 `cursor-proto` 项目的 Go 代码：

```bash
(cd ../cursor-proto && ./scripts/generate.sh)
go run .
```

服务只监听一个端口：

- Cursor API 服务：`http://127.0.0.1:9090`
- 调试界面：`http://127.0.0.1:9090/__debuger__/`
- 调试 API：`http://127.0.0.1:9090/__debuger__/api/*`
- 固定上游：`https://api2.cursor.sh`

启动后会自动打开调试界面。

## 配置 Cursor

完全退出 Cursor 后，从终端指定本地 API 地址启动：

```bash
CURSOR_API_ENDPOINT=http://127.0.0.1:9090 \
CURSOR_API_BASE_URL=http://127.0.0.1:9090 \
/Applications/Cursor.app/Contents/MacOS/Cursor
```

`CURSOR_API_ENDPOINT` 覆盖 Agent API 地址；`CURSOR_API_BASE_URL` 让使用基础 API 地址的认证等请求也经过本服务。无需修改 Cursor 代理设置或 Network 设置。

## 构建

```bash
(cd ../cursor-proto && ./scripts/generate.sh)
go build -o ./bin/cursor-proxy-debugger .
```

## 依赖说明

调试器是独立 Go module。Cursor protobuf 消息包由相邻的 `cursor-proto` module 生成；生成的 `gen/` 目录不提交到 Git，因此首次构建前需要运行其 `scripts/generate.sh`。本项目不依赖外层 `cursor-byok` Go module。

## 参数

```text
-addr             Cursor API 服务监听地址，默认 127.0.0.1:9090
-max-exchanges    内存中保留的最大请求数，默认 200
-db               SQLite 数据库路径，默认位于用户配置目录
-open             启动后是否打开浏览器，默认 true
```

## 数据处理

- 所有服务端口收到的请求都固定转发到 `https://api2.cursor.sh`，不会接受客户端指定的其他上游。
- `__debuger__` 命名空间由本地调试页面和调试 API 保留，不会转发到上游。
- `RunSSE` 按 5 字节 Connect 帧头增量拆帧，支持逐帧 gzip 解压。
- `BidiAppendRequest.data` 会继续解码为 `agent.v1.AgentClientMessage`。
- Fork Chat 的 `ForkBackgroundComposer`、`NotifyConversationClone` 和 `UploadConversationBlobs` 会双向解码为 protobuf JSON。
- `CppService/AvailableModels`、`AiService/AvailableModels`、`GetDefaultModel` 和 `GetDefaultModelNudgeData` 会双向解码模型相关数据。
- 请求列表支持按时间和协议 `request_id` 过滤；调试界面可按 `conversation_id` 查询并按会话分组。
- 完整抓包写入 SQLite，重启后仍可查询；`max-exchanges` 只限制内存热数据数量。
- `Authorization`、`Cookie`、`Set-Cookie` 等敏感请求头在界面中默认隐藏。
- 单侧原始正文默认最多保留 2 MiB；转发内容不会被抓取上限截断。
