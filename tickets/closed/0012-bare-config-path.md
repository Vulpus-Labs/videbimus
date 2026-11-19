---
id: "0012"
title: "A bare --config file name breaks command recipes"
priority: high
created: 2026-11-19
epic: E001
depends: []
---

## Summary

Found by the fenland project (0008). With `--config fenland.kdl`, the
config file's parent is the empty path. `std::path::absolute("")` fails, so
a command recipe's default working directory was empty and spawning
failed: "couldn't start python3 … No such file or directory". Paths derived
from the config were relative, where ADR 0009 §3 says they're absolute.

## Acceptance criteria

- [x] `Project::load` makes the config path absolute before anything is
      derived from it.
- [x] The command recipe's `dir_of` treats an empty parent as `.`.
- [x] Regression test: the stock binary, run from a project directory with
      `--config video.kdl`, runs a command in that directory.

## Close-out (2026-11-19)

The fix is in `crates/videbimus/src/project.rs` (`Project::load`) and
`crates/videbimus/src/command/mod.rs` (`dir_of`). The regression test is
`a_bare_config_name_runs_commands_in_its_directory` in
`crates/videbimus/tests/command.rs`. It runs the stock binary as a
subprocess, because changing the test process's own working directory
would race with the other tests. fenland's `cwd "."` workaround can now be
removed.
