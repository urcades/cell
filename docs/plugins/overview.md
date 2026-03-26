# Plugin Overview

`pi-rust` supports Rust-native executable plugins.

A plugin is a separate process that:

- is discovered from a package root or plugin root
- starts over stdio
- answers a versioned JSON-line handshake
- registers capabilities with the host

## What plugins are

- Native executables, usually written in Rust
- A way to add commands, tools, and lifecycle hooks
- Isolated from the main app process
- Best-effort at runtime: plugin failures become warnings instead of crashing the app

## What plugins are not

- They are not JavaScript or TypeScript extensions
- They are not embedded Node or Bun runtimes
- They are not a custom UI injection system
- They are not yet a supported way to provide live model or provider execution

## What works today

- Commands: live
- Tools: live
- Hooks: live

## What is declared but not yet part of the supported author story

- Flags: manifest metadata only
- Providers: accepted in registration, not executed
- Models: accepted in registration, not executed

## What stays deferred

- Provider execution
- Model execution
- JS/TS extension compatibility
- Rich injected plugin UI

## Main entry points

- `pi-rust plugins list`
- `pi-rust plugins list --mode json`
- `pi-rust plugins add-root <path>`
- `pi-rust plugins remove-root <path>`
- `pi-rust-plugin-host discover <root>`
- `pi-rust-plugin-host launch <descriptor>`
