---
description: Check the tap-on-shoulder install, config, and ntfy reachability
disable-model-invocation: true
allowed-tools: Bash(CLAUDE_PLUGIN_DATA="${CLAUDE_PLUGIN_DATA}" "${CLAUDE_PLUGIN_ROOT}/scripts/tap.sh" doctor 2>&1 || true)
---

Output of `tap doctor`:

```text
!`CLAUDE_PLUGIN_DATA="${CLAUDE_PLUGIN_DATA}" "${CLAUDE_PLUGIN_ROOT}/scripts/tap.sh" doctor 2>&1 || true`
```

Show the output above to the user verbatim. Do not run anything else.
