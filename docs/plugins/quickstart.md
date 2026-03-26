# Plugin Quickstart

This is the fastest working path from zero to a live plugin.

## 1. Start from the example plugin

Use `examples/plugin-hello`.

It already contains:

- a descriptor file
- a Rust executable
- handshake handling
- one command
- one tool
- one hook

## 2. Run the example test

From the repo root:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml --test runtime
```

That proves the example still supports live command, tool, and hook dispatch.

## 3. Inspect discovery

```bash
cargo run -p cell-plugin-host -- discover examples/plugin-hello
```

You should see the descriptor path and the launch settings for `Hello Plugin`.

## 4. Launch the plugin directly

```bash
cargo run -p cell-plugin-host -- launch examples/plugin-hello/cell-plugin-host.json
```

The summary should show:

- one command
- one tool
- one hook

The example behavior is intentionally simple:

- command `hello` rewrites its arguments into `hello:<arg>|<arg>`
- tool `echo` returns `tool:<text>`
- hook `session-started` listens to the `sessionStarted` lifecycle event

## 5. Add the plugin root to `cell`

User scope:

```bash
cargo run -p cell-cli -- plugins add-root /absolute/path/to/examples/plugin-hello
```

Project scope:

```bash
cargo run -p cell-cli -- plugins add-root /absolute/path/to/examples/plugin-hello --project
```

## 6. Inspect what the app sees

Human-readable diagnostics:

```bash
cargo run -p cell-cli -- plugins list
```

Machine-readable diagnostics:

```bash
cargo run -p cell-cli -- plugins list --mode json
```

If you want to see the example outside the app UI, the example runtime test is the clearest demonstration of the live behavior. It asserts that:

- `hello Ada Lovelace` becomes `hello:Ada|Lovelace`
- tool input `{ "text": "Ada" }` becomes `tool:Ada`
- the `sessionStarted` hook dispatch completes without warnings

## 7. Remove the root when you are done

```bash
cargo run -p cell-cli -- plugins remove-root /absolute/path/to/examples/plugin-hello
```

## Example layout

```text
examples/plugin-hello/
├── Cargo.toml
├── README.md
├── cell-plugin-host.json
├── src/
│   └── main.rs
└── tests/
    └── runtime.rs
```

## Next reading

- [Authoring](./authoring.md)
- [Discovery](./discovery.md)
- [Protocol](./protocol.md)
- [Capabilities](./capabilities.md)
- [Events](./events.md)
- [Troubleshooting](./troubleshooting.md)
