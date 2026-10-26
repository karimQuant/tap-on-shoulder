---
description: Send a test tap to your phone
disable-model-invocation: true
allowed-tools: Bash(CLAUDE_PLUGIN_DATA="${CLAUDE_PLUGIN_DATA}" "${CLAUDE_PLUGIN_ROOT}/scripts/tap.sh" test 2>&1 || true)
---

Output of `tap test`:

```text
!`CLAUDE_PLUGIN_DATA="${CLAUDE_PLUGIN_DATA}" "${CLAUDE_PLUGIN_ROOT}/scripts/tap.sh" test 2>&1 || true`
```

Show the output above to the user verbatim. Do not run anything else.
