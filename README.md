# Flutter Debugger for Zed

[简体中文](README.zh-CN.md)

Flutter debugging, Hot Reload, target device switching, test runner, and Pub dependency workflows for the [Zed](https://zed.dev) editor. Designed to work alongside Zed's official **Dart** extension without interfering with Dart syntax highlighting, diagnostics, completion, or formatting.

## Features

- **Flutter & Dart Debugging (DAP)**: Full `.zed/debug.json` schema support (`deviceId`, `flutterMode`, `flavor`, `dartDefineFromFile`, `toolArgs`, `env`) plus automatic task-to-debug conversion for `flutter run`, `flutter test`, `dart run`, and `dart test`.
- **Automatic Hot Reload on Save**: Saves in `.dart` files automatically trigger Hot Reload on active debug sessions (disabled in `profile` and `release` modes). Manual **Hot Reload**, **Hot Restart**, and **Open DevTools** actions are also available via CodeLens on `main()` and Code Actions (`Cmd+.` / `Ctrl+.`).
- **Device Discovery & Switching**: Caches `flutter devices --machine` locally so opening files stays fast. Cycle target devices directly from CodeLens (`◎ Device`) and refresh manually whenever needed (`↻ Refresh Devices`). Selected devices are persisted per project and automatically injected into debug launches.
- **Test Runner**: Monochrome CodeLens (`▷ Run Test` / `▷ Run Group`) on `test()`, `testWidgets()`, and `group()` in `*_test.dart` files, automatically invoking `flutter test` for Flutter projects and `dart test` for pure Dart packages.
- **Pub Dependency Workflow**:
  - `Cmd+Click` (macOS) / `Ctrl+Click` (Windows/Linux) any hosted package name in `pubspec.yaml` or `pubspec_overrides.yaml` to open its page on `pub.dev`.
  - Cached version hints (`↑ x.y.z`) and one-click upgrade Code Actions.
  - Top-of-file actions for `pub get`, `pub upgrade`, `pub outdated`, `build_runner`, and `flutter doctor` with live status bar progress and full ANSI-cleaned log output opened in a new Zed tab on failure or reports.
- **FVM & Monorepo Aware**: Automatically detects `.fvm/flutter_sdk` and resolves the nearest `pubspec.yaml` per file (including `pubspec_overrides.yaml`), distinguishing Flutter apps/plugins from pure Dart packages.

## Prerequisites

- [Flutter SDK](https://docs.flutter.dev/get-started/install) and/or [Dart SDK](https://dart.dev/get-dart) available on `PATH` (or configured via FVM / `settings.json`).
- Zed's official **Dart** extension installed for Dart syntax highlighting and Dart Analysis Server LSP.

## Installation

1. Open Zed Extensions (`Cmd+Shift+X` on macOS or `Ctrl+Shift+X` on Linux/Windows).
2. Search for **Flutter Debugger** and click **Install**.

On first activation, the extension automatically downloads the matching `flutter-debugger-server` binary for your platform from GitHub Releases if it is not already on your `PATH`.

## Recommended Configuration

Add the following to your Zed `settings.json` (`Cmd+,` / `Ctrl+,`) to enable Inlay Hints and customize Flutter Debugger alongside the official Dart extension:

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

### Example `.zed/debug.json`

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

## License

MIT — see [LICENSE](LICENSE).
