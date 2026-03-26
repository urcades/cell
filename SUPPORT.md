# Support

## Before opening an issue

Start with the main docs:

- [`README.md`](./README.md) for setup, running, packaging, and verification
- [`docs/README.md`](./docs/README.md) for the documentation index
- [`docs/plugins/README.md`](./docs/plugins/README.md) for plugin authoring and troubleshooting

Command-level help is also available locally:

```bash
cargo run -p cell-cli -- --help
cargo run -p cell-plugin-host -- --help
```

## Where to ask for help

- Bug reports and regressions: open a GitHub issue with the bug report template
- Feature requests: open a GitHub issue with the feature request template
- Security concerns: follow [`SECURITY.md`](./SECURITY.md) and report them privately

## What to include in a bug report

- the `cell` version, tag, or commit you tested
- your operating system and terminal environment
- the exact command you ran
- what happened and what you expected instead
- reproduction steps
- relevant logs or screenshots, with secrets removed

## Support boundary

This repository supports the Rust-native `cell` product and its documented plugin system. Optional parity tooling with the outer TypeScript repo is available when explicitly documented, but normal Rust development and support should not depend on that outer tree.
