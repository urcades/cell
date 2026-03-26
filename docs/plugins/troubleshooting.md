# Plugin Troubleshooting

## The host finds nothing

Check these first:

- the root you added actually contains a descriptor
- the descriptor file is named `cell-plugin-host.json` or `plugin-host.json`
- you added the correct root with `plugins add-root`

Useful commands:

```bash
cargo run -p cell-cli -- plugins list --mode json
cargo run -p cell-plugin-host -- discover /path/to/root
```

## The plugin launches but is rejected

Common causes:

- descriptor id does not match manifest plugin id
- unsupported protocol version
- unsupported manifest version
- duplicate capability names
- malformed JSON on stdout

Use:

```bash
cargo run -p cell-plugin-host -- launch /path/to/cell-plugin-host.json
```

That gives the clearest startup summary.

## The plugin prints to stderr

Stderr output is allowed.

It does not fail registration by itself. The host only warns or rejects when there is a real launch, timeout, or protocol problem alongside it.

## The plugin times out

The host uses request timeouts for startup and runtime dispatch.

If a command, tool, or hook takes too long:

- the request is treated as failed
- the app records a warning
- the main app keeps running

## `cargo run` plugins fail when you isolate `HOME`

If you test plugins with a temporary `HOME`, `cargo run` may stop finding the Rust toolchain.

Preserve:

- `CARGO_HOME`
- `RUSTUP_HOME`

and isolate only:

- `HOME`
- `XDG_CONFIG_HOME`

## Commands or tools do not appear where you expect

Check these points:

- hidden commands and hidden tools stay out of normal discovery surfaces
- duplicate names are rejected during merge
- built-in command names still win when a plugin tries to reuse them

Use plugin diagnostics first:

```bash
cargo run -p cell-cli -- plugins list --mode json
```

## JS and TS extensions do not run

That is expected.

`cell` does not embed the TypeScript extension runtime. Rust-native executable plugins are the supported path.
