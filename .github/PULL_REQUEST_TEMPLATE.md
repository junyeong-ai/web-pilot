## Summary

<!-- What does this PR change? Be specific. One paragraph max. -->

## Motivation

<!-- Why is this change needed? Link the related issue / discussion. -->

## Mode coverage

- [ ] Verified in **headless** mode (`webpilot ...`)
- [ ] Verified in **browser** mode (`webpilot --browser ...`)
- [ ] N/A — change does not affect a transport

## Wire-protocol impact

<!-- Any change touching crates/webpilot/{action,capture,protocol,error,types,wait}.rs,
     extension/content/bridge.js, or extension/background/*.js MUST be
     ticked here. -->

- [ ] No protocol change
- [ ] Additive change (new variant / field with default — both transports updated)
- [ ] Breaking change (`workspace.package.version` bumped)

## Local checks

The list CI gates on is "Build & Run" in `CLAUDE.md`.

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked --no-fail-fast`
- [ ] `RUSTDOCFLAGS='-D warnings -D rustdoc::broken-intra-doc-links' cargo doc --workspace --no-deps --locked`
- [ ] `npm ci && npm run lint`
- [ ] `shellcheck scripts/*.sh`

## Security

- [ ] No new dependencies, **or** the additions pass `cargo deny check`
- [ ] No weakening of the mode-0700 cache and data roots
- [ ] No new IPC / NM surface that bypasses policy enforcement
