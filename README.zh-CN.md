# Flutter Debugger for Zed

[English](README.md)

为 [Zed](https://zed.dev) 编辑器打造的 Flutter 调试、快捷 Debug/Profile/Release 运行、Profile 性能面板自动拉起、热重载、运行中调试视图开关、目标设备切换、测试运行、项目创建向导与 Pub 依赖管理扩展。专为与 Zed 官方 **Dart** 扩展和平共处而设计，不破坏原有的 Dart 语法高亮、诊断、补全与格式化能力。

## 核心功能

- **`main()` 快捷启动（`Debug` / `Profile` / `Release`）**：在 Flutter 项目的 `main()` 上方展示简洁的 CodeLens 按钮（`Debug`、`Profile`、`Release`）：
  - **Debug**：启动 `flutter run --debug --machine`，实时流式输出日志，支持保存自动 Hot Reload、`Hot Reload`、`Hot Restart`、`DevTools` 与 `Stop`。
  - **Profile**：启动 `flutter run --profile --machine`，VM Service 就绪后自动打开官方 **Flutter DevTools Performance** 性能分析面板。
  - **Release**：启动 `flutter run --release --machine` 并输出完整构建运行日志。
- **实时运行日志顶部控制条与可靠停止**：应用运行期间，实时日志标签页第 1 行直接展示 `Hot Reload`、`Hot Restart`、`DevTools`/`Performance` 与 `Stop`，且任意 `.dart` 文件的 Code Actions（`Cmd+.` / `Ctrl+.`）均置顶 `Flutter: Stop App`。停止时优先优雅请求 `app.stop`，超时或退出 LSP 时自动强杀完整进程树，避免残留进程。
- **完整断点调试（DAP）**：支持 `.zed/debug.json` 完整配置补全与校验（`deviceId`、`flutterMode`、`flavor`、`dartDefineFromFile`、`toolArgs`、`env`）、一键生成 `.zed/debug.json`（`Flutter: Generate .zed/debug.json`），以及任务一键转调试；通过 `F4` 以 `flutterMode: "profile"` 启动时同样自动打开 DevTools Performance 面板。
- **运行中调试视图开关与截图**：在 Debug/Profile 会话期间，可通过 Code Actions 一键切换 `Performance Overlay`、`Debug Paint`、`Repaint Rainbow`、`Slow Animations`、`Debug Banner`、`Target Platform`（`ext.flutter.platformOverride`），或执行 `Flutter: Save Screenshot` 保存设备截图。
- **测试一键运行与文件互跳**：自动识别 `*_test.dart` 中的 `test()`、`testWidgets()` 与 `group()` 提供 `Run Test` / `Run Group`，并支持通过 Code Actions 在 `lib/*.dart` 与 `test/*_test.dart` 之间一键互跳。
- **Pub 依赖工作流（`pubspec.yaml` / `pubspec_overrides.yaml`）**：
  - 文件顶部严格只展示依赖相关操作：`Pub Get`、`Pub Upgrade`、`Pub Outdated`、`Build Runner`（存在该依赖时）与 `Refresh Versions`。
  - 按住 `Cmd+Click`（macOS）/ `Ctrl+Click`（Windows/Linux）点击包名直接打开 `pub.dev` 详情页。
  - 缓存版依赖更新提示（`latest: x.y.z`）与一键升级 Code Action。
- **设备切换、模拟器启动、Clean、Doctor 与新建项目向导**：
  - 在 `main()` 上方（`Device: ...` / `Refresh Devices`）或 Dart 文件 Code Actions 中切换目标设备并按项目持久化，自动注入快捷运行与 DAP 调试。
  - 在 Dart 文件 Code Actions 中触发 `Flutter: Launch Emulator`、`Flutter: Clean Project`、`Flutter: Run Doctor` 或 `Flutter: New Project Wizard`（打开可编辑的 `flutter-project-wizard.json` 并一键创建）。
- **FVM 与 Monorepo 兼容**：优先识别项目内 `.fvm/flutter_sdk`，按当前文件逐级向上定位最近的 `pubspec.yaml`（编辑 `pubspec_overrides.yaml` 自动参考同级 `pubspec.yaml`），精准区分 Flutter 应用/插件与纯 Dart 包。

## 环境要求

- 系统 `PATH` 已安装 [Flutter SDK](https://docs.flutter.dev/get-started/install) 和/或 [Dart SDK](https://dart.dev/get-dart)（或使用 FVM / 在 `settings.json` 中指定路径）。
- 已安装 Zed 官方 **Dart** 扩展以获得 Dart 语法高亮与 Dart Analysis Server LSP 能力。

## 安装方式

1. 打开 Zed 扩展商店（macOS 按 `Cmd+Shift+X`，Linux/Windows 按 `Ctrl+Shift+X`）。
2. 搜索 **Flutter Debugger** 并点击 **Install**。

首次激活时，若系统 `PATH` 中未找到 `flutter-debugger-server`，扩展会自动按当前操作系统与 CPU 架构从 GitHub Releases 下载并缓存对应二进制文件。

## 推荐配置

在 Zed 的 `settings.json`（`Cmd+,` / `Ctrl+,`）中添加以下配置，以确保与官方 Dart 扩展协同工作并开启 Inlay Hints 与运行日志控制条：

```json
{
  "inlay_hints": {
    "enabled": true
  },
  "languages": {
    "Dart": {
      "format_on_save": "on",
      "language_servers": ["dart", "flutter-debugger"]
    },
    "YAML": {
      "language_servers": ["flutter-debugger", "..."]
    },
    "JSON": {
      "language_servers": ["flutter-debugger", "..."]
    },
    "Log": {
      "language_servers": ["flutter-debugger", "..."]
    },
    "Plain Text": {
      "language_servers": ["flutter-debugger", "..."]
    }
  },
  "lsp": {
    "flutter-debugger": {
      "settings": {
        "autoHotReloadOnSave": true,
        "flutterSdkPath": null,
        "deviceCacheTtlSecs": 3600,
        "pubCacheTtlSecs": 43200
      }
    }
  }
}
```

### `.zed/debug.json` 示例

```json
[
  {
    "adapter": "Flutter",
    "label": "Flutter Debug (Staging)",
    "request": "launch",
    "program": "lib/main.dart",
    "flutterMode": "debug",
    "flavor": "staging",
    "dartDefineFromFile": "env/staging.json"
  },
  {
    "adapter": "Flutter",
    "label": "Flutter Attach",
    "request": "attach"
  }
]
```

## 开源协议

MIT — 详见 [LICENSE](LICENSE)。
