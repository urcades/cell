# Packaging

Build release archives from the Rust repo root.

## Standard package command

```bash
./scripts/package_rust_repo.sh
```

By default this writes:

```text
dist/releases/<version>/cell-<version>-<target>.tar.gz
dist/releases/<version>/cell-<version>-<target>.tar.gz.sha256
```

## Custom output path

```bash
./scripts/package_rust_repo.sh --output /tmp/cell.tar.gz
```

## Archive contents

The current archive includes:

- the release `cell` binary
- `README.md`
- `CHANGELOG.md`
- `LICENSE`
- the `docs/` tree, including maintainer, plugin, architecture, and history pages

## Notes

- The checksum file is written beside the archive.
- Generated archives and checksums stay out of git.
- Packaging does not require `PI_TS_REPO`.
