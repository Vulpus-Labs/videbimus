# videbimus: working conventions

## Process

- Decisions live in `adrs/NNNN-slug.md`, one per topic. A change that alters
  a decision updates or supersedes the ADR in the same change.
- Work is tracked in `epics/` and `tickets/` (see `tickets/README.md`). Work
  tickets in dependency order (`depends:` frontmatter). When a ticket's
  acceptance criteria are met, tick them, add a `## Close-out (YYYY-MM-DD)`
  section and `git mv` it to `tickets/closed/`. Close an epic the same way
  when its last ticket closes.
- Commit per ticket, with the ticket number in the subject.

## Code

- Frames are counted as integers (`Frame`). Time in seconds is derived
  from the frame number, and is used only to compute values, never to
  decide which frame something is or whether it is active (ADR 0001).
- Components never start heavy work themselves. They declare the assets
  they need, and the scheduler builds them (ADR 0003, 0004).
- Every task logs. A task that runs a subprocess keeps its full output in
  its log (ADR 0007).
- Config errors name the file, the line, the element and the fix.

## Verification

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```
