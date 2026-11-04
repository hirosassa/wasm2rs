#!/bin/sh
# Predicate for `wasm-tools shrink`: the module (given as $1) still differs.
exec "$(dirname "$0")/target/release/examples/interesting" "$1"
