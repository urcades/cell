# Hello Plugin Example

This is the smallest complete Rust-native plugin in the repo.

Use it when you want a concrete reference for:

- descriptor layout
- handshake and registration
- one live command
- one live tool
- one live hook

## Files

- `Cargo.toml`: isolated example workspace
- `cell-plugin-host.json`: host launch descriptor
- `src/main.rs`: plugin executable
- `tests/runtime.rs`: end-to-end runtime test

## Run it from the repo root

Run the example test:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml --test runtime
```

Inspect discovery:

```bash
cargo run -p cell-plugin-host -- discover examples/plugin-hello
```

Launch it directly:

```bash
cargo run -p cell-plugin-host -- launch examples/plugin-hello/cell-plugin-host.json
```

## Load it into `cell`

Add the example directory as a plugin root:

```bash
cargo run -p cell-cli -- plugins add-root /absolute/path/to/examples/plugin-hello
```

Inspect the plugin runtime summary:

```bash
cargo run -p cell-cli -- plugins list
cargo run -p cell-cli -- plugins list --mode json
```

Remove the root when you are done:

```bash
cargo run -p cell-cli -- plugins remove-root /absolute/path/to/examples/plugin-hello
```

## What the example registers

- Command: `hello`
- Tool: `echo`
- Hook: `session-started`
