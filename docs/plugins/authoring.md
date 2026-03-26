# Plugin Authoring

This guide shows the minimum shape of a Rust-native plugin that works with the current host.

## Authoring model

A plugin has three moving parts:

1. a descriptor file that tells the host how to launch it
2. an executable that speaks the plugin protocol over stdio
3. a registration manifest that describes its capabilities

The best starting point is still the runnable example under `examples/plugin-hello`.

## 1. Create a descriptor

The preferred descriptor name is `cell-plugin-host.json`.

The host also accepts `plugin-host.json` for compatibility, but new plugins should use the `cell-plugin-host.json` name.

A minimal descriptor looks like this:

```json
{
  "id": "hello-plugin",
  "name": "Hello Plugin",
  "executable": "cargo",
  "args": ["run", "--quiet", "--manifest-path", "./Cargo.toml"],
  "description": "Minimal standalone Rust plugin example"
}
```

Important rules:

- the descriptor `id` must match the manifest plugin id
- the descriptor directory becomes the default working directory unless `workingDirectory` is set
- the executable can be a compiled binary or a wrapper command like `cargo run`
- relative paths in `args` and environment values are evaluated by the launched process from that working directory

## 2. Build a real executable

The plugin process must:

- read one JSON message per line from stdin
- write one JSON message per line to stdout
- keep stderr for logs only
- stay alive long enough to answer startup and runtime requests

For the example in this repo, the descriptor runs `cargo run` so the plugin stays outside the main workspace build.

## 3. Answer the handshake

The host sends `handshake_request` first.

The plugin must answer with `registration`, including:

- `protocolVersion: 1`
- `manifestVersion: 1`
- a plugin identity whose `id` matches the descriptor id

If startup sends anything else, the host rejects the plugin.

## 4. Register capabilities

The manifest can declare commands, tools, flags, hooks, providers, and models.

Supported live author target today:

- commands
- tools
- hooks

Accepted but not yet live:

- flags as a runtime surface
- providers
- models

Duplicate capability names are rejected during validation or merge.

## 5. Keep the message loop small and predictable

A good first plugin should:

- parse one message at a time
- match on message type
- answer each request with the same `requestId`
- return structured errors instead of panicking when possible
- exit cleanly after `shutdown_request`

Do not start with a complex background runtime. Start with a tight request-response loop and add behavior only after the basic host path is reliable.

## 6. Test it early

Useful checks from the repo root:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml --test runtime
cargo run -p cell-plugin-host -- discover examples/plugin-hello
cargo run -p cell-plugin-host -- launch examples/plugin-hello/cell-plugin-host.json
```

Then add the plugin root to the main app and inspect diagnostics:

```bash
cargo run -p cell-cli -- plugins add-root /absolute/path/to/plugin-root
cargo run -p cell-cli -- plugins list --mode json
```

## 7. Know the support boundary

What works today:

- command execution
- tool execution
- hook execution

What is deferred:

- provider execution
- model execution
- plugin-defined runtime flags as a live surface

What is out of scope:

- JavaScript and TypeScript extension execution
- Node or Bun embedding
- injected custom UI

## Best next step

If you are starting a real plugin, copy the example plugin structure first and change one thing at a time.
