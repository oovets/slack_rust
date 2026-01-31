#!/bin/bash
set -e

echo "Building Slack Client (Rust TUI)..."
echo "===================================="

# Build in release mode for maximum performance
cargo build --release

echo ""
echo "Build complete!"
echo "Run with: ./target/release/slack_client_rs"
