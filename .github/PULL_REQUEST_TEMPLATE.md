## Summary

<!-- Why this change exists. One to three bullets. The diff already shows what changed. -->

-

## Changes

<!-- The actual edits, grouped by area. -->

-

## Test plan

<!-- What you ran, not what you intend to run. Tick only what passed. -->

- [ ] `cargo fmt --check`
- [ ] `cargo check --all-targets`
- [ ] `cargo test`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `npm test --prefix browser-worker` (if the worker or its schema changed)
- [ ] Exercised through a real MCP client (if a tool's behavior changed)

## Checklist

- [ ] Anything reachable through an MCP tool has a test that enters through that tool
- [ ] New operations that fetch remote content go through the robots gate and carry explicit bounds
- [ ] No invariant in `CLAUDE.md` was relaxed, or the change says so and explains why
- [ ] `CLAUDE.md` updated if the module layout, configuration, or invariants changed
