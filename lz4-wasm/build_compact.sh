#!/bin/bash
set -euo pipefail

BINARY=pkg/lz4_wasm_nodejs_bg.wasm

wasm-pack build

# remove unused parts of code
wasm-snip --snip-rust-fmt-code \
          --snip-rust-panicking-code \
          -o $BINARY \
          $BINARY

wasm-strip $BINARY
wasm-opt -Oz -o $BINARY $BINARY

