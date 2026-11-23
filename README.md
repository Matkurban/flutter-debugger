# Flutter Debugger for Zed (Personal Dev Extension Suite)

A personal Zed Dev Extension suite that brings a VS Code Flutter (`Dart-Code`) daily development and debugging experience to Zed — designed for zero-conflict coexistence with Zed's official `dart` extension and `pubgrade`.

> [中文文档](./README.zh-CN.md)

## Architecture & Coexistence Boundaries

| Layer | Responsibility | Zero-Conflict Boundary |
|---|---|---|
| **Official `dart` Extension** | Syntax highlighting, completions, hover, go-to-definition, formatting, rename, Dart Analysis diagnostics | Never duplicated by `flutter-debugger` |
| **`pubgrade` Extension** | `pubspec.yaml` dependency version hints, outdated checks & upgrades | `enablePubVersionHints` defaults to `false` so there is zero duplicate hint/CodeLens |
| **`flutter-debugger` LSP** (`["Dart", "YAML"]`) | **Widget Closing Labels** (`inlayHint`), **Color Previews** (`documentColor`/`colorPresentation`), **Streaming `--machine` Test Runner** with nested `group` paths & failure diagnostics, **Debounced Auto Hot Reload on Save**, `pubspec.yaml` **asset/font validation & local path links**, and clean `Cmd+.` Code Actions | Scoped strictly to Dart & YAML; zero background processes outside Dart/Flutter workspaces |
| **`flutter-debugger` DAP + `ctl` CLI** | Delayed Monorepo/FVM/`--test` DAP proxy, multi-session registry with PID liveness checks, real-time `flutter daemon` hotplug, full **Service Extensions** (Widget Inspector, Overlays, Brightness, Platform Override), page-targeted **DevTools**, **Screenshots**, native terminal `ctl run`, and lossless `init-zed` | Works identically across `F4` breakpoint debugging, `attach`, and native terminal `ctl run` |

## One-Command Local Install

```bash
./scripts/install-local.sh
```

Then in Zed:
1. Open Command Palette -> `zed: install dev extension` -> select this repository directory.
2. Make sure `~/.local/bin` is on your `PATH` (or set `lsp.flutter-debugger.binary.path` in `settings.json`).
3. Inside any Flutter project, run:

```bash
flutter-debugger-server init-zed
```

This losslessly creates/merges `.zed/debug.json` (auto-detecting Gradle/iOS flavors, `lib/main*.dart`, and `--test` breakpoint configs) and `.zed/tasks.json` (complete `ctl` task suite).

## Recommended Zed Configuration

Run `flutter-debugger-server ctl print-zed-config` anytime to print these snippets.

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

### `~/.config/zed/keymap.json` (VS Code-style Flutter Shortcuts)

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

## Daily Workflow Cheatsheet

- **Breakpoint Debugging (App or `*_test.dart`)**: Press `F4` -> pick `Flutter: Debug` or `Flutter: Current Test (Breakpoints)` (automatically passes `--test` to `flutter debug_adapter`).
- **Native Terminal Run (ANSI logs + Hot Reload)**: Run task `Flutter: Run (Debug)` (`flutter-debugger-server ctl run --mode debug`). Saving any non-test `.dart` file triggers debounced Hot Reload with elapsed ms in status progress.
- **Select Device / Launch Emulator**: `cmd-alt-d` / `cmd-alt-e` opens an interactive numbered selector in Zed's terminal.
- **Widget Inspector / Overlays / Brightness**: `cmd-alt-i`, `cmd-alt-b`, or `flutter-debugger-server ctl overlay <inspector|performance|debug-paint|baselines|repaint|slow-anim|banner|oversized|platform|brightness>`.
- **DevTools**: `cmd-alt-p` or `flutter-debugger-server ctl devtools --page <inspector|performance|memory|network|logging|cpu-profiler>`.
- **Build Runner**: Run task `Flutter: Build Runner Watch` or `flutter-debugger-server ctl build-runner watch`.
