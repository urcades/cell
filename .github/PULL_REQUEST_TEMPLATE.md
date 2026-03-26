## Summary

Describe the change in plain language.

## Verification

- [ ] `cargo test --workspace`
- [ ] `cargo check --workspace --lib --bins`
- [ ] `env -u PI_TS_REPO cargo test -p cell-cli --test tui_parity` if interactive behavior changed
- [ ] `./scripts/rust_only_ci.sh` if CI, packaging, release flow, or cross-cutting behavior changed
- [ ] Docs updated when commands, workflows, or support boundaries changed

## Notes

Anything reviewers should pay special attention to.
