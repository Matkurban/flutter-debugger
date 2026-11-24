# Flutter Debugger for Zed

面向 [Zed](https://zed.dev) 的全功能 Flutter / Dart 开发、`pubspec.yaml` 依赖管理、断点调试与原生 DevTools 套件，致力于提供完整的类 VS Code Flutter (`Dart-Code`) 日常开发体验，并与 Zed 官方 `dart` 扩展零冲突协同。

> [English README](./README.md) · [版本发布 Releases](https://github.com/Matkurban/flutter-debugger/releases)

---

## 核心特性

- **内置 `pubspec.yaml` 依赖与资源智能管理（默认开启）**：
  - **可点击内联版本提示（`inlayHint`）**：行尾显示 `✓ latest`、`⇄ 5.9.0 (locked)`（点击同步 `pubspec.yaml` 约束至 `pubspec.lock`）、`locked 5.9.0 · ⬆ 6.0.0 (major)`、`⬆ 5.9.0`、`⬆ 6.0.0 (major)`、`⬆ 6.0.0 (major, SDK)`、`⚠ retracted → ...` 与 `⚠ discontinued`，点击过期或锁漂移提示即可直接就地更新版本号。
  - **富文本悬停卡片（`textDocument/hover`）**：展示包描述、Pub Points / 点赞数 / 30 天下载量、声明版本 vs `pubspec.lock` 锁定版本 vs 最新兼容/总体版本、Dart SDK 兼容性提示以及懒加载缓存的 `pub.dev` 更新日志（Changelog）；同时支持离线悬停查看本地 `path:` / `sdk:` / `git:` 依赖与 `flutter.assets` / `flutter.fonts` 磁盘状态。
  - **依赖版本自动补全（`textDocument/completion`）**：在 `pubspec.yaml` / `pubspec_overrides.yaml` 中输入 `:` 或 `^` 即触发版本补全，自动保留原有引号与 `^` 前缀，并标注 `latest`、`locked`、发布日期与 SDK 兼容性。
  - **统一诊断、一键升级与细粒度忽略（`Cmd+.`）**：将大版本/小版本/补丁/预发布/撤回/已停维护（含 `DEPRECATED` 删除线）以及缺失资源/字体诊断原子合并发布；支持单包与批量升级，以及一键忽略规则写入 `<workspace>/.zed/pubspec-ignore.json`（不污染 `pubspec.yaml`）：支持忽略单版本（`=X.Y.Z`）、忽略某版本及之后所有版本（`>=X.Y.Z`）或彻底忽略该包（`*`）。
  - **保存自动 `pub get` 与 `pubspec.lock` 实时同步**：保存 `pubspec.yaml` 时通过依赖与 SDK 约束指纹比对，**仅在依赖真正变化时**调用项目 FVM / 自定义 SDK 自动执行 `<flutter|dart> pub get`；同时通过动态文件监听 `**/pubspec.lock` 实时同步外部命令行执行结果。
  - **顶部 CodeLens 与可点击链接**：提供 `Refresh (N updates)`、`<flutter|dart> pub get`、`pub upgrade`、`pub outdated` 一键入口，以及直达 `pub.dev`（支持 `PUB_HOSTED_URL` / `pubHostedUrl` 镜像与私服）、本地 `path:` 包与本地资源/字体的 `Cmd+点击` 链接。
- **Dart LSP 编辑器增强**：Widget 闭合标签（Closing Labels）、Flutter 颜色预览与取色器、`main()` 与测试文件 CodeLens、流式 `--machine` 测试运行器（支持嵌套 `group` 路径与 UTF-16 精准失败诊断）与保存防抖自动热重载。
- **DAP 调试代理与 Debug Console 增强**：延迟解析 Monorepo / FVM / `--test`、`flutter daemon` 实时设备热插拔与智能回退选设备、异常断点与 Just-My-Code 开关、Debug Console `zed://file/...:line:col` `Cmd+点击` 精准跳转源码（复用当前 Zed 窗口）、对齐 VS Code 的多状态 ANSI 彩色日志（自动适配 Zed 浅/深色主题）。
- **原生 GPUI DevTools（`flutter-devtools-gpui`）**：独立轻量原生桌面伴侣应用，内置 **Widget Inspector**（Layout Explorer、Properties、Details Tree、Select Widget Mode 跳转源码）、**Network**（HTTP + Dart 3.14+ WebSocket、gzip/JSON 高亮、详情选中与一键复制、列标题三态排序）、**Logging**（VM Service 实时日志、选中防滑走、点击跳转当前 Zed 窗口）、**Performance**（UI/Raster 帧耗时与卡顿统计）与 **Memory**（Isolate 堆内存、RSS 与手动 GC）。
- **`ctl` CLI 与零配置初始化**：交互式设备/模拟器选择、原生终端 `ctl run`、全套 Flutter Service Extensions（`overlay` / `brightness` / `platform` / `screenshot`）、`pub-get` / `pub-upgrade` / `pub-outdated`、直达指定页的 Web/GPUI DevTools，以及无损 `init-zed` 项目配置生成。

---

## 架构分层与零冲突边界

| 模块 | 目录 | 职责 | 零冲突边界 |
|---|---|---|---|
| **官方 `dart` 扩展** | 外部扩展 | Dart 语法高亮、补全、Hover、跳转定义、格式化、重命名、Dart Analysis 诊断 | `flutter-debugger` 在 `.dart` 文件的 `hover` / `completion` 中立即返回 `None`，绝不干扰 |
| **Zed WASM 扩展** | `src/` | 注册 `flutter-debugger` LSP（`Dart` / `YAML`）、`Flutter` DAP 适配器与 `flutter` Debug Locator；合并传递 `binary.env` | 仅在含 `pubspec.yaml` 的目录生效，非 Flutter 目录零后台进程 |
| **共享协议库** | `crates/protocol/` | 会话注册表、IPC 协议、Overlay 状态、Dart URI 与 `zed://file/...` 改写、ANSI 控制台着色 | `server` 与 `devtools-gpui` 共享单一数据源 |
| **后端服务（`flutter-debugger-server`）** | `server/` | 统一 LSP 服务（`Dart` + `pubspec.yaml` / `pubspec_overrides.yaml`）、DAP 代理、`flutter daemon` 监听、VM Service 客户端与 `ctl` CLI | 单进程、共享 FVM/SDK 执行器与持久化磁盘缓存 |
| **原生 GPUI DevTools（`flutter-devtools-gpui`）** | `devtools-gpui/` | 单实例原生 GPUI 桌面 DevTools（Inspector / Network / Logging / Performance / Memory） | 直连当前活跃会话 IPC 与 Dart VM Service |

> [!TIP]
> `pubspec.yaml` 依赖管理现已完整原生集成至 `flutter-debugger`（默认开启）。若你此前单独安装过 `pubgrade` 扩展，建议将其卸载或禁用以避免重复的版本提示与诊断。

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
  "code_lens": "on",
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
        "exposeGlobalCodeActions": false,
        "pubspec": {
          "enabled": true,
          "autoPubGetOnSave": true,
          "inlayHints": true,
          "showUpToDateHints": true,
          "codeLens": true,
          "majorSeverity": "warning",
          "minorPatchSeverity": "info"
        }
      }
    }
  }
}
```

> **可选优化（避免 `pubspec.yaml` Hover 与通用 YAML Schema 重复）**：若 `yaml-language-server` 同时在 `pubspec.yaml` 中展示通用 Schema 悬停提示，可在 `settings.json` 中仅对 `pubspec.yaml` 排除 SchemaStore（不影响 GitHub Actions / Docker Compose 等其他 YAML 文件）：
> ```json
> "lsp": {
>   "yaml-language-server": {
>     "settings": {
>       "yaml": {
>         "schemas": {
>           "https://json.schemastore.org/pubspec.json": "!pubspec.yaml"
>         }
>       }
>     }
>   }
> }
> ```

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

### `pubspec.yaml` 与 `pubspec_overrides.yaml` 智能管理
- **完整的 Pub V2 仓库与镜像支持**：遵循 Hosted Pub Repository Specification V2（`Accept: application/vnd.pub.v2+json`）与 `/score` 接口，按 `pubspec.pubHostedUrl` → `PUB_HOSTED_URL` → `https://pub.dev` 优先级解析，兼容单包自定义 `hosted:` 源、HTTP/2 + gzip + 系统代理，以及原子双层缓存（`<cache>/pub_packages` 与 `pub_changelogs`）。
- **精准的 Dart SemVer 与 Lockfile 同步检测**：完整支持 `^`、区间（`>=1.0.0 <2.0.0`）、精确版本、`any` 与空约束；自动读取同级 `pubspec.lock` 并通过 `workspace/didChangeWatchedFiles` 实时同步；当 `flutter pub upgrade` 更新了 `pubspec.lock` 但 `pubspec.yaml` 约束滞后时，自动发射 `lock-drift`（默认 `info`）诊断、行尾显示可点击 `⇄ x.y.z (locked)`、`Cmd+.` 提供单包/批量同步至 `pubspec.lock`，且 Hover 自动按「声明版本 → 锁定/最新版本」展示完整更新日志；同时支持撤回（retracted）、停维护（discontinued）与 Dart `environment.sdk` 兼容性校验。
- **交互式升级与版本补全**：点击任意 `⬆` 内联提示、使用 `Cmd+.` Code Actions（单包兼容/大版本升级、批量升级兼容依赖、批量升级全部至最新版），或在 `:` / `^` 后触发版本自动补全。
- **细粒度版本忽略（`.zed/pubspec-ignore.json`）**：支持忽略单个版本（`=5.9.0`，后续发布新版自动重新提示）、忽略某版本及之后所有版本（`>=6.0.0`，若存在更低兼容更新如 `5.9.0` 仍照常提示，并保留 `discontinued`/`retracted` 警告），或彻底忽略该包全部更新（`*`，连同 `discontinued`/`retracted` 一并静音且不发起网络请求）。规则保存在工作区根目录 `.zed/pubspec-ignore.json`（不修改 `pubspec.yaml`，Monorepo 所有子包共享并自动合并子包规则及 `settings.json` `pubspec.ignorePackages`），支持文件变更实时生效；被静音的依赖行尾完全隐藏 Inlay Hint，可通过 Hover 查看规则与真实最新版，或按 `Cmd+.` 一键取消忽略 / 打开配置文件。
- **保存自动 `pub get`**：对每个打开的 `pubspec.yaml` 维护依赖与 SDK 约束指纹，修改注释/描述/资源路径不会误触发，仅在依赖或 SDK 约束真正变更并保存时通过项目 FVM / SDK 自动运行 `<flutter|dart> pub get`。
- **资源、字体与本地依赖**：检测缺失的 `flutter.assets` / `flutter.fonts`，提供 `Cmd+点击` 跳转链接，并支持离线悬停查看文件大小、目录文件数量或本地 `path:` 包元信息。

### Dart LSP 编辑器增强
- **Widget 闭合标签（Closing Labels）**：在多行 Widget 构造闭合括号后以 `inlayHint` 显示 `// WidgetName`。
- **Flutter 颜色预览**：支持 `Color(0xAARRGGBB)`、`Color.fromARGB`、`Color.fromRGBO`、`Color.from(...)` 与 `Colors.*` 色板预览和内联改色。
- **`main()` 与测试 CodeLens**：在 `main()` 上方直接提供设备切换、Hot Reload / Hot Restart、Stop 与 Web/原生 DevTools 入口；在 `*_test.dart` 与 `integration_test/` 中提供单项、嵌套 `group()` 与整文件运行/断点调试入口。
- **保存防抖热重载**：保存非测试 `.dart` 文件自动触发热重载并在 Zed 进度条显示耗时（兼容 DAP 调试与 `ctl run` 会话）。

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

### LSP `initialization_options`（顶层配置）

| 配置项 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `autoHotReloadOnSave` | `bool` | `true` | 保存非测试 `.dart` 文件时自动防抖热重载 |
| `hotReloadDebounceMs` | `number` | `250` | 保存热重载防抖间隔（毫秒） |
| `closingLabels` | `bool` | `true` | 通过 `inlayHint` 显示 Widget 闭合标签 |
| `closingLabelsMinLines` | `number` | `2` | 触发显示闭合标签的最小代码行数 |
| `colorPreviews` | `bool` | `true` | 启用 Flutter 颜色预览与取色器 |
| `exposeGlobalCodeActions` | `bool` | `false` | 在 `.dart` 文件的 `Cmd+.` Code Actions 中暴露全局 Flutter 命令 |
| `flutterSdkPath` | `string?` | `null` | 自定义 Flutter SDK 路径（覆盖 `PATH` 与 FVM 自动检测） |
| `deviceCacheTtlSecs` | `number` | `60` | 设备列表磁盘缓存有效期（秒） |
| `pubspec` | `object` | `{ ... }` | 嵌套的 `pubspec.yaml` 依赖管理配置（详见下表） |

### LSP `initialization_options.pubspec`

| 配置项 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | `bool` | `true` | 开启 `pubspec.yaml` / `pubspec_overrides.yaml` 依赖检查、悬停、补全、提示、CodeLens 与快捷操作 |
| `pubHostedUrl` | `string?` | `null` | 自定义 Pub 仓库/镜像地址（依次回退至 `PUB_HOSTED_URL` 环境变量与 `https://pub.dev`） |
| `maxConcurrency` | `number` | `10` | 最大并发查询数（范围 `1..=32`） |
| `requestTimeoutSecs` | `number` | `15` | HTTP 请求超时时间（秒，最小 `3`） |
| `cacheTtlSecs` | `number` | `86400` | 包元数据内存/磁盘缓存有效期（24 小时） |
| `changelogCacheTtlSecs` | `number` | `86400` | Changelog HTML 缓存有效期（24 小时） |
| `includePrerelease` | `bool` | `false` | 即使当前使用稳定版也包含预发布版本 |
| `inlayHints` | `bool` | `true` | 在依赖行尾显示内联版本提示 |
| `showUpToDateHints` | `bool` | `true` | 对已是最新版的依赖显示 `✓ latest` 提示 |
| `codeLens` | `bool` | `true` | 在 `pubspec.yaml` 顶部显示 CodeLens 操作栏 |
| `codeLensRefresh` | `bool` | `true` | 显示 `Refresh (N updates)` CodeLens |
| `codeLensPubGet` | `bool` | `true` | 显示 `<flutter\|dart> pub get` CodeLens |
| `codeLensPubUpgrade` | `bool` | `true` | 显示 `<flutter\|dart> pub upgrade` CodeLens |
| `codeLensPubOutdated` | `bool` | `true` | 显示 `<flutter\|dart> pub outdated` CodeLens |
| `checkDevDependencies` | `bool` | `true` | 检查 `dev_dependencies` |
| `checkDependencyOverrides` | `bool` | `true` | 检查 `dependency_overrides` |
| `autoPubGetOnSave` | `bool` | `true` | 保存时若依赖或 SDK 约束发生变更，自动运行 `<flutter\|dart> pub get` |
| `pubUpgradeTighten` | `bool` | `false` | 点击顶部 `pub upgrade` CodeLens 时是否附加 `--tighten` 自动收紧 `pubspec.yaml` 下界 |
| `maxChangelogVersions` | `number` | `3` | Hover 卡片中最多展示的 Changelog 版本条目数 |
| `majorSeverity` | `"error" \| "warning" \| "info" \| "hint" \| "off"` | `"warning"` | 大版本更新诊断级别 |
| `minorPatchSeverity` | `"error" \| "warning" \| "info" \| "hint" \| "off"` | `"info"` | 小版本 / 补丁 / 预发布更新诊断级别 |
| `lockDriftSeverity` | `"error" \| "warning" \| "info" \| "hint" \| "off"` | `"info"` | `pubspec.lock` 领先于 `pubspec.yaml` 声明下界时的诊断级别 |
| `ignorePackages` | `string[]` | `[]` | 跳过版本检查与诊断的包名列表 |

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
flutter-debugger-server ctl pub-get | pub-upgrade [--tighten] | pub-outdated | clean | doctor | create
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
