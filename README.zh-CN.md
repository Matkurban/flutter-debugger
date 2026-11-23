# Flutter Debugger for Zed

面向 [Zed](https://zed.dev) 的全功能 Flutter / Dart 开发、调试与原生 DevTools 套件，致力于提供接近 VS Code Flutter (`Dart-Code`) 的日常开发体验，并与 Zed 官方 `dart` 扩展及 `pubgrade` 零冲突协同。

> [English README](./README.md) · [版本发布 Releases](https://github.com/Matkurban/flutter-debugger/releases)

---

## 核心特性

- **LSP 编辑器增强（`Dart` & `YAML`）**：Widget 闭合标签（Closing Labels）、Flutter 颜色预览与取色器、`main()` 与测试文件 CodeLens、流式 `--machine` 测试运行器（支持嵌套 `group` 路径与 UTF-16 精准失败诊断）、保存防抖自动热重载、`pubspec.yaml` 资源/字体/本地路径校验与跳转。
- **DAP 调试代理与 Debug Console 增强**：延迟解析 Monorepo / FVM / `--test`、`flutter daemon` 实时设备热插拔与智能回退选设备、异常断点与 Just-My-Code 开关、Debug Console `zed://file/...:line:col` `Cmd+点击` 精准跳转源码（复用当前 Zed 窗口）、对齐 VS Code 的多状态 ANSI 彩色日志（自动适配 Zed 浅/深色主题）。
- **原生 GPUI DevTools（`flutter-devtools-gpui`）**：独立轻量原生桌面伴侣应用，内置 **Widget Inspector**（Layout Explorer、Properties、Details Tree、Select Widget Mode 跳转源码）、**Network**（HTTP + Dart 3.14+ WebSocket、gzip/JSON 高亮、详情选中与一键复制、列标题三态排序）、**Logging**（VM Service 实时日志、选中防滑走、点击跳转当前 Zed 窗口）、**Performance**（UI/Raster 帧耗时与卡顿统计）与 **Memory**（Isolate 堆内存、RSS 与手动 GC）。
- **`ctl` CLI 与零配置初始化**：交互式设备/模拟器选择、原生终端 `ctl run`、全套 Flutter Service Extensions（`overlay` / `brightness` / `platform` / `screenshot`）、直达指定页的 Web/GPUI DevTools，以及无损 `init-zed` 项目配置生成。

---

## 架构分层与零冲突边界

| 模块 | 目录 | 职责 | 零冲突边界 |
|---|---|---|---|
| **官方 `dart` 扩展** | 外部扩展 | 语法高亮、补全、Hover、跳转定义、格式化、重命名、Dart Analysis 诊断 | 绝不重复实现 |
| **`pubgrade` 扩展** | 外部扩展 | `pubspec.yaml` 依赖版本提示、过期检测与升级 | `enablePubVersionHints` 默认 `false`，避免重复提示 |
| **Zed WASM 扩展** | `src/` | 注册 `flutter-debugger` LSP（仅 `Dart` / `YAML`）、`Flutter` DAP 适配器与 `flutter` Debug Locator；按配置 / `PATH` / GitHub Releases 解析后端二进制 | 仅在含 `pubspec.yaml` 的目录生效，非 Flutter 目录零后台进程 |
| **共享协议库** | `crates/protocol/` | 会话注册表、IPC 协议、Overlay 状态、Dart URI 与 `zed://file/...` 改写、ANSI 控制台着色 | `server` 与 `devtools-gpui` 共享单一数据源 |
| **后端服务（`flutter-debugger-server`）** | `server/` | LSP 服务、DAP 代理、`flutter daemon` 监听、VM Service 客户端与 `ctl` CLI | `F4` 断点调试、`attach` 与原生终端 `ctl run` 体验完全一致 |
| **原生 GPUI DevTools（`flutter-devtools-gpui`）** | `devtools-gpui/` | 单实例原生 GPUI 桌面 DevTools（Inspector / Network / Logging / Performance / Memory） | 直连当前活跃会话 IPC 与 Dart VM Service |

---

## 安装指南

### 方式一：本地一键构建安装（推荐，同时安装原生 GPUI DevTools）

前置依赖：Rust stable 工具链（需安装 `wasm32-wasip2` 目标：`rustup target add wasm32-wasip2`）与 Flutter SDK。

```bash
./scripts/install-local.sh
```

脚本会自动完成 WASM 扩展校验与构建、全部单元测试与 Clippy 检查，并将 `flutter-debugger-server` 和 `flutter-devtools-gpui` 安装至 `~/.local/bin`（传入 `--server-only` 可仅构建安装后端服务）。

### 方式二：GitHub Releases 预编译后端

当 `PATH` 与 `settings.json` 中未配置本地二进制时，WASM 扩展会自动从 [GitHub Releases](https://github.com/Matkurban/flutter-debugger/releases) 下载对应平台的 `flutter-debugger-server`：
- macOS：`macos-aarch64`、`macos-x86_64`
- Linux：`linux-x86_64`、`linux-aarch64`
- Windows：`windows-x86_64`

> 注：原生 GPUI DevTools（`flutter-devtools-gpui`）目前需通过 `./scripts/install-local.sh` 本地构建安装。

### 在 Zed 中启用扩展

1. 打开命令面板 → `zed: install dev extension` → 选择本仓库根目录。
2. 确保 `~/.local/bin` 已加入 `PATH`（macOS 下若 Zed GUI 未继承 shell `PATH`，推荐在 `~/.config/zed/settings.json` 中显式指定 `lsp.flutter-debugger.binary.path`）。
3. 在任意 Flutter 项目根目录执行：
   ```bash
   flutter-debugger-server init-zed
   ```
   该命令会自动扫描 Android Gradle / iOS Scheme flavors 与 `lib/main*.dart`，无损生成或合并 `.zed/debug.json`（含 `Flutter: Current Test (Breakpoints)`）与 `.zed/tasks.json`。

---

## 推荐 Zed 配置

可随时运行 `flutter-debugger-server ctl print-zed-config` 打印最新推荐配置。

### `~/.config/zed/settings.json`

```json
{
  "lsp_document_colors": "inlay",
  "inlay_hints": {
    "enabled": true
  },
  "lsp": {
    "flutter-debugger": {
      "binary": {
        "path": "/Users/<你的用户名>/.local/bin/flutter-debugger-server"
      },
      "initialization_options": {
        "autoHotReloadOnSave": true,
        "hotReloadDebounceMs": 250,
        "closingLabels": true,
        "closingLabelsMinLines": 2,
        "colorPreviews": true,
        "enablePubVersionHints": false,
        "exposeGlobalCodeActions": false
      }
    }
  }
}
```

### `~/.config/zed/keymap.json`（类 VS Code Flutter 快捷键）

```json
[
  {
    "context": "Workspace",
    "bindings": {
      "cmd-alt-r": ["task::Spawn", { "task_name": "Flutter: Hot Reload" }],
      "cmd-shift-alt-r": ["task::Spawn", { "task_name": "Flutter: Hot Restart" }],
      "cmd-alt-d": ["task::Spawn", { "task_name": "Flutter: Select Device" }],
      "cmd-alt-e": ["task::Spawn", { "task_name": "Flutter: Launch Emulator" }],
      "cmd-alt-i": ["task::Spawn", { "task_name": "Flutter: Toggle Widget Inspector" }],
      "cmd-alt-p": ["task::Spawn", { "task_name": "Flutter: Open DevTools (Inspector)" }],
      "cmd-alt-b": ["task::Spawn", { "task_name": "Flutter: Toggle Brightness" }],
      "cmd-alt-s": ["task::Spawn", { "task_name": "Flutter: Stop App" }]
    }
  }
]
```

---

## 功能详解

### LSP 编辑器增强
- **Widget 闭合标签（Closing Labels）**：在多行 Widget 构造闭合括号后以 `inlayHint` 显示 `// WidgetName`。
- **Flutter 颜色预览**：支持 `Color(0xAARRGGBB)`、`Color.fromARGB`、`Color.fromRGBO`、`Color.from(...)` 与 `Colors.*` 色板预览和内联改色。
- **`main()` 与测试 CodeLens**：在 `main()` 上方直接提供设备切换、Hot Reload / Hot Restart、Stop 与 Web/原生 DevTools 入口；在 `*_test.dart` 与 `integration_test/` 中提供单项、嵌套 `group()` 与整文件运行/断点调试入口。
- **保存防抖热重载**：保存非测试 `.dart` 文件自动触发热重载并在 Zed 进度条显示耗时（兼容 DAP 调试与 `ctl run` 会话）。
- **`pubspec.yaml` / `pubspec_overrides.yaml`**：检测缺失的资源、字体与本地 `path:` 依赖并提供 `DocumentLink` 点击跳转，支持 `Pub Get` / `Pub Upgrade` / `Pub Outdated` / `Clean` / `Doctor`。

### DAP 调试代理与 Debug Console
- **Monorepo、FVM 与测试断点支持**：`launch` / `attach` 时延迟解析子包根目录，自动适配 `.fvm` 与 `FLUTTER_SDK_PATH`，对测试目标自动附带 `--test` 启动 `flutter debug_adapter`。
- **智能设备解析**：结合 `flutter daemon` 实时热插拔与本地持久化设备；未配置或目标设备离线时自动选择兼容设备并在控制台给出提示。
- **异常断点与 Just-My-Code 开关**：默认不启用异常断点（`None`），支持在运行期实时切换异常暂停模式（`None` / `Unhandled` / `All`）与调试库作用域（仅我的代码 / 含外部包 / 含 SDK 库）。
- **源码路径 `Cmd+悬停高亮` 与 `Cmd+点击跳转`**：将日志/异常中的 `file://`、`package:`（基于 `mtime` 缓存的 `.dart_tool/package_config.json`）及磁盘真实存在的裸 `.dart:line:col` 路径改写为百分比编码的 `zed://file/...:line:col` 链接（同时保持 DAP `body.source.path` 为纯本地路径），点击后在已打开的当前 Zed 窗口中精准跳转至目标行列。
- **多状态 ANSI 彩色日志**：按 DAP `category` 与 Flutter/Dart 日志级别自动注入主题自适应 ANSI 16 色（`stderr`/错误红、`console` 构建/警告黄、热重载/构建成功绿、服务链接青、`stdout` 常规信息蓝、调试/框架堆栈与 `\x1b[2m` 暗灰），同时完整保留第三方日志库自带的 16 色、24-bit TrueColor 及跨行 ANSI 彩色块。

### 原生 GPUI DevTools（`flutter-devtools-gpui`）
可通过 `main()` CodeLens、`flutter-debugger-server ctl devtools-ui` 或直接命令行启动：`flutter-devtools-gpui [--project <dir>] [--session <id>] [--vm-uri <ws://...>] [--page <inspector|network|logging|performance|memory>]`：

- **Widget Inspector**：支持搜索与切换实现层 Widget 的组件树、**Layout Explorer**（实时编辑 Flex 属性）、**Properties**、**Details Tree**，以及严格受开关控制的 **Select Widget Mode**（仅开启后在应用内点选组件才跳转 Zed 源码）。
- **Network**：同时捕获 `dart:io` HTTP 请求与 Dart 3.14+ WebSocket 连接及收发帧；完整展示请求/响应头与正文（自动 gzip 解压 + JSON 语法高亮），支持任意文本选中复制、一键复制完整请求信息、状态/类型筛选（`All` / `2xx` / `4xx/5xx` / `Pending` / `JSON` / `WS`）与列标题三态排序（升序 → 降序 → 恢复默认顺序）。
- **Logging**：实时订阅 VM Service 日志流（`Stdout` / `Stderr` / `FlutterError` / `FlutterInfo` / `Log` / `GC`），选中某条日志自动锁定视图不随新日志滑走（超出环形缓冲上限仍保留选中详情），点击日志中的 Dart 位置可在当前 Zed 项目窗口直接跳转。
- **Performance & Memory**：实时展示 UI/Raster 帧耗时与卡顿（Jank）统计，以及 Isolate 堆内存、进程 RSS 指标和手动 GC 触发。

---

## 配置项参考

### LSP `initialization_options`

| 配置项 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `autoHotReloadOnSave` | `bool` | `true` | 保存非测试 `.dart` 文件时自动防抖热重载 |
| `hotReloadDebounceMs` | `number` | `250` | 保存热重载防抖间隔（毫秒） |
| `closingLabels` | `bool` | `true` | 通过 `inlayHint` 显示 Widget 闭合标签 |
| `closingLabelsMinLines` | `number` | `2` | 触发显示闭合标签的最小代码行数 |
| `colorPreviews` | `bool` | `true` | 启用 Flutter 颜色预览与取色器 |
| `enablePubVersionHints` | `bool` | `false` | 在 `pubspec.yaml` 中显示 `pub.dev` 版本提示（默认关闭以与 `pubgrade` 零冲突） |
| `exposeGlobalCodeActions` | `bool` | `false` | 在 `Cmd+.` Code Actions 中暴露全局 Flutter 命令 |
| `flutterSdkPath` | `string?` | `null` | 自定义 Flutter SDK 路径（覆盖 `PATH` 与 FVM 自动检测） |
| `pubDevTimeoutSecs` | `number` | `5` | 启用版本提示时查询 `pub.dev` 的超时时间（秒） |
| `deviceCacheTtlSecs` | `number` | `60` | 设备列表磁盘缓存有效期（秒） |
| `pubCacheTtlSecs` | `number` | `3600` | `pub.dev` 版本查询缓存有效期（秒） |

### DAP `.zed/debug.json` 配置项（`adapter: "Flutter"`）

| 配置项 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `request` | `"launch" \| "attach"` | `"launch"` | 启动新应用/测试或附加到已有 VM Service |
| `program` | `string` | `"lib/main.dart"` | 入口文件或测试文件路径（支持 `$ZED_RELATIVE_FILE`） |
| `isTest` | `bool` | `false` | 强制以 `--test` 模式启动（`*_test.dart`、`test/`、`integration_test/` 自动识别） |
| `cwd` | `string?` | 自动推断 | 含 `pubspec.yaml` 的项目目录（Monorepo 子包自动解析） |
| `deviceId` | `string?` | 当前选中设备 | 指定目标设备 ID（省略时沿用 `ctl select-device` 或自动回退） |
| `flutterMode` | `"debug" \| "profile" \| "release"` | `"debug"` | Flutter 运行模式 |
| `flavor` | `string?` | `null` | 透传 `--flavor` |
| `dartDefineFromFile` | `string?` | `null` | 透传 `--dart-define-from-file` |
| `args` / `toolArgs` | `string[]` | `[]` | 应用运行时参数 / 附加给 Flutter/Dart CLI 的参数 |
| `vmServiceUri` | `string?` | `null` | `attach` 模式的 VM Service URI |
| `debugExternalPackageLibraries` | `bool` | `false` | 单步调试时是否进入第三方 pub 包源码 |
| `debugSdkLibraries` | `bool` | `false` | 单步调试时是否进入 Dart/Flutter SDK 源码 |
| `noDebug` | `bool` | `false` | 以无断点模式启动 |

---

## `ctl` CLI 速查

```bash
# 会话控制
flutter-debugger-server ctl hot-reload [--project <dir>]
flutter-debugger-server ctl hot-restart [--project <dir>]
flutter-debugger-server ctl stop [--project <dir>]
flutter-debugger-server ctl status [--project <dir>]
flutter-debugger-server ctl screenshot [--out <path>]

# DevTools 与调试浮层
flutter-debugger-server ctl devtools [--page <inspector|performance|memory|network|logging|cpu-profiler>]
flutter-debugger-server ctl devtools-ui [--project <dir>] [--page <inspector|network|logging|performance|memory>]
flutter-debugger-server ctl overlay <inspector|performance|debug-paint|baselines|repaint|slow-anim|banner|oversized|platform|brightness>

# 设备与模拟器
flutter-debugger-server ctl devices
flutter-debugger-server ctl select-device [device_id]
flutter-debugger-server ctl emulators
flutter-debugger-server ctl launch-emulator [emulator_id]

# 运行与工程工作流
flutter-debugger-server ctl run [--mode debug|profile|release] [--target lib/main.dart] [--device <id>] [--flavor <f>] [--dart-define-from-file <file>]
flutter-debugger-server ctl build-runner <build|watch>
flutter-debugger-server ctl pub-get | clean | doctor | create
flutter-debugger-server init-zed
flutter-debugger-server ctl print-zed-config
```

---

## 本地开发与校验

```bash
# 全量单测与 Clippy 零警告校验
cargo clippy --target wasm32-wasip2 --all-targets -- -D warnings
cargo test --manifest-path crates/protocol/Cargo.toml && cargo clippy --manifest-path crates/protocol/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path server/Cargo.toml && cargo clippy --manifest-path server/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path devtools-gpui/Cargo.toml && cargo clippy --manifest-path devtools-gpui/Cargo.toml --all-targets -- -D warnings

# 构建并安装 Release 二进制至 ~/.local/bin
./scripts/install-local.sh
```

## 开源协议

基于 [Apache License 2.0](./LICENSE) 开源。
