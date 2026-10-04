# Cloud sessions and teleport

[← README](../README.md) · [Русский](ru/cloud.md)

The dedicated cloud commands select one account using the same secure launch
boundary as `run`, then let Claude Code own the cloud operation itself. The
manager does not call an Anthropic API, inspect credentials, or maintain cloud
session or credit state.

## Start a new cloud session

```bash
claude-acc cloud personal1 "Continue the implementation"
```

This launches Claude with exactly:

```text
--cloud
Continue the implementation
--permission-mode
auto
```

The task stays immediately after `--cloud`. The command always requests Auto
mode because it is intended for hands-off cloud execution. Upstream Claude
only makes Auto available when the account's organization policy permits it
and the selected model supports it; `claude-acc` does not try to predict that
availability. Cloud sessions offer Accept edits, Plan, and Auto, but not
Bypass permissions. Extra flags such as `--dangerously-skip-permissions` are
rejected before Claude starts. To choose another upstream mode or use other
advanced Claude flags, use the transparent escape hatch instead:

```bash
claude-acc run personal1 --cloud "Plan this migration" --permission-mode plan
```

The dedicated command is creation-only by construction: it never passes
`-p`, which upstream Claude requires when `--cloud` sends a follow-up to an
existing session. Therefore even an exact `session_...`, `cse_...`, or
`claude.ai/code/...` string is forwarded as the new session's task
description. R2 does not add a manager-side follow-up command.

### Account and eligibility

For a named account, cloud starts with:

```text
CLAUDE_CONFIG_DIR=~/.claude-switch/accounts/<name>
ANTHROPIC_CONFIG_DIR=~/.claude-switch/accounts/<name>/.anthropic
```

It also receives the full authentication/provider/routing scrub documented in
[Accounts and configuration](accounts.md#secure-launch-boundary). `default`
keeps the normal upstream Claude and Anthropic profile behavior.

Cloud sessions require an eligible claude.ai account and organization policy.
Claude Code currently makes them available on supported Pro, Max, Team, and
Enterprise seats. Amazon Bedrock, Google Cloud's Agent Platform, Microsoft
Foundry, and other third-party provider configurations can be rejected by
upstream Claude. The manager does no eligibility preflight; Claude reports the
actual account or provider error.

### What repository is sent

Upstream Claude decides how to provide the repository to the cloud session. It
normally clones the current GitHub remote and branch when the configured
GitHub access supports that. In other situations, including some repositories
without an eligible remote or GitHub App installation, Claude Code can bundle
and upload the local repository instead.

On native Windows, that bundle may include uncommitted changes to tracked
files without the sensitive-filename exclusions used on macOS, Linux, and
WSL. Before intentionally sending a local bundle, review or stash sensitive
uncommitted tracked changes. `claude-acc` does not commit, stash, push, or
otherwise change Git state before cloud starts.

## Teleport a cloud session

```bash
claude-acc teleport personal1 session_123
```

This launches exact argv:

```text
--teleport
session_123
```

The session value is forwarded unchanged under the selected account's same R1
profile isolation. It is not looked up in local transcripts and is not passed
through `session copy` or `--resume`.

Claude itself verifies that the terminal is authenticated to the same
claude.ai account, is in the correct repository, has a suitable Git state,
and can fetch the cloud branch. It may prompt to stash changes, fetch the
branch, and check out or change the local branch before loading the cloud
conversation. `--teleport` is an upstream cloud-session operation; it is
distinct from reopening local history with `--resume`.

## What the manager does not track

The manager does not list cloud sessions, send follow-up messages, inspect
cloud usage, or calculate cloud-credit balances. Use Claude's own surfaces for
session state and entitlement information.
