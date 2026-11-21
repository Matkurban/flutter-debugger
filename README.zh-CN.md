# Flutter Debugger for Zed

[English](README.md)

为 [Zed](https://zed.dev) 编辑器打造的 Flutter 调试、快捷 Debug/Profile/Release 运行、Profile 性能面板自动拉起、热重载、目标设备切换、测试运行、项目创建向导与 Pub 依赖管理扩展。专为与 Zed 官方 **Dart** 扩展和平共处而设计，不破坏原有的 Dart 语法高亮、诊断、补全与格式化能力。

## 核心功能

- **`main()` 快捷启动（`Debug` / `Profile` / `Release`）**：在 Flutter 项目的 `main()` 上方展示简洁的 CodeLens 按钮（`Debug`、`Profile`、`Release`）：
  - **Debug**：启动 `flutter run --debug --machine`，实时流式输出日志，支持保存自动 Hot Reload、`Hot Reload`、`Hot Restart`、`DevTools` 与 `Stop`。
  - **Profile**：启动 `flutter run --profile --machine`，VM Service 就绪后自动打开官方 **Flutter DevTools Performance** 性能分析面板。
  - **Release**：启动 `flutter run --release --machine` 并输出完整构建运行日志。
- **完整断点调试（DAP）**：支持 `.zed/debug.json` 完整配置补全与校验（`deviceId`、`flutterMode`、`flavor`、`dartDefineFromFile`、`toolArgs`、`env`），并支持将 `flutter run`、`flutter test`、`dart run`、`dart test` 任务一键转为调试会话。
- **Flutter 项目创建向导**：点击 `pubspec.yaml` 顶部的 `New Project`（或在 Code Actions `Cmd+.` / `Ctrl+.` 中触发 `Flutter: New Project Wizard`），即可打开可编辑的 `flutter-project-wizard.json` 向导面板，配置项目名、组织名、模板（`app`/`package`/`plugin`/`plugin_ffi`/`module`/`skeleton`）、目标平台、描述与输出目录，点击顶部 `Create Flutter Project` 一键创建并自动打开新项目。
- **目标设备发现与切换**：`flutter devices --machine` 扫描结果自动落盘缓存，首开文件不重复扫描、不闪烁进度条。点击 `Device` CodeLens 可切换目标设备并按项目持久化，自动注入快捷运行与 DAP 调试；随时可通过 `Refresh Devices` 手动刷新。
- **测试一键运行**：自动识别 `*_test.dart` 中的 `test()`、`testWidgets()` 与 `group()`，提供简洁的 CodeLens（`Run Test` / `Run Group`），并按项目类型自动调用 `flutter test` 或 `dart test`。
- **Pub 依赖工作流**：
  - 在 `pubspec.yaml` 或 `pubspec_overrides.yaml` 中按住 `Cmd+Click`（macOS）/ `Ctrl+Click`（Windows/Linux）点击包名直接打开 `pub.dev` 详情页。
  - 缓存版依赖更新提示（`latest: x.y.z`）与一键升级 Code Action。
  - 文件顶部提供 `pub get`、`pub upgrade`、`pub outdated`、`build_runner`、`flutter doctor` 快捷入口；耗时操作在 Zed 底部状态栏显示实时进度，失败或报表命令会自动去除 ANSI 颜色码并在 Zed 新标签页打开完整日志。
- **FVM 与 Monorepo 兼容**：优先识别项目内 `.fvm/flutter_sdk`，按当前文件逐级向上定位最近的 `pubspec.yaml`（编辑 `pubspec_overrides.yaml` 自动参考同级 `pubspec.yaml`），精准区分 Flutter 应用/插件与纯 Dart 包。

## 环境要求

- 系统 `PATH` 已安装 [Flutter SDK](https://docs.flutter.dev/get-started/install) 和/或 [Dart SDK](https://dart.dev/get-dart)（或使用 FVM / 在 `settings.json` 中指定路径）。
- 已安装 Zed 官方 **Dart** 扩展以获得 Dart 语法高亮与 Dart Analysis Server LSP 能力。

## 安装方式

1. 打开 Zed 扩展商店（macOS 按 `Cmd+Shift+X`，Linux/Windows 按 `Ctrl+Shift+X`）。
2. 搜索 **Flutter Debugger** 并点击 **Install**。

首次激活时，若系统 `PATH` 中未找到 `flutter-debugger-server`，扩展会自动按当前操作系统与 CPU 架构从 GitHub Releases 下载并缓存对应二进制文件。

## 推荐配置

在 Zed 的 `settings.json`（`Cmd+,` / `Ctrl+,`）中添加以下配置，以确保与官方 Dart 扩展协同工作并开启 Inlay Hints：

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
