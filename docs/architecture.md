# Architecture Overview

This page is the current, non-historical view of how the Rust `cell` repo is organized.

## Product shape

At a high level, the repo has four layers:

1. the user-facing CLI and terminal UI
2. the core session and runtime layer
3. supporting product services such as models, config, resources, packages, tools, and OAuth
4. the plugin and transport layer

## Main runtime path

The normal runtime path looks like this:

1. `cell-cli` parses command-line input and starts the requested mode.
2. `cell-core` creates or resumes a session and coordinates prompt execution.
3. `cell-models`, `cell-ai-providers`, and `cell-oauth` resolve provider, model, and auth state.
4. `cell-tools` handles built-in tools.
5. `cell-resources` and `cell-packages` supply package and resource state.
6. `cell-tui` renders the terminal UI when running interactively.
7. `cell-plugin-host` dispatches plugin commands, tools, and hooks when plugins are loaded.

## Important crates

- `crates/cell-cli`: top-level CLI, interactive UI, config UI, and command routing
- `crates/cell-core`: core runtime, session orchestration, export flow, and plugin integration points
- `crates/cell-session`: session persistence and branching
- `crates/cell-models`: model catalog, availability, and selection logic
- `crates/cell-ai-providers`: provider-specific request and streaming behavior
- `crates/cell-oauth`: auth storage, login, and token refresh behavior
- `crates/cell-tools`: built-in tool implementations
- `crates/cell-resources`: resource discovery and cataloging
- `crates/cell-packages`: package installation, update, and listing
- `crates/cell-config`: layered config and settings persistence
- `crates/cell-tui`: terminal rendering primitives and widgets
- `crates/cell-jsonline-transport`: shared JSON-line transport
- `crates/cell-rpc`: reusable RPC layer on top of that transport
- `crates/cell-protocol`: RPC command, event, and diagnostics types
- `crates/cell-plugin-host`: plugin discovery, launch, validation, and runtime dispatch
- `crates/cell-plugin-protocol`: plugin wire protocol types
- `crates/cell-plugins`: plugin manifest and registration types

## Plugin architecture

Plugins are separate executables, not in-process modules.

The host flow is:

1. discover descriptor files
2. launch plugin processes
3. perform stdio handshake and registration
4. merge capabilities into the active registry
5. dispatch commands, tools, and hooks at runtime

The current live plugin classes are:

- commands
- tools
- hooks

Provider and model execution are still deferred.

## Configuration and compatibility

The Rust repo is the product source of truth, but some compatibility surfaces remain:

- project-scoped settings still live under `.pi/settings.json`
- some environment variables still use `PI_*` names
- optional TypeScript parity tooling still uses `PI_TS_REPO`

Those are compatibility holdovers, not active architectural dependencies.

## Where to go next

- For how to work in the repo: [repo-workflow.md](./repo-workflow.md)
- For plugin authoring: [plugins/README.md](./plugins/README.md)
- For the historical analysis: [history/port-postmortem.md](./history/port-postmortem.md)
