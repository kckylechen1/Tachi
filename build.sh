#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Build the release binary
cargo build --release -p memory-server

# Check if build succeeded
if [ $? -eq 0 ]; then
    echo "Build successful!"
    echo "Binary location: ./target/release/memory-server"
    ls -lh ./target/release/memory-server
else
    echo "Build failed!"
    exit 1
fi