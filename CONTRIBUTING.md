# Contributing

Pairagen accepts focused bug fixes, tests, documentation improvements, and
backend integrations.

## Development setup

Requirements:

- Rust 1.85 or newer
- Neovim 0.10 or newer
- `cargo`, `curl`, `tar`, and a SHA-256 utility

Use the development backend configuration from the README, then run:

```sh
scripts/check-versions.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
XDG_STATE_HOME=/tmp/pairagen-state \
XDG_DATA_HOME=/tmp/pairagen-data \
XDG_CACHE_HOME=/tmp/pairagen-cache \
nvim --headless -u NONE -i NONE -l scripts/headless-smoke.lua
```

## Pull requests

- Keep commits focused and use concise imperative commit messages.
- Add a regression test for behavior changes.
- Keep source code, UI text, comments, and documentation in English.
- Do not include session traces, private source code, credentials, or generated
  `paird` binaries in commits.
- Update `CHANGELOG.md` for user-visible changes.

## Releases

1. Set the same version in `Cargo.toml`, `lua/pair/version.lua`, and the README.
2. Move changelog entries from `Unreleased` into the versioned section.
3. Run the complete validation suite.
4. Create and push an annotated `vMAJOR.MINOR.PATCH` tag.

The release workflow builds four target archives, generates SHA-256 checksums,
and creates the GitHub release.
