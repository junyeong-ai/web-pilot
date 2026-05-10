## Summary

<!-- What does this PR change? Be specific. One paragraph max. -->

## Motivation

<!-- Why is this change needed? Link the related issue / discussion. -->

## Mode coverage

- [ ] Verified in **headless** mode (`webpilot ...`)
- [ ] Verified in **browser** mode (`webpilot --browser ...`)
- [ ] N/A — change does not affect a transport

## Wire-protocol impact

<!-- Any change touching crates/webpilot/{action,capture,protocol,error,wait}.rs
     or extension/content/bridge.js MUST be ticked here. -->

- [ ] No protocol change
- [ ] Additive change (new variant / field with default — both transports updated)
- [ ] Breaking change (`workspace.package.version` bumped)

## Local checks

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked --no-fail-fast`
- [ ] `RUSTDOCFLAGS='-D warnings -D rustdoc::broken-intra-doc-links' cargo doc --workspace --no-deps --locked` (if rustdoc touched)

## Security

- [ ] No new dependencies, **or** the additions pass `cargo deny check`
- [ ] No expansion of the `WEBPILOT_HOME` mode-0700 invariant
- [ ] No new IPC / NM surface that bypasses policy enforcement
