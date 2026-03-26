# Release

This is the release process for the nested Rust repo.

## Release flow

1. Run the Rust-native verification flow.
2. Build the release archive.
3. Verify the checksum beside the archive.
4. Create the release commit in the nested Rust repo.
5. Tag that commit as `v<version>`.
6. Push the commit and tag from the nested Rust repo.

## Commands

Verification:

```bash
./scripts/rust_only_ci.sh
```

Packaging:

```bash
./scripts/package_rust_repo.sh
```

## Boundaries

- Use the nested Rust repo for Rust tags and releases.
- Do not rely on the outer repo to publish Rust history.
- Do not commit release archives or checksum files.
