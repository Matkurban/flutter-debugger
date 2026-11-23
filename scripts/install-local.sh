#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
INSTALLED_BIN="$INSTALL_DIR/flutter-debugger-server"

echo "==> [1/4] Checking & building WASM extension (wasm32-wasip2)..."
cd "$REPO_ROOT"
cargo clippy --target wasm32-wasip2 --all-targets -- -D warnings
cargo build --release --target wasm32-wasip2

echo "==> [2/4] Running server clippy & unit tests..."
cd "$REPO_ROOT/server"
cargo clippy --all-targets -- -D warnings
cargo test --all-targets

echo "==> [3/4] Building release flutter-debugger-server..."
cargo build --release

echo "==> [4/4] Installing binary to $INSTALLED_BIN..."
mkdir -p "$INSTALL_DIR"
cp "$REPO_ROOT/server/target/release/flutter-debugger-server" "$INSTALLED_BIN"
chmod +x "$INSTALLED_BIN"

echo ""
echo "Installed: $INSTALLED_BIN ($("$INSTALLED_BIN" --version))"
echo ""

if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  echo "WARNING: $INSTALL_DIR is NOT in your current shell PATH."
  echo "  Add to your ~/.zshrc:  export PATH=\"$INSTALL_DIR:\$PATH\""
  echo ""
fi

echo "NOTE for macOS Zed GUI (if GUI app does not inherit ~/.local/bin in PATH):"
echo "  You can point Zed directly to the installed binary in ~/.config/zed/settings.json:"
echo "    \"lsp\": { \"flutter-debugger\": { \"binary\": { \"path\": \"$INSTALLED_BIN\" } } }"
echo ""
echo "Next steps in Zed:"
echo "  1. Command Palette -> 'zed: install dev extension' -> select: $REPO_ROOT"
echo "  2. In any Flutter project root, run: flutter-debugger-server init-zed"
echo "  3. Print recommended settings.json & keymap.json: flutter-debugger-server ctl print-zed-config"
