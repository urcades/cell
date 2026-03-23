## Repo workflow

`/Users/edouard/Developer/pi/rust` is the source of truth for the Rust port.

- The outer TypeScript repo ignores `rust/`.
- Normal Rust work happens entirely inside the nested Rust repo.
- Rust development and CI must work without the TypeScript checkout.
- TypeScript parity capture is optional bridge tooling only.

## Rust-native workflow

Run commands from `/Users/edouard/Developer/pi/rust`.

- `cargo test --workspace`
- `cargo test -p pi-rust-cli --test tui_parity`
- `cargo test -p pi-rust-plugins`

These commands must pass even when `PI_TS_REPO` is unset.

## Parity bridge workflow

TypeScript side-by-side capture is opt-in.

- Set `PI_TS_REPO` to the TypeScript checkout root when you want parity comparisons.
- If `PI_TS_REPO` is unset, Rust-native development still works and parity bridge commands should fail clearly rather than guessing parent-relative paths.

## Git workflow

- Use the outer repo only for TypeScript changes and for the single ignore rule that excludes `rust/`.
- Use the nested Rust repo for Rust changes, history, branches, and releases.
- Do not rely on the outer repo to track Rust work.
