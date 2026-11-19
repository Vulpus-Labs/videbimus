---
id: "0016"
title: "Command recipe: probe timeouts, probing only when needed, undeclared programs"
priority: low
created: 2026-11-19
epic: E001
depends: []
---

## Summary

From 0011's close-out:

- tool version probes have no timeout, so a hanging probe blocks loading;
- probes run at load, so a missing Blender fails even `status`;
- programs run without being declared as tools (`sh`, `cp`) aren't
  version-keyed.

## Acceptance criteria

- [ ] Probes time out, with a located error naming the tool.
- [ ] `status` and `review` don't fail on a tool that's missing but not
      needed to answer them.
- [ ] A decision, recorded in ADR 0009 §3: undeclared programs are allowed
      with a warning, or must be declared.
