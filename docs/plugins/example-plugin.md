# Example Plugin

The example plugin lives at [`examples/plugin-hello`](../../examples/plugin-hello).

It is intentionally small and stable. Treat it as the reference implementation for the current plugin authoring story.

## What it demonstrates

- descriptor-based launch through `cell-plugin-host.json`
- handshake and registration over stdio
- one live command
- one live tool
- one live hook
- running outside the main workspace build

## File-by-file map

- `Cargo.toml`: isolated example workspace
- `cell-plugin-host.json`: host launch descriptor
- `src/main.rs`: plugin executable
- `tests/runtime.rs`: end-to-end runtime test
- `README.md`: example-specific quick reference

## How to run it

From the repo root:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml --test runtime
cargo run -p cell-plugin-host -- discover examples/plugin-hello
cargo run -p cell-plugin-host -- launch examples/plugin-hello/cell-plugin-host.json
```

## What to expect

The host summary should report:

- plugin id: `hello-plugin`
- plugin name: `Hello Plugin`
- commands: 1
- tools: 1
- hooks: 1

The example behavior is intentionally simple and easy to inspect:

- command `hello` turns arguments like `Ada Lovelace` into `hello:Ada|Lovelace`
- tool `echo` turns `{ "text": "Ada" }` into `tool:Ada`
- hook name `session-started` listens for the `sessionStarted` event and returns `continue`

## How to use it with `cell`

Add the example as a plugin root:

```bash
cargo run -p cell-cli -- plugins add-root /absolute/path/to/examples/plugin-hello
```

Then inspect runtime diagnostics:

```bash
cargo run -p cell-cli -- plugins list
cargo run -p cell-cli -- plugins list --mode json
```

Remove it when you are done:

```bash
cargo run -p cell-cli -- plugins remove-root /absolute/path/to/examples/plugin-hello
```
