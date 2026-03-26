# Plugin Overview

A `cell` plugin is a separate executable process that the host discovers, launches, handshakes with over stdio, and then calls for supported capability classes.

## Mental model

A plugin is not loaded into the main process. Instead:

1. `cell` or `cell-plugin-host` finds a descriptor file.
2. The host starts the plugin executable.
3. The host sends a JSON-line handshake request.
4. The plugin answers with a versioned registration manifest.
5. The host validates the manifest and merges the capabilities.
6. The host sends command, tool, hook, and shutdown requests as needed.

This gives the plugin system real process isolation. A bad plugin should produce warnings, not take down the main app.

## What plugins are

- Rust-native executable extensions to the product
- A way to add commands, tools, and lifecycle hooks
- Out-of-process integrations with clear protocol boundaries
- Best-effort at runtime: failures are surfaced as warnings

## What plugins are not

- They are not JavaScript or TypeScript extensions
- They are not a Node or Bun embedding layer
- They are not an injected custom UI system
- They are not yet a supported path for live provider or model execution

## Live capability classes

- Commands: live
- Tools: live
- Hooks: live

## Deferred capability classes

- Flags: accepted in registration and diagnostics, not yet executed as a live flag surface
- Providers: accepted in registration and diagnostics, not yet executed
- Models: accepted in registration and diagnostics, not yet executed

## Runtime behavior that matters

- Plugin discovery problems are warnings
- Startup failures are warnings
- Duplicate capability names are rejected
- Hook failures are warnings
- Timeouts are warnings
- Stderr noise does not fail a plugin on its own

## Useful entry points

```bash
cargo run -p cell-cli -- plugins list
cargo run -p cell-cli -- plugins list --mode json
cargo run -p cell-plugin-host -- discover examples/plugin-hello
cargo run -p cell-plugin-host -- launch examples/plugin-hello/cell-plugin-host.json
```
