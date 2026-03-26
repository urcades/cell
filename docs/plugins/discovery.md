# Plugin Discovery

`pi-rust` discovers plugins from two kinds of roots:

- package install roots
- plugin roots

## Discovery order

The current order is:

1. project package roots
2. user package roots
3. project plugin roots
4. user plugin roots

Paths are normalized and deduplicated before discovery continues.

## Descriptor files

The host looks for either:

- `pi-plugin-host.json`
- `plugin-host.json`

The descriptor points to an executable plus optional arguments and environment variables.

## Plugin roots

Plugin roots are configured through the Rust-native `plugins` command surface.

Add a root:

```bash
pi-rust plugins add-root /path/to/plugin-root
```

Add a project-scoped root:

```bash
pi-rust plugins add-root ./plugins --project
```

Remove a root:

```bash
pi-rust plugins remove-root /path/to/plugin-root
```

List discovered plugins:

```bash
pi-rust plugins list
pi-rust plugins list --mode json
```

## Important boundary

This discovery system is for Rust-native executable plugins.

Legacy `--extension` and `--no-extensions` flags are still unsupported for JS/TS execution. They are not the plugin configuration surface.

## Failure behavior

Discovery problems are warnings, not startup blockers:

- unreadable root
- malformed descriptor
- missing executable
- spawn failure
- timeout
- malformed output
- duplicate capability names

Those warnings appear in plugin diagnostics instead of crashing the app.
