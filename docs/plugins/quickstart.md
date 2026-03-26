# Plugin Quickstart

This is the fastest working path for a new plugin author.

## 1. Start from the example plugin

Use [examples/plugin-hello](../../examples/plugin-hello).

It already includes:

- a descriptor file
- a Rust executable
- handshake handling
- one command
- one tool
- one hook

## 2. Build and test the example

From the Rust repo root:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml --test runtime
```

## 3. Inspect it with the host

Discover the descriptor:

```bash
cargo run --manifest-path crates/pi-rust-plugin-host/Cargo.toml -- discover examples/plugin-hello
```

Launch it directly:

```bash
cargo run --manifest-path crates/pi-rust-plugin-host/Cargo.toml -- launch examples/plugin-hello/pi-plugin-host.json
```

The launch summary should show one command, one tool, and one hook.

## 4. Add the plugin root to `pi-rust`

User scope:

```bash
pi-rust plugins add-root /absolute/path/to/examples/plugin-hello
```

Project scope:

```bash
pi-rust plugins add-root ./relative/path/to/plugin-root --project
```

## 5. Inspect what the app sees

Human-readable summary:

```bash
pi-rust plugins list
```

Machine-readable summary:

```bash
pi-rust plugins list --mode json
```

## 6. Remove the root when you are done

```bash
pi-rust plugins remove-root /absolute/path/to/examples/plugin-hello
```

## Folder layout

The example plugin uses this layout:

```text
examples/plugin-hello/
├── Cargo.toml
├── README.md
├── pi-plugin-host.json
├── src/
│   └── main.rs
└── tests/
    └── runtime.rs
```

## Next reading

- [Discovery](./discovery.md)
- [Protocol](./protocol.md)
- [Capabilities](./capabilities.md)
- [Events](./events.md)
- [Troubleshooting](./troubleshooting.md)
