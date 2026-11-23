#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
INSTALLED_SERVER_BIN="$INSTALL_DIR/flutter-debugger-server"
INSTALLED_GUI_BIN="$INSTALL_DIR/flutter-devtools-gpui"

SERVER_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --server-only)
      SERVER_ONLY=1
      ;;
  esac
done

echo "==> [1/6] Checking & building WASM extension (wasm32-wasip2)..."
cd "$REPO_ROOT"
cargo clippy --target wasm32-wasip2 --all-targets -- -D warnings
cargo build --release --target wasm32-wasip2

echo "==> [2/6] Running shared protocol crate clippy & unit tests..."
cd "$REPO_ROOT/crates/protocol"
cargo clippy --all-targets -- -D warnings
cargo test --all-targets

echo "==> [3/6] Running server clippy & unit tests..."
cd "$REPO_ROOT/server"
cargo clippy --all-targets -- -D warnings
cargo test --all-targets

echo "==> [4/6] Building release flutter-debugger-server..."
cargo build --release

mkdir -p "$INSTALL_DIR"
cp "$REPO_ROOT/server/target/release/flutter-debugger-server" "$INSTALLED_SERVER_BIN"
chmod +x "$INSTALLED_SERVER_BIN"

if [[ "$SERVER_ONLY" -eq 0 ]]; then
  echo "==> [5/6] Running devtools-gpui clippy, unit tests & release build..."
  cd "$REPO_ROOT/devtools-gpui"
  cargo clippy --all-targets -- -D warnings
  cargo test --all-targets
  cargo build --release
  cp "$REPO_ROOT/devtools-gpui/target/release/flutter-devtools-gpui" "$INSTALLED_GUI_BIN"
  chmod +x "$INSTALLED_GUI_BIN"
else
  echo "==> [5/6] Skipping devtools-gpui build (--server-only)"
fi

echo "==> [6/6] Installation Summary:"
echo "  Server: $INSTALLED_SERVER_BIN ($("$INSTALLED_SERVER_BIN" --version))"
if [[ -x "$INSTALLED_GUI_BIN" ]]; then
  echo "  GPUI DevTools: $INSTALLED_GUI_BIN ($("$INSTALLED_GUI_BIN" --version))"
fi
echo ""

if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  echo "WARNING: $INSTALL_DIR is NOT in your current shell PATH."
  echo "  Add to your ~/.zshrc:  export PATH=\"$INSTALL_DIR:\$PATH\""
  echo ""
fi

echo "NOTE for macOS Zed GUI (if GUI app does not inherit ~/.local/bin in PATH):"
echo "  You can point Zed directly to the installed binary in ~/.config/zed/settings.json:"
echo "    \"lsp\": { \"flutter-debugger\": { \"binary\": { \"path\": \"$INSTALLED_SERVER_BIN\" } } }"
echo ""
echo "Next steps in Zed:"
echo "  1. Command Palette -> 'zed: install dev extension' -> select: $REPO_ROOT"
echo "  2. In any Flutter project root, run: flutter-debugger-server init-zed"
echo "  3. Launch Native GPUI DevTools anytime: flutter-debugger-server ctl devtools-ui"
