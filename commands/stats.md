---
description: Show how long your agents sat blocked on you (local idle ledger)
disable-model-invocation: true
allowed-tools: Bash(CLAUDE_PLUGIN_DATA="${CLAUDE_PLUGIN_DATA}" "${CLAUDE_PLUGIN_ROOT}/scripts/tap.sh" stats 2>&1 || true)
---

Output of `tap stats`:

```text
!`CLAUDE_PLUGIN_DATA="${CLAUDE_PLUGIN_DATA}" "${CLAUDE_PLUGIN_ROOT}/scripts/tap.sh" stats 2>&1 || true`
```

Show the output above to the user verbatim. Do not run anything else.
