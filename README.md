# Flutter Debugger for Zed

[简体中文](README.zh-CN.md)

Flutter debugging, quick Debug/Profile/Release launching, automatic DevTools Performance view, Hot Reload, target device switching, visual debug overlays, test runner, project creation wizard, and Pub dependency workflows for the [Zed](https://zed.dev) editor. Designed to work alongside Zed's official **Dart** extension without interfering with Dart syntax highlighting, diagnostics, completion, or formatting.

## Features

- **Quick `main()` Launchers (`Debug` / `Profile` / `Release`)**: In Flutter projects, `main()` displays clean CodeLens buttons (`Debug`, `Profile`, `Release`) modeled after VS Code's Flutter extension:
  - **Debug**: Launches `flutter run --debug --machine` with live log streaming, automatic Hot Reload on save, `Hot Reload`, `Hot Restart`, `DevTools`, and `Stop`.
  - **Profile**: Launches `flutter run --profile --machine` and automatically opens the official **Flutter DevTools Performance** view once the VM Service is ready.
  - **Release**: Launches `flutter run --release --machine` with live log streaming.
- **Live Run Log Control Bar & Reliable Stop**: While a Flutter app is running, line 1 of the live run log tab displays `Hot Reload`, `Hot Restart`, `DevTools` / `Performance`, and `Stop`, and every `.dart` file exposes `Flutter: Stop App` at the top of Code Actions (`Cmd+.` / `Ctrl+.`). Stopping gracefully requests `app.stop` and terminates the full process tree on timeout or LSP shutdown.
- **Full Breakpoint Debugging (DAP)**: Full `.zed/debug.json` schema support (`deviceId`, `flutterMode`, `flavor`, `dartDefineFromFile`, `toolArgs`, `env`), one-click `.zed/debug.json` generation (`Flutter: Generate .zed/debug.json`), plus automatic task-to-debug conversion for `flutter run`, `flutter test`, `dart run`, and `dart test`. Launching with `flutterMode: "profile"` via `F4` also opens Flutter DevTools Performance automatically.
- **Visual Debug Overlays & Screenshots**: While an app is running in Debug or Profile mode, toggle `Performance Overlay`, `Debug Paint`, `Repaint Rainbow`, `Slow Animations`, `Debug Banner`, and `Target Platform` (`ext.flutter.platformOverride`), or capture a device screenshot (`Flutter: Save Screenshot`) directly from Code Actions (`Cmd+.` / `Ctrl+.`).
- **Test Runner & File Navigation**: Clean CodeLens (`Run Test` / `Run Group`) on `test()`, `testWidgets()`, and `group()` in `*_test.dart` files (automatically invoking `flutter test` or `dart test`), plus `Dart: Go to Test File` / `Dart: Go to Implementation File` in Code Actions.
- **Pub Dependency Workflow (`pubspec.yaml` / `pubspec_overrides.yaml`)**:
  - Top-of-file actions stay strictly dependency-focused: `Pub Get`, `Pub Upgrade`, and `Pub Outdated`.
  - `Cmd+Click` (macOS) / `Ctrl+Click` (Windows/Linux) any hosted package name to open its page on `pub.dev`.
  - Cached version hints (`latest: x.y.z`) and one-click upgrade Code Actions.
- **Devices, Emulators, Clean, Doctor & Project Wizard**:
  - Cycle target devices from `main()` (`Device: ...` / `Refresh Devices`) or Dart Code Actions (`Flutter: Switch Target Device`), persisted per project and automatically injected into both quick runs and DAP launches.
  - Trigger `Flutter: Launch Emulator`, `Flutter: Clean Project`, `Flutter: Run Doctor`, or `Flutter: New Project Wizard` from Dart Code Actions (`Cmd+.` / `Ctrl+.`).
- **FVM & Monorepo Aware**: Automatically detects `.fvm/flutter_sdk` and resolves the nearest `pubspec.yaml` per file (including `pubspec_overrides.yaml`), distinguishing Flutter apps/plugins from pure Dart packages.

## Prerequisites

- [Flutter SDK](https://docs.flutter.dev/get-started/install) and/or [Dart SDK](https://dart.dev/get-dart) available on `PATH` (or configured via FVM / `settings.json`).
- Zed's official **Dart** extension installed for Dart syntax highlighting and Dart Analysis Server LSP.

## Installation

1. Open Zed Extensions (`Cmd+Shift+X` on macOS or `Ctrl+Shift+X` on Linux/Windows).
2. Search for **Flutter Debugger** and click **Install**.

On first activation, the extension automatically downloads the matching `flutter-debugger-server` binary for your platform from GitHub Releases if it is not already on your `PATH`.

## Recommended Configuration

Add the following to your Zed `settings.json` (`Cmd+,` / `Ctrl+,`) to enable Inlay Hints and configure Flutter Debugger alongside the official Dart extension:

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

Apache 2.0 — see [LICENSE](LICENSE).
