# Repo Workflow

This page is the maintainer landing page for the Rust `cell` repo.

## Repo boundaries

`/Users/edouard/Developer/pi/rust` is the source of truth for the Rust product.

- Do normal Rust work inside this repo.
- Use the outer TypeScript repo only when you intentionally need parity tooling.
- Do not treat the outer repo as the owner of Rust history, branches, or releases.

## Maintainer docs

- [Verification](./maintainers/verification.md)
- [Packaging](./maintainers/packaging.md)
- [Release](./maintainers/release.md)
- [Maintainer Guide Index](./maintainers/README.md)

## Common source-run commands

Main help:

```bash
cargo run -p cell-cli -- --help
```

Interactive mode:

```bash
cargo run -p cell-cli --
```

Plugin host help:

```bash
cargo run -p cell-plugin-host -- --help
```

## Plugin root workflow

Manage plugin roots through the built-in `plugins` command group.

User scope:

```bash
cargo run -p cell-cli -- plugins add-root /absolute/path/to/plugins
```

Project scope:

```bash
cargo run -p cell-cli -- plugins add-root /absolute/path/to/plugins --project
```

Inspect plugin runtime diagnostics:

```bash
cargo run -p cell-cli -- plugins list
cargo run -p cell-cli -- plugins list --mode json
```

Project-scoped settings still live in `.pi/settings.json` for compatibility.
