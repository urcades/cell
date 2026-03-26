# Plugin Troubleshooting

## The host finds nothing

Check these first:

- the root you added actually contains a descriptor
- the descriptor file is named `pi-plugin-host.json` or `plugin-host.json`
- you added the correct root with `pi-rust plugins add-root`

Use:

```bash
pi-rust plugins list --mode json
```

or:

```bash
pi-rust-plugin-host discover /path/to/root
```

## The plugin launches but is rejected

Common causes:

- descriptor id does not match manifest id
- wrong protocol version
- wrong manifest version
- duplicate capability names
- malformed JSON on stdout

Use:

```bash
pi-rust-plugin-host launch /path/to/pi-plugin-host.json
```

That gives the clearest startup summary.

## The plugin prints to stderr

Stderr noise is allowed. It does not block registration by itself.

If the plugin also hangs, crashes, or sends malformed output, the host reports that as a warning.

## The plugin times out

The host uses request timeouts for startup and runtime dispatch.

If a command, tool, or hook takes too long:

- the request is treated as failed
- the app records a warning
- the main app keeps running

## `cargo run` plugins fail when you isolate `HOME`

If you test plugin roots with a temporary `HOME`, `cargo run` may stop finding the Rust toolchain.

Preserve:

- `CARGO_HOME`
- `RUSTUP_HOME`

and isolate only:

- `HOME`
- `XDG_CONFIG_HOME`

## JS/TS extensions do not run

That is expected.

`pi-rust` does not embed the TypeScript extension runtime. Rust-native executable plugins are the supported direction.
