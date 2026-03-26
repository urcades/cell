# Plugin Authoring

This guide shows the minimum shape of a Rust-native plugin that works with the current host.

## 1. Create a descriptor

The host looks for `pi-plugin-host.json` or `plugin-host.json`.

Each descriptor points to an executable and optional arguments:

```json
{
  "id": "hello-plugin",
  "name": "Hello Plugin",
  "executable": "cargo",
  "args": ["run", "--quiet", "--manifest-path", "./Cargo.toml"],
  "description": "Minimal standalone Rust plugin example"
}
```

The descriptor directory becomes the default working directory unless `workingDirectory` is set.

## 2. Write a real executable

The plugin entrypoint can be a compiled Rust binary, or a wrapper that launches one.
For the example in this repo, the descriptor runs `cargo run` inside the example directory so the plugin stays outside the main workspace build.

## 3. Complete the handshake

The host sends one `HandshakeRequest` line first.
The plugin must answer with a `Registration` message that includes:

- `protocolVersion: 1`
- a manifest whose `manifestVersion` is also `1`
- a plugin identity whose `id` matches the descriptor id

If the plugin sends anything else, the host treats startup as a failure.

## 4. Register capabilities

The manifest can register commands, tools, flags, hooks, providers, and models.

The current runtime classes are split like this:

- Commands, tools, and lifecycle hooks are live and dispatchable.
- Flags are carried in the manifest and startup summary, but they are not yet bound into a live plugin flag surface.
- Providers and models are accepted and merged, but execution is still deferred.

Duplicate names are rejected during registration or runtime merge.

## 5. Keep the message loop simple

After registration, the host sends request messages for commands, tools, hooks, and shutdown.
A plugin should:

- read one line at a time from stdin
- parse the JSON payload
- respond on stdout with the matching request id
- ignore or log unexpected messages instead of crashing when possible

## 6. Test the plugin

Run the example host launch path from the repo root:

```bash
cargo run --manifest-path crates/pi-rust-plugin-host/Cargo.toml -- launch examples/plugin-hello/pi-plugin-host.json
```

Or run the example crate directly:

```bash
cargo test --manifest-path examples/plugin-hello/Cargo.toml
```

The example crate includes an integration test that exercises the live command, tool, and hook paths.
