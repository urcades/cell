# Changelog

## [Unreleased]

### Added
- Nested Rust repository foundation with Rust-local hygiene files and standalone CI.
- Explicit extension capability matrix documenting fixable parity gaps versus pure-Rust stop points.

### Changed
- Repository docs now describe the Rust workspace as a standalone nested project with optional TypeScript parity tooling.
- TS parity bridge tooling now requires explicit `PI_TS_REPO` configuration instead of assuming the parent checkout.

### Fixed
- Ignored Rust build output and local parity scratch files in the Rust repository.
- Standalone Rust workspace tests no longer fail when the TypeScript parity bridge is not configured.
- RPC `abort_retry` and `abort_bash` no longer return stubbed “not implemented” errors.
- OpenRouter requests now identify the standalone Rust repository instead of the old parent monorepo.
