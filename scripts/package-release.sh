#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <target> <paird-binary> [output-directory]" >&2
  exit 2
fi

target="$1"
binary="$2"
output="${3:-.}"
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
artifact="paird-v${version}-${target}.tar.gz"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

test -x "$binary"
mkdir -p "$output"
cp "$binary" "$staging/paird"
cp LICENSE README.md "$staging/"
tar -C "$staging" -czf "$output/$artifact" paird LICENSE README.md

if command -v sha256sum >/dev/null; then
  (cd "$output" && sha256sum "$artifact" > "$artifact.sha256")
else
  (cd "$output" && shasum -a 256 "$artifact" > "$artifact.sha256")
fi

echo "$output/$artifact"
