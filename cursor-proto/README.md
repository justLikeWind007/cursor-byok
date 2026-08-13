# cursor-proto

`cursor-proto` 从已安装 Cursor 的 JavaScript bundle 中提取 Protobuf 定义。

## 目录

- `extractor/`：Go 提取器。
- `scripts/extract.sh`：扫描 Cursor 安装目录并安全更新输出。
- `proto/`：提取结果，是项目内唯一的 Proto 输出目录；由脚本重新生成，不提交到 Git。
- `scripts/generate.sh`：根据提取结果生成可导入的 Go 消息包。
- `gen/`：供其他 Go module 使用的 Go 消息包；由脚本重新生成，不提交到 Git。

## 使用

默认从 `/Applications/Cursor.app` 提取：

```bash
./scripts/extract.sh
```

也可以指定 Cursor 应用、bundle 文件和输出目录：

```bash
./scripts/extract.sh /path/to/Cursor.app
./scripts/extract.sh /path/to/workbench.desktop.main.js /path/to/output
```

直接运行 Go 提取器时，可重复传入多个 bundle：

```bash
go run ./extractor \
  -input /path/to/workbench.desktop.main.js \
  -input /path/to/extensionHostProcess.js \
  -output ./proto \
  -strict
```

提取完成后重新生成 Go 消息包：

```bash
./scripts/generate.sh
```

## 验证

```bash
go test ./...
```
