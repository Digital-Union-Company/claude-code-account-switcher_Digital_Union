# IDE integration

[← README](../README.md) · [Русский](ru/ide.md)

JetBrains IDEs (PhpStorm, IntelliJ etc.) and terminals launch the `claude` binary from `PATH`. To make those launches account-aware, `claude-acc install` sets up the platform's PATH wrapper:

- macOS/Linux use `~/.claude-switch/bin/claude`, a shell wrapper.
- Windows uses a native `~/.claude-switch/bin/claude.exe`, a second entry point of the manager binary. It re-evaluates the current directory on every invocation and launches the real Claude through the secure structured launcher, excluding the manager bin directory to prevent recursion.
- On macOS/Linux, a symlink `~/.claude-switch/accounts/<name>/ide → ~/.claude/ide` also makes Claude Code and IDE plugins agree on the lock-file directory. Windows lock-directory sharing is still deferred.

`~/.claude-switch/bin` is prepended to `PATH` by shell init. Named Windows accounts receive account-local Claude and Anthropic profile directories, even if the shell exported stale values. Default/unlinked directories retain upstream default-profile behavior.

### The VS Code extension's native UI needs one more step (`vscode`)

Everything above rests on `PATH`, and the Claude Code **VS Code extension does not use it**. Its native UI runs the `claude` binary it ships (`resources/native-binary/claude` inside the extension directory) with the extension host's own environment — so the wrapper never runs, and `CLAUDE_CONFIG_DIR` is whatever a login shell happened to resolve at editor startup, the same value for every workspace. Its one env-var setting, `claudeCode.environmentVariables`, is machine-scoped and can't be set per-project.

Terminal mode (`claudeCode.useTerminal: true`) has never had this problem — that path does resolve `claude` from `PATH`.

The extension has a setting for exactly this: `claudeCode.claudeProcessWrapper`, an executable it calls *instead of* its bundled binary, passing that binary as the first argument and running it with the working directory set to the workspace folder. That is enough to put **the agent** on the right account — read the limits below before deciding it is enough for you:

```bash
claude-acc vscode install     # point installed editors at ~/.claude-switch/bin/claude-vscode
claude-acc vscode status      # what each editor currently points at
claude-acc vscode uninstall   # remove the setting again
```

```
$ claude-acc vscode status
VS Code process wrapper:
    VS Code            off — the native UI ignores the account
    Cursor             off — the native UI ignores the account
  Turn it on:  claude-acc vscode install

$ claude-acc vscode install
VS Code: claudeCode.claudeProcessWrapper -> /Users/you/.claude-switch/bin/claude-vscode
Cursor: claudeCode.claudeProcessWrapper -> /Users/you/.claude-switch/bin/claude-vscode
With a process wrapper set, the extension resolves the permission mode itself instead of deferring to the CLI, and stops checking for its own updates. Both are its behaviour, not ours; undo with claude-acc vscode uninstall.
Restart the editor for it to take effect.
```

It covers VS Code, VS Code Insiders, VSCodium and Cursor — whichever are installed. `claude-acc install` only *mentions* it; the setting is machine-scoped and lives in your editor's config, so writing it is opted into rather than done for you.

#### What this does not cover

- **This puts the agent on the right account, not the whole extension.** A wrapper sets the environment of the process it launches, and only that. The extension *host* — the session list and picker, MCP and plugin persistence in `.claude.json`, file history, plans — resolves the config dir from its own environment, which never receives ours and architecturally cannot: in 2.1.252 that is `process.env.CLAUDE_CONFIG_DIR ?? ~/.claude`, read by the host process. So conversations run on the workspace's account while the picker still lists `~/.claude`'s history, and MCP servers and plugins do not separate per account. Upstream tracks the host half as [anthropics/claude-code#30538](https://github.com/anthropics/claude-code/issues/30538); closing it needs a setting the extension reads for its own process, which no wrapper can substitute for.
- **Only the default VS Code profile is set up.** `claudeCode.claudeProcessWrapper` is machine-scoped, and VS Code keeps only `application`-scoped settings outside a profile — its own UI strings say so: an application setting "is not specific to the current profile, and will retain its value when switching profiles", and `settings.applyToAllProfiles` exists precisely so other settings can opt in. A profile carrying its own settings therefore reads those *instead of* the file this writes, with no fallback, and a window on it goes on ignoring the account. `vscode install` and `vscode status` name such profiles and stop reporting a flat "on", since that would be a false positive — worse than doing nothing, because you would look for the fault elsewhere. Two ways out: work in the default profile, or add `claudeCode.claudeProcessWrapper` to `settings.applyToAllProfiles`. (A profile created without ticking Settings shares the default profile's file, so it is already covered.) Writing every profile is a follow-up.
- **Two behaviours of the extension change** when any process wrapper is set, ours or anyone's: it resolves the permission mode itself instead of deferring to the CLI, and it stops checking for its own updates. `vscode uninstall` puts both back.
- **`settings.json` is edited as text, not reserialised.** It is JSONC — comments and trailing commas are legal, and round-tripping it through a JSON parser would delete every comment in it. Only the one key's value is touched; a file that isn't a JSON object is reported and left alone, and a wrapper pointing at another tool is never replaced without `--force`.
- **Windows native UI wiring remains deferred.** The Windows PATH shim is a native `.exe`, but the extension passes its bundled Claude executable as an extra first argument under a different wrapper contract. R3A intentionally does not point `claudeCode.claudeProcessWrapper` at the PATH shim or enable `vscode install` on Windows. Terminal mode and other PATH-honoring launchers are covered.
- **Remote-SSH, WSL and code-server are not covered.** The wrapper would have to live on the remote machine, which is a different problem.
- **Windows IDE lock-directory sharing remains deferred.** No symlink or junction is created for `<account>/ide` and `~/.claude/ide`.
- **Windows mapping normalization remains deferred.** Existing raw links keep their exact drive/slash/case, junction, UNC and verbatim-path semantics.
