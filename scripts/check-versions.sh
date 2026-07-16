#!/usr/bin/env bash
set -euo pipefail

cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
lua_version="$(sed -n 's/^  plugin = "\([^"]*\)",/\1/p' lua/loopbiotic/version.lua)"
rust_protocol="$(sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p' rust/crates/loopbiotic_protocol/src/lib.rs)"
lua_protocol="$(sed -n 's/^  protocol = \([0-9]*\),/\1/p' lua/loopbiotic/version.lua)"

test -n "$cargo_version"
test "$cargo_version" = "$lua_version"
test -n "$rust_protocol"
test "$rust_protocol" = "$lua_protocol"
grep -q "^## \[$cargo_version\]" CHANGELOG.md

echo "Versions match: v$cargo_version, protocol $rust_protocol"
