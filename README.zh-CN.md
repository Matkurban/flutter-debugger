# Flutter Debugger for Zed（个人本地 Dev Extension 套件）

面向个人本地自用的 Zed Flutter 开发与调试扩展套件，致力于在 Zed 中提供接近 VS Code Flutter (`Dart-Code`) 的日常开发体验，并与 Zed 官方 `dart` 扩展及 `pubgrade` 扩展零冲突协同。

> [English README](./README.md)

## 架构分层与扩展边界

| 层次 | 职责 | 零冲突边界 |
|---|---|---|
| **官方 `dart` 扩展** | 语法高亮、补全、Hover、跳转定义、格式化、重命名、Dart Analysis 诊断 | 绝不重复实现 |
| **`pubgrade` 扩展** | `pubspec.yaml` 依赖版本提示、过期检测与升级 | `enablePubVersionHints` 默认 `false`，避免重复提示 |
| **`flutter-debugger` LSP**（仅挂载 `["Dart", "YAML"]`） | **Widget 闭合标签 Closing Labels**（`inlayHint`）、**Flutter 颜色预览**（`documentColor`/`colorPresentation`）、**`--machine` 流式测试运行器**（支持嵌套 `group` 完整路径与失败诊断）、**防抖自动热重载**、`pubspec.yaml` **资源/字体路径诊断与跳转**、纯净 `Cmd+.` Code Actions | 仅在含 `pubspec.yaml` 的项目生效；非 Flutter 目录零后台进程 |
| **`flutter-debugger` DAP + `ctl` CLI** | 延迟解析 Monorepo/FVM/`--test` 的 DAP 代理、多会话 + PID 探活注册表、`flutter daemon` 实时设备热插拔、全套 **Service Extensions**（Widget Inspector / Overlays / 明暗切换 / 平台切换）、直达指定页面的 **DevTools**、截图、原生终端 `ctl run`、无损 `init-zed` 配置生成器 | `F4` 断点调试、`attach` 与原生终端 `ctl run` 享受完全一致的控制体验 |

## 一键本地构建与安装

```bash
./scripts/install-local.sh
```

随后在 Zed 中完成初始化：
1. 打开命令面板 -> `zed: install dev extension` -> 选择本仓库目录。
2. 确保 `~/.local/bin` 已加入 `PATH`（或在 `settings.json` 中配置 `lsp.flutter-debugger.binary.path`）。
3. 在任意 Flutter 项目根目录执行：

```bash
flutter-debugger-server init-zed
```

该命令会自动扫描 Android Gradle / iOS Scheme 的 flavors 与 `lib/main*.dart`，无损生成或合并 `.zed/debug.json`（含 `Flutter: Current Test (Breakpoints)`）与 `.zed/tasks.json`。

## 推荐 Zed 配置

可随时运行 `flutter-debugger-server ctl print-zed-config` 打印配置片段。

### `~/.config/zed/settings.json`

```json
{
  "lsp_document_colors": "inlay",
  "inlay_hints": {
    "enabled": true
  },
  "lsp": {
    "flutter-debugger": {
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

## 日常高频工作流

- **断点调试（App 或 `*_test.dart`）**：按 `F4` 选择 `Flutter: Debug` 或 `Flutter: Current Test (Breakpoints)`（自动为测试文件加 `--test` 启动 `debug_adapter`）。
- **原生终端运行（完整 ANSI 颜色 + 保存热重载）**：执行任务 `Flutter: Run (Debug)`（`flutter-debugger-server ctl run --mode debug`）。保存非测试 `.dart` 文件自动防抖热重载并在状态栏显示耗时。
- **交互式选设备 / 启模拟器**：按 `cmd-alt-d` / `cmd-alt-e` 在终端按编号选择并持久化。
- **实时调试浮层与明暗主题**：`cmd-alt-i` 开关 Widget Inspector、`cmd-alt-b` 切换 Light/Dark，或运行 `flutter-debugger-server ctl overlay <inspector|performance|debug-paint|baselines|repaint|slow-anim|banner|oversized|platform|brightness>`。
- **精准拉起 DevTools**：`cmd-alt-p` 或 `flutter-debugger-server ctl devtools --page <inspector|performance|memory|network|logging|cpu-profiler>`。
- **代码生成**：执行任务 `Flutter: Build Runner Watch` 或 `flutter-debugger-server ctl build-runner watch`。
