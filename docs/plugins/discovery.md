# Plugin Discovery

`cell` discovers plugins from configured roots and package install roots.

If you are writing a new plugin, the first thing you create is a descriptor file. Discovery exists to find that descriptor.

## Descriptor names

Use `cell-plugin-host.json` for new plugins.

The host also accepts `plugin-host.json` for compatibility, but `cell-plugin-host.json` is the preferred name for current projects.

The descriptor tells the host what executable to launch, which arguments to pass, and which environment overrides to apply.

## Root types

The current discovery system uses two kinds of roots:

- package install roots
- explicit plugin roots

## Discovery order

The current order is:

1. project package roots
2. user package roots
3. project plugin roots
4. user plugin roots

Paths are normalized and deduplicated before discovery continues.

## Managing plugin roots

Add a user-scoped root:

```bash
cargo run -p cell-cli -- plugins add-root /path/to/plugin-root
```

Add a project-scoped root:

```bash
cargo run -p cell-cli -- plugins add-root /path/to/plugin-root --project
```

Remove a root:

```bash
cargo run -p cell-cli -- plugins remove-root /path/to/plugin-root
```

Inspect discovery results:

```bash
cargo run -p cell-cli -- plugins list
cargo run -p cell-cli -- plugins list --mode json
```

Inspect discovery without the main app:

```bash
cargo run -p cell-plugin-host -- discover /path/to/root
```

## Scope and storage

Project-scoped plugin roots are currently written into `.pi/settings.json`.

That file path is a compatibility holdover. The supported product surface is the `plugins` command group, not the older extension flags.

## Failure behavior

Discovery problems are warnings, not startup blockers.

Typical warnings include:

- unreadable root
- malformed descriptor
- missing executable
- spawn failure
- timeout
- malformed output
- duplicate capability names

Those warnings appear in plugin diagnostics instead of crashing the app.
