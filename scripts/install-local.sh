#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

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

echo "==> [4/4] Installing binary to $INSTALL_DIR/flutter-debugger-server..."
mkdir -p "$INSTALL_DIR"
cp "$REPO_ROOT/server/target/release/flutter-debugger-server" "$INSTALL_DIR/flutter-debugger-server"
chmod +x "$INSTALL_DIR/flutter-debugger-server"

echo ""
echo "Installed: $INSTALL_DIR/flutter-debugger-server ($("$INSTALL_DIR/flutter-debugger-server" --version))"
echo ""
echo "Next steps in Zed:"
echo "  1. Ensure $INSTALL_DIR is in your shell PATH (e.g. export PATH=\"\$HOME/.local/bin:\$PATH\")."
echo "  2. In Zed: Command Palette -> 'zed: install dev extension' -> select:"
echo "       $REPO_ROOT"
echo "  3. In any Flutter project root, generate .zed/debug.json & .zed/tasks.json:"
echo "       flutter-debugger-server init-zed"
echo "  4. Print recommended ~/.config/zed/settings.json & keymap.json snippets:"
echo "       flutter-debugger-server ctl print-zed-config"
