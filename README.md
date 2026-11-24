# Flutter Debugger for Zed

A full-featured Flutter & Dart development, `pubspec.yaml` dependency intelligence, debugging, and native DevTools suite for [Zed](https://zed.dev), designed to bring a complete VS Code (`Dart-Code`) workflow to Zed while coexisting cleanly with Zed's official `dart` extension.

> [中文文档](./README.zh-CN.md) · [Releases](https://github.com/Matkurban/flutter-debugger/releases)

---

## Highlights

- **Built-In `pubspec.yaml` Dependency & Asset Intelligence (Enabled by Default)**:
  - **Clickable Inline Version Hints (`inlayHint`)**: `✓ latest`, `⇄ 5.9.0 (locked)` (click to sync `pubspec.yaml` with `pubspec.lock`), `locked 5.9.0 · ⬆ 6.0.0 (major)`, `⬆ 5.9.0`, `⬆ 6.0.0 (major)`, `⬆ 6.0.0 (major, SDK)`, `⚠ retracted → ...`, and `⚠ discontinued` — click an outdated or lock-drifted hint to update directly in place.
  - **Rich Hover (`textDocument/hover`)**: Package description, Pub Points / Likes / 30-day downloads, declared vs `pubspec.lock` vs latest compatible/overall versions, SDK compatibility notes, and lazily cached `pub.dev` changelogs; plus offline Hover cards for local `path:`, `sdk:`, `git:` dependencies, and `flutter.assets` / `flutter.fonts`.
  - **Version Autocompletion (`textDocument/completion`)**: Triggered on `^` or `:` inside `pubspec.yaml` / `pubspec_overrides.yaml`, honoring existing quotes, caret prefix, locked status, and Dart SDK compatibility.
  - **Unified Diagnostics, Quick Fixes & Fine-Grained Ignore (`Cmd+.`)**: Major/minor/patch/prerelease/retracted/discontinued (`DEPRECATED` strikethrough) & missing asset/font diagnostics published atomically; single-package compatible/major upgrades, batch upgrades, plus one-click ignore rules persisted in `<workspace>/.zed/pubspec-ignore.json` without touching `pubspec.yaml` (`=X.Y.Z` single version, `>=X.Y.Z` this & all newer versions, or `*` entire package).
  - **Auto `pub get` on Save & Live `pubspec.lock` Sync**: Automatically runs `<flutter|dart> pub get` (via your project's FVM / custom SDK) **only** when dependency or SDK constraints actually changed on save, and watches `**/pubspec.lock` for external updates.
  - **Actionable CodeLens & Clickable Links**: `Refresh (N updates)`, `<flutter|dart> pub get`, `pub upgrade`, and `pub outdated` CodeLenses at the top of `pubspec.yaml`, plus clickable links to `pub.dev` (or private registries via `PUB_HOSTED_URL` / `pubHostedUrl`), local `path:` packages, and local assets/fonts.
- **Dart LSP Editor Enhancements**: Widget Closing Labels (`inlayHint`), Flutter Color Previews & picker (`documentColor`), `main()` & test CodeLens, streaming `--machine` test runner with nested `group` paths and UTF-16 failure diagnostics, and debounced Auto Hot Reload on save.
- **DAP Proxy & Enhanced Debug Console**: Lazy Monorepo/FVM/`--test` resolution, real-time `flutter daemon` device hotplug & smart fallback selection, exception pause mode & Just-My-Code switches, clickable `zed://file/...:line:col` source links (`Cmd+Click` reuses your active Zed window), and VS Code-style multi-state ANSI console colors (auto-adapting to Zed light & dark themes).
- **Native GPUI DevTools (`flutter-devtools-gpui`)**: Standalone native desktop companion featuring **Widget Inspector** (Layout Explorer, Properties, Details Tree, Select Widget Mode jump-to-source), **Network** (HTTP + Dart 3.14+ WebSocket, gzip/JSON highlighting, selectable text & one-click copy, tri-state column sorting), **Logging** (VM Service streams, pin-on-select, click-to-jump), **Performance** (UI/Raster frame timings & jank), and **Memory** (Isolate heap, RSS & GC).
- **`ctl` CLI & Zero-Config Setup**: Interactive device/emulator selection, native terminal `ctl run`, full Flutter Service Extensions (`overlay`, `brightness`, `platform`, `screenshot`), `pub-get` / `pub-upgrade` / `pub-outdated`, page-targeted Web/GPUI DevTools launch, and lossless `init-zed` project scaffolding.

---

## Architecture & Coexistence Boundaries

| Component | Path | Responsibility | Zero-Conflict Boundary |
|---|---|---|---|
| **Official `dart` Extension** | External | Dart syntax highlighting, completions, hover, go-to-definition, formatting, rename, Dart Analysis diagnostics | `flutter-debugger` returns `None` immediately for `hover` & `completion` on `.dart` files |
| **Zed WASM Extension** | `src/` | Registers `flutter-debugger` LSP (`Dart`, `YAML`), `Flutter` DAP adapter, and `flutter` Debug Locator; forwards `binary.env` overrides | Zero background processes outside projects containing `pubspec.yaml` |
| **Shared Protocol Crate** | `crates/protocol/` | Session registry, IPC protocol, overlay states, Dart URI & `zed://file/...` rewriter, and ANSI console colorizer | Single source of truth shared by `server` and `devtools-gpui` |
| **Backend Server (`flutter-debugger-server`)** | `server/` | Unified LSP server (`Dart` + `pubspec.yaml` / `pubspec_overrides.yaml`), DAP proxy, `flutter daemon` watcher, VM Service client, and `ctl` CLI | Single process, shared FVM/SDK runner & persistent cache |
| **Native GPUI DevTools (`flutter-devtools-gpui`)** | `devtools-gpui/` | Single-instance native GPUI desktop DevTools (Inspector, Network, Logging, Performance, Memory) | Connects directly to active session IPC & Dart VM Service |

> [!TIP]
> Because `pubspec.yaml` dependency management is now natively integrated into `flutter-debugger` (enabled by default), you can uninstall standalone `pubgrade` to avoid duplicate inlay hints or diagnostics.

---

## Installation

### Option 1: One-Command Local Build & Install (Recommended — includes Native GPUI DevTools)

Prerequisites: Rust stable toolchain (`wasm32-wasip2` target installed via `rustup target add wasm32-wasip2`) and Flutter SDK.

```bash
./scripts/install-local.sh
```

This checks & builds the WASM extension, runs all unit tests & Clippy checks, and installs both `flutter-debugger-server` and `flutter-devtools-gpui` into `~/.local/bin` (pass `--server-only` to skip building `devtools-gpui`).

### Option 2: Prebuilt Server Binaries (GitHub Releases)

The WASM extension automatically downloads the matching `flutter-debugger-server` archive from [GitHub Releases](https://github.com/Matkurban/flutter-debugger/releases) if not found in `PATH` or `settings.json`:
- macOS: `macos-aarch64`, `macos-x86_64`
- Linux: `linux-x86_64`, `linux-aarch64`
- Windows: `windows-x86_64`

> Note: `flutter-devtools-gpui` is currently built locally via `./scripts/install-local.sh`.

### Install Extension in Zed

1. Open Command Palette → `zed: install dev extension` → select this repository directory.
2. Ensure `~/.local/bin` is on your `PATH` (on macOS, if Zed GUI doesn't inherit shell `PATH`, set `lsp.flutter-debugger.binary.path` in `~/.config/zed/settings.json`).
3. Inside any Flutter project root, run:
   ```bash
   flutter-debugger-server init-zed
   ```
   This scans Android Gradle / iOS Scheme flavors and `lib/main*.dart`, then losslessly creates or merges `.zed/debug.json` (including `Flutter: Current Test (Breakpoints)`) and `.zed/tasks.json`.

---

## Recommended Zed Configuration

Run `flutter-debugger-server ctl print-zed-config` anytime to print these snippets.

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
        "path": "/Users/<you>/.local/bin/flutter-debugger-server"
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

> **Optional tip for `pubspec.yaml` Hover clarity**: If `yaml-language-server` also displays a generic JSON Schema hover alongside `flutter-debugger`'s rich package card in `pubspec.yaml`, you can optionally exclude `pubspec.yaml` from SchemaStore in your Zed `settings.json`:
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

### `~/.config/zed/keymap.json` (VS Code-Style Shortcuts)

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

## Feature Details

### `pubspec.yaml` & `pubspec_overrides.yaml` Intelligence
- **Full Pub V2 Registry & Mirror Support**: Uses Hosted Pub Repository Specification V2 (`Accept: application/vnd.pub.v2+json`) plus `/score`, respects `pubspec.pubHostedUrl` → `PUB_HOSTED_URL` → `https://pub.dev`, supports per-dependency `hosted:` URLs, HTTP/2 + gzip + system proxy, and atomic two-tier caching (`<cache>/pub_packages` & `pub_changelogs`).
- **Accurate Dart SemVer & Lockfile Sync**: Understands caret (`^`), range (`>=1.0.0 <2.0.0`), exact, `any`, and empty constraints; reads sibling `pubspec.lock` (with live `workspace/didChangeWatchedFiles` sync), detects when `pubspec.lock` is ahead of `pubspec.yaml` (e.g. after `flutter pub upgrade`) with `lock-drift` info diagnostics, clickable `⇄ x.y.z (locked)` inlay hints, single & batch `Cmd+.` sync actions, and `declared → locked` Hover changelogs; checks retracted/discontinued packages and Dart `environment.sdk` compatibility.
- **Interactive Upgrades & Version Completion**: Click any `⬆` inlay hint, trigger `Cmd+.` Quick Fixes (single compatible/major upgrade, batch compatible upgrade, or batch upgrade-all including major), or invoke autocomplete after `:` / `^` to pick from published versions.
- **Fine-Grained Version Ignore (`.zed/pubspec-ignore.json`)**: Mute a single version (`=5.9.0`, re-prompts on newer releases), mute a version and all newer versions (`>=6.0.0`, still surfaces lower compatible updates like `5.9.0` while keeping `discontinued`/`retracted` alerts), or mute all updates for a package (`*`, silences everything including `discontinued`/`retracted` with zero network requests). Rules are stored cleanly in `<workspace>/.zed/pubspec-ignore.json` (shared across Monorepo packages, automatically merged with any subpackage `.zed/pubspec-ignore.json` and `settings.json` `pubspec.ignorePackages`), watched live for edits, hide inline hints when muted, and remain inspectable/recoverable via Hover and `Cmd+.` (`Unignore` / `Open .zed/pubspec-ignore.json`).
- **Auto `pub get` on Save**: Tracks a dependency & SDK constraint fingerprint per open `pubspec.yaml` — saving comment, description, or asset edits never triggers unnecessary `pub get`, while adding/changing a dependency runs `<flutter|dart> pub get` automatically using your project's FVM / SDK.
- **Assets, Fonts & Local Packages**: Missing `flutter.assets`/`flutter.fonts` warnings, clickable `DocumentLink`s, and offline Hover cards showing local file size / directory file count or local `path:` package metadata.

### Dart LSP Editor Features
- **Widget Closing Labels**: Displays `// WidgetName` hints at closing parentheses of multi-line widget constructors.
- **Flutter Color Previews**: Supports `Color(0xAARRGGBB)`, `Color.fromARGB`, `Color.fromRGBO`, `Color.from(...)`, and `Colors.*` swatches with inline color editing.
- **`main()` & Test CodeLens**: Device switcher, Hot Reload, Hot Restart, Stop, and Web/Native DevTools directly above `main()`; single test, nested `group()`, and file-level run/debug actions in `*_test.dart` and `integration_test/`.
- **Debounced Hot Reload on Save**: Automatically reloads non-test `.dart` saves across both DAP and `ctl run` sessions, reporting elapsed milliseconds in Zed progress.

### DAP Debugging & Debug Console
- **Monorepo, FVM & Test Support**: Resolves nested package roots lazily on `launch`/`attach`, honors `.fvm` and `FLUTTER_SDK_PATH`, and automatically passes `--test` to `flutter debug_adapter` for test targets.
- **Smart Device Selection**: Persists your chosen device and tracks hotplug events via `flutter daemon`; if the target device is missing or offline, automatically falls back to the best platform-compatible device with a console notice.
- **Exception Breakpoints & Just-My-Code**: Exception breakpoints default to off (`None`) and can be switched live (`None` / `Unhandled` / `All`) alongside library stepping scope (`My Code Only` / `+ Packages` / `+ SDK`).
- **Clickable Source Links (`Cmd+Hover` & `Cmd+Click`)**: Rewrites `file://`, `package:` (via `mtime`-cached `.dart_tool/package_config.json`), and existing bare `.dart:line:col` paths in DAP output to percent-encoded `zed://file/...:line:col` URLs while keeping `body.source.path` as a local path. Clicking opens the exact file, line, and column inside your existing Zed window.
- **Multi-State ANSI Console Colors**: Translates DAP output categories and Flutter/Dart log levels into theme-adaptive standard ANSI 16 colors (`stderr` / errors → red, `console` build & warnings → yellow, reload/build success → green, service links → cyan, `stdout` info → blue, debug/verbose/framework frames & `\x1b[2m` dim → dark gray) while preserving user-provided 16-color, 24-bit TrueColor, and multi-line ANSI blocks.

### Native GPUI DevTools (`flutter-devtools-gpui`)
Launch via `main()` CodeLens, `flutter-debugger-server ctl devtools-ui`, or directly via `flutter-devtools-gpui [--project <dir>] [--session <id>] [--vm-uri <ws://...>] [--page <inspector|network|logging|performance|memory>]`:

- **Widget Inspector**: Searchable widget tree (toggle implementation widgets), **Layout Explorer** (interactive Flex property editing), **Properties**, **Details Tree**, and **Select Widget Mode** (only jumps to source in Zed when Select Widget Mode is explicitly enabled).
- **Network**: Captures `dart:io` HTTP requests and Dart 3.14+ WebSocket connections/frames; displays decoded Request/Response headers & bodies (gzip decompression + JSON syntax highlighting), supports selectable text, one-click full request copy, status/type filter chips (`All`, `2xx`, `4xx/5xx`, `Pending`, `JSON`, `WS`), and tri-state column sorting (asc → desc → default).
- **Logging**: Subscribes to VM Service logs (`Stdout`, `Stderr`, `FlutterError`, `FlutterInfo`, `Log`, `GC`), pins the selected entry so incoming logs never scroll it away (even across ring-buffer eviction), and jumps to Dart source locations in the active Zed project window.
- **Performance & Memory**: Real-time UI/Raster frame timings with jank detection, plus Isolate heap/RSS metrics and manual GC trigger.

---

## Configuration Reference

### LSP `initialization_options` (Top-Level)

| Key | Type | Default | Description |
|---|---|---|---|
| `autoHotReloadOnSave` | `bool` | `true` | Trigger debounced Hot Reload when saving non-test `.dart` files |
| `hotReloadDebounceMs` | `number` | `250` | Debounce window (ms) for save-triggered Hot Reload |
| `closingLabels` | `bool` | `true` | Show Widget Closing Labels via `inlayHint` |
| `closingLabelsMinLines` | `number` | `2` | Minimum line span required before showing a closing label |
| `colorPreviews` | `bool` | `true` | Enable Flutter color previews and inline color picker |
| `exposeGlobalCodeActions` | `bool` | `false` | Expose project-wide Flutter commands in `Cmd+.` Code Actions on `.dart` files |
| `flutterSdkPath` | `string?` | `null` | Custom Flutter SDK root path (overrides `PATH` / FVM auto-detection) |
| `deviceCacheTtlSecs` | `number` | `60` | Disk cache TTL for device listing |
| `pubspec` | `object` | `{ ... }` | Nested `pubspec.yaml` dependency checker configuration (see below) |

### LSP `initialization_options.pubspec`

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | `bool` | `true` | Enable `pubspec.yaml` / `pubspec_overrides.yaml` dependency checks, hover, completions, hints, CodeLens & actions |
| `pubHostedUrl` | `string?` | `null` | Custom pub registry URL (falls back to `PUB_HOSTED_URL` env var, then `https://pub.dev`) |
| `maxConcurrency` | `number` | `10` | Max concurrent pub registry requests (`1..=32`) |
| `requestTimeoutSecs` | `number` | `15` | HTTP request timeout in seconds (minimum `3`) |
| `cacheTtlSecs` | `number` | `86400` | Package metadata disk/memory cache TTL (24h) |
| `changelogCacheTtlSecs` | `number` | `86400` | Changelog HTML cache TTL (24h) |
| `includePrerelease` | `bool` | `false` | Include prerelease versions even when current constraint is stable |
| `inlayHints` | `bool` | `true` | Show inline version hints at end of dependency lines |
| `showUpToDateHints` | `bool` | `true` | Show `✓ latest` hints on up-to-date dependencies |
| `codeLens` | `bool` | `true` | Show CodeLens bar at top of `pubspec.yaml` |
| `codeLensRefresh` | `bool` | `true` | Show `Refresh (N updates)` CodeLens |
| `codeLensPubGet` | `bool` | `true` | Show `<flutter\|dart> pub get` CodeLens |
| `codeLensPubUpgrade` | `bool` | `true` | Show `<flutter\|dart> pub upgrade` CodeLens |
| `codeLensPubOutdated` | `bool` | `true` | Show `<flutter\|dart> pub outdated` CodeLens |
| `checkDevDependencies` | `bool` | `true` | Check `dev_dependencies` |
| `checkDependencyOverrides` | `bool` | `true` | Check `dependency_overrides` |
| `autoPubGetOnSave` | `bool` | `true` | Automatically run `<flutter\|dart> pub get` on save when dependencies or SDK constraints changed |
| `pubUpgradeTighten` | `bool` | `false` | Pass `--tighten` when running `<flutter\|dart> pub upgrade` from CodeLens |
| `maxChangelogVersions` | `number` | `3` | Maximum changelog release entries rendered in Hover |
| `majorSeverity` | `"error" \| "warning" \| "info" \| "hint" \| "off"` | `"warning"` | Diagnostic severity for major version updates |
| `minorPatchSeverity` | `"error" \| "warning" \| "info" \| "hint" \| "off"` | `"info"` | Diagnostic severity for minor/patch/prerelease updates |
| `lockDriftSeverity` | `"error" \| "warning" \| "info" \| "hint" \| "off"` | `"info"` | Diagnostic severity when `pubspec.lock` is ahead of `pubspec.yaml` lower bound |
| `ignorePackages` | `string[]` | `[]` | Package names excluded from registry checks & diagnostics |

### DAP `.zed/debug.json` Options (`adapter: "Flutter"`)

| Key | Type | Default | Description |
|---|---|---|---|
| `request` | `"launch" \| "attach"` | `"launch"` | Launch a new app/test or attach to a running VM Service |
| `program` | `string` | `"lib/main.dart"` | Entry point or test file (supports `$ZED_RELATIVE_FILE`) |
| `isTest` | `bool` | `false` | Force `--test` mode (auto-detected for `*_test.dart`, `test/`, `integration_test/`) |
| `cwd` | `string?` | auto | Project directory containing `pubspec.yaml` (Monorepo subpackages auto-resolved) |
| `deviceId` | `string?` | selected | Explicit target device ID (falls back to `ctl select-device` / auto-selection) |
| `flutterMode` | `"debug" \| "profile" \| "release"` | `"debug"` | Flutter build mode |
| `flavor` | `string?` | `null` | Passed via `--flavor` |
| `dartDefineFromFile` | `string?` | `null` | Passed via `--dart-define-from-file` |
| `args` / `toolArgs` | `string[]` | `[]` | App runtime arguments / extra Flutter or Dart CLI flags |
| `vmServiceUri` | `string?` | `null` | VM Service URI when `request` is `"attach"` |
| `debugExternalPackageLibraries` | `bool` | `false` | Step into external pub packages during debugging |
| `debugSdkLibraries` | `bool` | `false` | Step into Dart/Flutter SDK libraries during debugging |
| `noDebug` | `bool` | `false` | Launch without attaching breakpoints |

---

## `ctl` CLI Reference

```bash
# Session control
flutter-debugger-server ctl hot-reload [--project <dir>]
flutter-debugger-server ctl hot-restart [--project <dir>]
flutter-debugger-server ctl stop [--project <dir>]
flutter-debugger-server ctl status [--project <dir>]
flutter-debugger-server ctl screenshot [--out <path>]

# DevTools & Service Extensions
flutter-debugger-server ctl devtools [--page <inspector|performance|memory|network|logging|cpu-profiler>]
flutter-debugger-server ctl devtools-ui [--project <dir>] [--page <inspector|network|logging|performance|memory>]
flutter-debugger-server ctl overlay <inspector|performance|debug-paint|baselines|repaint|slow-anim|banner|oversized|platform|brightness>

# Devices & Emulators
flutter-debugger-server ctl devices
flutter-debugger-server ctl select-device [device_id]
flutter-debugger-server ctl emulators
flutter-debugger-server ctl launch-emulator [emulator_id]

# Run & Project Workflow
flutter-debugger-server ctl run [--mode debug|profile|release] [--target lib/main.dart] [--device <id>] [--flavor <f>] [--dart-define-from-file <file>]
flutter-debugger-server ctl build-runner <build|watch>
flutter-debugger-server ctl pub-get | pub-upgrade [--tighten] | pub-outdated | clean | doctor | create
flutter-debugger-server init-zed
flutter-debugger-server ctl print-zed-config
```

---

## Development & Verification

```bash
# Run all tests & Clippy across every crate
cargo clippy --target wasm32-wasip2 --all-targets -- -D warnings
cargo test --manifest-path crates/protocol/Cargo.toml && cargo clippy --manifest-path crates/protocol/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path server/Cargo.toml && cargo clippy --manifest-path server/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path devtools-gpui/Cargo.toml && cargo clippy --manifest-path devtools-gpui/Cargo.toml --all-targets -- -D warnings

# Build & install release binaries locally
./scripts/install-local.sh
```

## License

Licensed under the [Apache License 2.0](./LICENSE).
