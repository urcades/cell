# Cell Plugin Guide

This section documents the Rust-native plugin system in `cell`.

## Who this is for

Use these docs if you want to:

- understand what the plugin system supports today
- build a new Rust-native plugin
- debug plugin discovery, launch, or runtime behavior
- inspect the example plugin and adapt it into your own project

## Current support boundary

Live today:

- commands
- tools
- hooks

Accepted in manifests but not yet live:

- flags as a runtime surface
- provider execution
- model execution

Out of scope:

- JavaScript and TypeScript extension execution
- embedding Node or Bun into the Rust runtime
- custom injected plugin UI

## Reading paths

If you are new here:

1. [Overview](./overview.md)
2. [Quickstart](./quickstart.md)
3. [Authoring](./authoring.md)

If you are debugging:

1. [Discovery](./discovery.md)
2. [Protocol](./protocol.md)
3. [Troubleshooting](./troubleshooting.md)

If you need the exact runtime boundary:

- [Capabilities](./capabilities.md)
- [Events](./events.md)
- [Capability Classes](./capability-classes.md)

If you want a runnable reference:

- [Example Plugin](./example-plugin.md)
- [`examples/plugin-hello/README.md`](../../examples/plugin-hello/README.md)

## Command convention used in these docs

Most commands are shown in source-tree form so they work from the repo checkout:

- `cargo run -p cell-cli -- ...`
- `cargo run -p cell-plugin-host -- ...`

If you already have the release binaries on your `PATH`, you can replace those with:

- `cell ...`
- `cell-plugin-host ...`
