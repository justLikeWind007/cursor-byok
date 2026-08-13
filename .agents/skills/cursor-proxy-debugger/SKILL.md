---
name: cursor-proxy-debugger
description: Maintain, diagnose, extend, and validate the standalone Cursor HTTPS protocol debugger in cursor-proxy-debugger. Use when changing its command startup, MITM capture behavior, Connect streaming or protobuf decoding, SQLite persistence, local debugging API, embedded web UI, tests, documentation, or when investigating captured Cursor BidiAppend, RunSSE, Fork Chat, or model-discovery traffic.
---

# Cursor Proxy Debugger

Treat `cursor-proxy-debugger` as an independent Go module and executable project. Keep its command entry point and all debugger-specific assets in that directory.

## Respect the project boundary

- Keep every Go file in the project root in `package main`; do not recreate a command directory in the main repository.
- Reuse the shared CA and generated Cursor protobuf packages from `cursor-byok` rather than copying them.
- The canonical proto sources are `cursor-byok/internal/backend/cursor/proto`; update or regenerate them in the main repository when schemas change.
- Keep the tool observational: never modify Cursor settings, system proxy settings, or the installed client automatically.
- Bind the debugging UI to loopback addresses only. Continue passing non-target CONNECT traffic through without MITM.
- Preserve forwarded request and response bodies even when local capture limits truncate stored copies.

## Locate the responsibility

- `main.go`: flags, startup output, browser opening, signals, and graceful shutdown.
- `proxy.go` and `capture.go`: listeners, target matching, MITM, streaming capture, and forwarding.
- `decode.go`: Connect envelopes, compression, protobuf message selection, and JSON views.
- `decode_stored.go`: persisted payload hydration and stored protobuf/text views.
- `proxy_capture.go`: request/response body capture and frame event assembly.
- `store.go`: hot-memory state, SQLite persistence, subscriptions, and conversation queries.
- `store_queries.go`: persisted exchange queries, cloning, redaction helpers, and subscriptions.
- `types.go`: configuration and API-facing capture models.
- `web.go`: loopback API, SSE events, CA download, security headers, and embedded assets.
- `web/app.js`: page state, rendering, Monaco editor lifecycle, and bootstrap.
- `web/app_events.js`: UI event binding for filters, details, pause, and resizing.
- `web/view_helpers.js`: display formatting, HTML escaping, and copy-text helpers.
- `web/styles*.css`: split base, control, detail, and responsive stylesheets.
- `web/`: dependency-free debugging UI and its Chinese/English text.

## Follow the change workflow

1. Inspect `git status` and the relevant staged and unstaged diffs before editing; captures and debugger files may already contain user work.
2. Read the smallest responsible source files. This standalone temporary debugger intentionally does not carry a test suite; for backend, MITM, or routing changes in formal product modules, also follow `chinese-code-style` and its `MODULES.md` boundary rules.
3. For a new protocol endpoint, confirm the exact URL path, request/response direction, streaming mode, compression, and generated protobuf message type. Do not infer schemas from similar endpoints.
4. Decode incrementally across arbitrary read boundaries. Treat Connect flags and the five-byte frame header as protocol data, and keep malformed-frame errors visible without breaking upstream forwarding.
5. Redact sensitive headers in every newly exposed API or UI path. Never log or render authorization material by default.
6. When changing UI text, update both locale tables in `web/i18n.js`, keep `data-i18n` keys aligned, and verify the fallback language.
7. Update `README.md` and `README.en.md` together when commands, flags, supported traffic, storage, or setup steps change.

## TDD boundary and proportional validation

- Formal product modules must follow TDD: write or update a focused failing test first, implement the smallest change that makes it pass, then refactor while keeping the test green.
- This project is a temporary observational tool, so TDD is not mandatory and test files may be intentionally omitted. Validate it with formatting, build checks, the style checker, and targeted manual smoke checks instead.

- Format changed Go files with `gofmt`.
- Run `go build -o <temporary-path>/cursor-proxy-debugger .` from the standalone project after entry-point, dependency, embed, or build-task changes. Do not require tests while this temporary project has no tests.
- Run the Chinese style checker on changed handwritten source files.
- For UI changes, start with `go run . -open=false` when safe, query `/api/status`, and inspect the page in a browser if layout or interaction changed.
- For capture or decoding changes, perform focused manual checks for split reads, compressed frames, malformed input, endpoint direction, persistence, or pass-through behavior as applicable.

## Use the canonical commands

From `cursor-proxy-debugger`:

```bash
go run .
go build -o ./bin/cursor-proxy-debugger .
```
