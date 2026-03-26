# Verification

Run verification from the Rust repo root.

## Fast local checks

Build and test the full workspace:

```bash
cargo test --workspace
```

Run the Rust-only terminal regression suite:

```bash
env -u PI_TS_REPO cargo test -p cell-cli --test tui_parity
```

Check production targets without test-only noise:

```bash
cargo check --workspace --lib --bins
```

## Rust-native verification entrypoint

Use this when you want the repo's standard verification flow:

```bash
./scripts/rust_only_ci.sh
```

This flow is expected to work even when `PI_TS_REPO` is unset.

## Optional parity tooling

TypeScript parity capture is opt-in.

- Set `PI_TS_REPO` only when you intentionally want side-by-side comparisons.
- If `PI_TS_REPO` is unset, Rust-native development should still work cleanly.
- The Rust-only CI and packaging scripts do not depend on `PI_TS_REPO`.
