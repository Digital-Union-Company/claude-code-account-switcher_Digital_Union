# Machine API: `--json` for scripts and GUIs

[← README](../README.md) · [Русский](ru/machine-api.md)

Six commands have a `--json` mode meant for a script or another program to
parse, not a human to read. Five are read-only — `list`, `doctor`, `status`,
`links`, `usage` — and one is the single mutation the machine API exposes,
`link --json`, which conditionally writes one directory→account routing link.
This is the contract a tool like a GUI front-end builds against.

## The rules every `--json` command follows

- **Exactly one JSON document on stdout, nothing else.** No update-available
  hint, no progress line, no decorative text. A consumer that sees anything
  else on stdout should treat the whole invocation as failed rather than try
  to find the JSON inside the noise.
- **`schema_version`** is an integer, independent per command, starting at
  `1`. A future release may add fields without bumping it; it only bumps
  when an existing field's *meaning* changes or a field is removed.
- **`ok: true`** means a result document was produced — not that everything
  it describes is healthy. `doctor --json` is the clearest example: `ok:
  true` can appear right next to an `offline` account and a non-zero exit
  code (see below).
- **Operational failures** — claude-acc's own config store unreadable, or
  equivalent — are the only case that produces the error envelope:
  ```json
  {
    "schema_version": 1,
    "ok": false,
    "error": { "code": "SOME_STABLE_CODE", "message": "for a human, not for parsing", "details": {} }
  }
  ```
  Branch on `error.code`, never on `message` — `message` may read
  differently between CLAUDE_ACC_LANG settings or versions; `code` will not.
- **No field anywhere carries a secret** — not a token, not a hash, not a
  credential-file path's contents.
- Paths are absolute. Supported platforms use their native separator
  (`\` on Windows, `/` elsewhere); nothing is rewritten for display.
- **Path equivalence is platform-dependent, and only Windows's is
  case/slash-insensitive.** On Windows, claude-acc treats case and `/`
  vs `\` as equivalent when comparing stored links (`src/path_identity.rs`).
  On every other supported platform, path equivalence is exact-string —
  `/Users/alice/work` and `/Users/alice/Work` are two unrelated,
  non-conflicting paths there, not an ambiguity. The examples below use
  repeated, identically-spelled stored paths precisely so they hold
  regardless of platform.

## `claude-acc list --json`

Fast, local, no network — safe to call on every GUI startup. Every field
comes from data already on disk.

```bash
$ claude-acc list --json
{
  "schema_version": 1,
  "ok": true,
  "accounts": [
    {
      "name": "work",
      "managed": true,
      "default": true,
      "auth_present": true,
      "cached_email": "alice@anthropic.com",
      "cached_uuid": "aa6c22d5-...",
      "cached_plan": "Max 20x",
      "token_changed_since_audit": false,
      "chrome_enabled": true,
      "config_dir": "/Users/alice/.claude-switch/accounts/work"
    },
    {
      "name": "default",
      "managed": false,
      "default": false,
      "auth_present": false,
      "cached_email": null,
      "cached_uuid": null,
      "cached_plan": null,
      "token_changed_since_audit": false,
      "chrome_enabled": null,
      "config_dir": "/Users/alice/.claude"
    }
  ]
}
```

- The standard, unmanaged `~/.claude/` account — when it appears — always
  uses the stable name `"default"` with `"managed": false`. It shows up only
  once it has actually been used (a real login, or an existing `doctor`
  cache), the same visibility rule the human `list` uses.
- `auth_present` is a local token-presence check only — no network request.
  It is *not* the same question as "is the cached identity fresh": an
  account can have `auth_present: true` with no `cached_email` yet, if
  `doctor` has never audited it.
- `token_changed_since_audit` means the OAuth token on disk no longer
  matches the one `doctor` last cached — usually a routine refresh, not
  itself an identity judgement (that's `doctor`'s job).
- `chrome_enabled` is `true`/`false`/`null` — `null` means this config dir
  has never been offered Claude in Chrome, not "off."
- Error: `CONFIG_STORE_UNREADABLE` if the account inventory itself
  couldn't be enumerated.

## `claude-acc doctor --json`

The identity/lock audit authority — slower than `list`, since it hits the
OAuth profile API live. Additive on top of the existing shape (`doctor
--json` predates this contract): `schema_version`, `ok`, and per-row
`chrome_enabled`/`config_dir` are new; `accounts`/`standard` and their
`status`/`email`/`uuid`/`plan`/`lock`/`pinned_uuid` fields are unchanged.

```bash
$ claude-acc doctor --json
{
  "schema_version": 1,
  "ok": true,
  "accounts": [
    {
      "name": "work",
      "status": "ok",
      "email": "alice@anthropic.com",
      "uuid": "aa6c22d5-...",
      "plan": "Max 20x",
      "default": true,
      "lock": "ok",
      "pinned_uuid": "aa6c22d5-...",
      "chrome_enabled": true,
      "config_dir": "/Users/alice/.claude-switch/accounts/work"
    }
  ],
  "standard": null
}
```

**The exit code keeps its pre-existing, separate meaning** — this is not an
operational-failure signal. `claude-acc doctor --json` exits non-zero when
any account is `"status": "offline"` or `"lock": "drift"`; a `"no_token"`
account alone never does. A fully valid document is always printed first,
so a `--json` caller must parse stdout regardless of the exit code — a
non-zero exit here means "an audited account needs attention," never
"claude-acc couldn't produce a result."

## `claude-acc status --json [--path <dir>]`

The single authoritative answer to "which account would `claude` actually
resolve for this directory right now." Never reimplement this lookup —
call it with the directory you care about, as cwd or via `--path`
(`--path` requires `--json`).

```bash
$ claude-acc status --json
{
  "schema_version": 1,
  "ok": true,
  "query_path": "/Users/alice/work/project-a",
  "resolved_account": "work",
  "source": "linked",
  "owning_link_path": "/Users/alice/work"
}
```

- `source` is one of `"linked"` (an ancestor directory link matched — see
  `owning_link_path` for which one), `"default"` (no link, a configured
  managed default applies), or `"standard"` (no link, no managed default).
- The standard account is `resolved_account: "default"` — never `null` —
  the same stable identifier `list --json` uses, so a project registry
  entry can be compared directly against this field with no special-casing.
- There is no `ambiguous` field. Ambiguity (two+ equivalent links naming
  different accounts) has exactly one representation — the error envelope:
  ```json
  {
    "schema_version": 1,
    "ok": false,
    "error": {
      "code": "AMBIGUOUS_LINK",
      "message": "...",
      "details": {
        "query_path": "/Users/alice/Work",
        "mappings": [
          { "stored_path": "/Users/alice/Work", "account": "personal1" },
          { "stored_path": "/Users/alice/Work", "account": "personal2" }
        ]
      }
    }
  }
  ```
  (Two *identically-spelled* stored links naming different accounts — the
  simplest ambiguity that holds on every platform. On Windows, two
  case/slash-variant spellings of the same directory would conflict the
  same way; on other platforms they would not, since path equivalence
  there is exact-string — see the note above.)
- `--path` errors: `PATH_NOT_FOUND` if it doesn't exist, `PATH_NOT_DIRECTORY`
  if it exists but isn't a directory.

## `claude-acc links --json`

The stored directory→account map, plus a genuine whole-store conflict
analysis — never a mutation path.

```bash
$ claude-acc links --json
{
  "schema_version": 1,
  "ok": true,
  "links": [
    { "stored_path": "/Users/alice/work", "account": "personal1" },
    { "stored_path": "/Users/alice/work", "account": "personal2" }
  ],
  "conflicts": [
    {
      "code": "AMBIGUOUS_EQUIVALENT_PATHS",
      "mappings": [
        { "stored_path": "/Users/alice/work", "account": "personal1" },
        { "stored_path": "/Users/alice/work", "account": "personal2" }
      ]
    }
  ]
}
```

- `links` is the complete, unconditional dump, in the order stored —
  original spelling (case, separators, every character of the path and
  account name itself), never normalized for display. The one exception
  is not a normalization at all but the stored format's own syntax:
  whitespace immediately touching the `=` delimiter is trimmed when the
  line is parsed, the same way it would be for any `key=value` file — a
  line written as `  /Users/alice/work  =personal1` stores
  `/Users/alice/work`, not a copy padded with those spaces. This is not
  byte-for-byte line preservation; it is spelling preservation of the
  path and account values the delimiter actually separates.
- A `conflicts` group appears only when two or more *distinct* accounts
  share an equivalence class; the same account linked under two equivalent
  spellings is unremarkable and stays as ordinary `links` entries. No
  member is ever dropped or picked as canonical.
- Unlike the human `links` command (which silently skips a line it can't
  parse), `--json` reports it: `LINKS_STORE_INVALID`, with
  `details.lines: [{line_number, raw}, ...]` for every offending line,
  rather than quietly returning an incomplete map as if it were whole.
  `LINKS_STORE_UNREADABLE` covers the filesystem-level failure instead.

## `claude-acc usage --json`

Informational only — utilization and reset timestamps, never plan (that's
owned by `list --json`'s `cached_plan` / `doctor --json`'s `plan`). Never
gates anything; a slow or failed fetch degrades to `"unavailable"`.

```bash
$ claude-acc usage --json
{
  "schema_version": 1,
  "ok": true,
  "accounts": [
    {
      "name": "work",
      "source": "live",
      "fetched_at": "2026-10-05T12:00:00Z",
      "five_hour": { "utilization": 42, "resets_at": "2026-10-05T14:14:00Z" },
      "seven_day": { "utilization": 11, "resets_at": "2026-10-10T00:00:00Z" }
    },
    {
      "name": "personal",
      "source": "unavailable",
      "fetched_at": null,
      "five_hour": null,
      "seven_day": null
    }
  ]
}
```

- `source: "live"` — fetched this call. `"cache"` — the live request
  failed, so Claude Code's own last reading was used instead (refused if
  it doesn't belong to the identity currently signed in). `"unavailable"` —
  neither worked.
- A cached window whose `resets_at` has already passed is `null` rather
  than shown as current — the same suppression the human `usage` command
  applies to a stale reading. A *live* reading is never suppressed this
  way.
- Same account visibility as `list --json`: the standard account appears
  only once it has actually been used.

## `claude-acc link --json --path <DIR> --account <ACCOUNT>`

The **one mutation** in the machine API: it makes `<DIR>`'s own exact link name
`<ACCOUNT>`, so `status --json` for `<DIR>` then resolves `<ACCOUNT>` by an
exact link of its own. Built for a GUI that wants to apply its registry's
account onto claude-acc's routing, as a single conditional write — never a
`unlink`-then-`link` sequence a consumer drives itself.

It requires **exactly one** of two modes, and both `--path` and `--account`:

- `--dry-run` — resolve, plan, and return an `expected_state` token. Writes
  nothing.
- `--expect-state <token>` — commit, but only if the routing state still matches
  that token from a prior `--dry-run`.

A machine mutation is therefore never unconditional: the consumer previews,
shows the user, and commits against the state the user actually saw.

### Scope — exactly one link, nothing else

It only ever adds, replaces, or leaves `<DIR>`'s own *exact-equivalent* link; a
change collapses case/slash aliases of `<DIR>` to the single chosen entry (on
Windows), while an already-correct exact link is left byte-for-byte untouched.
Ancestor, descendant and unrelated links are never touched. Because routing is
inherited — the nearest linked ancestor wins — writing an exact link for `<DIR>`
**pins** it: a directory that previously inherited its account now has its own
link, and a later change to the ancestor no longer moves it. The `before`/`after`
routes make that visible. Writing an exact link for `<DIR>` changes `<DIR>`'s
effective account and that of any descendant inheriting from it — it is one
*stored* link, not necessarily one *effective* directory.

claude-acc owns every routing and path-equivalence decision here (`src/resolve.rs`,
`src/path_identity.rs`); a consumer passes the directory and account as strings
and never compares paths or reads the links file itself.

### Preview — `--dry-run`

```bash
$ claude-acc link --json --path /Users/alice/work/project-a --account work --dry-run
{
  "schema_version": 1,
  "ok": true,
  "dry_run": true,
  "would_change": true,
  "query_path": "/Users/alice/work/project-a",
  "account": "work",
  "before": { "resolved_account": "default", "source": "standard", "owning_link_path": null },
  "after": { "resolved_account": "work", "source": "linked", "owning_link_path": "/Users/alice/work/project-a" },
  "stored_link_path": "/Users/alice/work/project-a",
  "expected_state": "42cfd5972ba543f2f876687273ffc9b737971b0b4589fc342d71cd4b95991f77"
}
```

- `after` is the **projected** route (`dry_run: true` / `would_change`), not a
  claim that anything was written.
- `expected_state` is a 64-hex-character fingerprint of every routing-relevant
  input — the directory, the account, the whole links store, the configured
  default, the account inventory, **and the resolved routing interpretation
  itself**: the before-route claude-acc resolves for `<DIR>`, the complete
  ordered set of `<DIR>`'s own exact-equivalent stored entries (every entry a
  commit would drop and replace, not merely the first), and `<DIR>`'s own
  resolved filesystem identity. It is a conservative **state fingerprint, not
  authentication and not a transaction id**: an identical restored state
  produces an identical token, so it cannot tell "never changed" from "changed
  and changed back" (an ABA change). It refuses on **any** routing-store change,
  including ones unrelated to this directory. It carries no secret — only
  directory strings, account names and the stored map.
- Because the resolved route, the exact-equivalence set and the directory
  identity all come from claude-acc's resolver and `path_identity` — which
  consult live filesystem identity (`canonicalize`) on platforms that have it —
  a symlink or junction retarget that changes how `<DIR>` is interpreted, or
  which stored entries fall in its equivalence class, flips the token and is
  refused at the expected-state comparison, even when no stored string moved.
  What the token still cannot cover is the narrow window **after** that
  comparison and **before** the atomic write (see "Atomicity, locking, and who
  is bound"); there the post-write re-verification — not the token — is the
  final guard that `<DIR>` ends up resolving to `<ACCOUNT>` by its own exact
  link.
- `would_change` is `false` when `<DIR>` already has an exact link to
  `<ACCOUNT>`. An *inherited* route that already resolves to `<ACCOUNT>` is
  **not** a no-op: there is no exact link yet, so committing would still write
  the pinning link, and `would_change` is `true`.

### Commit — `--expect-state <token>`

```bash
$ claude-acc link --json --path /Users/alice/work/project-a --account work \
    --expect-state 42cfd5972ba543f2f876687273ffc9b737971b0b4589fc342d71cd4b95991f77
{
  "schema_version": 1,
  "ok": true,
  "dry_run": false,
  "changed": true,
  "query_path": "/Users/alice/work/project-a",
  "account": "work",
  "before": { "resolved_account": "default", "source": "standard", "owning_link_path": null },
  "after": { "resolved_account": "work", "source": "linked", "owning_link_path": "/Users/alice/work/project-a" },
  "stored_link_path": "/Users/alice/work/project-a"
}
```

- `after` here is the **verified** route, re-read from the store after the write.
- A commit that finds the exact link already present is a **no-op success**:
  `changed: false`, the store is not rewritten, and `before`/`after` are equal.

### Outcomes, and what each means for the store

A `link --json` commit ends in exactly one of four states, and the distinction
matters because they do not all leave the store in the same place:

- **A definite pre-commit refusal** — every `error.code` listed below *except*
  `POSTCONDITION_FAILED`. The refusal is decided before any write, so the store
  is byte-for-byte unchanged. These are the cases that write nothing.
- **A successful no-op** (`ok: true`, `changed: false`) — the exact link was
  already present; the store is not rewritten and `before`/`after` are equal.
- **A successful committed mutation** (`ok: true`, `changed: true`) — the exact
  link was written and re-verified; `after` is the route re-read from the store.
- **An uncertain post-commit outcome** (`POSTCONDITION_FAILED`) — the write was
  attempted and the store **may have changed**, but routing did not re-verify as
  expected. This is a refusal envelope too, but unlike the others it is **not** a
  guarantee that nothing was written; it is never reported as "unchanged" and is
  never auto-rolled-back.

Every refusal is an operational-failure envelope (`ok: false`, exit 1). Branch
on `error.code`, never on `message`:

- `STATE_CHANGED` — the `--expect-state` token no longer matches; the state the
  user confirmed has moved. `details` carries the current `before` route.
  Re-preview and decide again.
- `AMBIGUOUS_LINK` — routing is already ambiguous for this directory (two
  equivalent stored links naming different accounts). It is refused, never
  silently healed; the store is left exactly as it was.
- `ACCOUNT_NOT_FOUND` / `ACCOUNT_NAME_INVALID` — the account is not a managed
  account (or the standard `default`), or is not a valid account name.
- `PATH_NOT_FOUND` / `PATH_NOT_DIRECTORY` — as for `status --json`.
- `PATH_NOT_REPRESENTABLE` — the directory cannot be stored losslessly in the
  `dir=account` line format (e.g. it embeds a newline, or whitespace the parser
  would trim). The store is never corrupted by a path it cannot represent.
- `LINKS_STORE_INVALID` / `LINKS_STORE_UNREADABLE` — the store is malformed or
  unreadable; a malformed store is refused, never rewritten by this path.
- `CONFIG_STORE_UNREADABLE` — the config file (which holds `default=`) or the
  account inventory exists but could not be read or decoded. Unlike the human
  path, which treats an unreadable config as "no default", the machine path
  fingerprints and reports the default as part of the route, so it refuses
  loudly rather than committing against a skewed before/after route and token.
- `NOT_INITIALIZED` — claude-acc has no routing store yet. This verb never
  creates one (so a dry-run has no side effect); initialize it by running any
  other claude-acc command first.
- `LINKS_STORE_LOCKED` — another cooperating writer holds the routing lock; retry.
- `LINKS_STORE_WRITE_FAILED` — the write itself failed; the store is unchanged.
- `POSTCONDITION_FAILED` — the link was written but routing does not re-verify as
  expected. The outcome is **uncertain**: the store may have changed, so this is
  never reported as "unchanged", and it is **not** auto-rolled-back. Re-read
  routing (`status --json`, `links --json`) and reconcile before any retry.
- `INVALID_ARGUMENTS` — not exactly one of `--dry-run`/`--expect-state`, or
  `--path`/`--account` missing.

### Atomicity, locking, and who is bound

- **Atomic store writes.** The links store (and the config file's `default=`)
  are replaced via a same-directory temp file, fsync, then rename, so a reader
  only ever sees the complete old or complete new store — never a torn one — and
  a crash between steps leaves the previous store intact. This is
  process-interruption safety, **not** a power-loss durability guarantee of the
  rename metadata itself (the directory is not fsynced).
- **One routing lock.** A single OS advisory lock serializes every writer of
  routing-relevant state in *this binary* — the machine mutation, the human
  `link`/`unlink`, and `default`/account-removal changes — so their
  read-modify-writes can never interleave. It is released automatically when the
  process exits, so a crashed holder leaves nothing stale, and nothing is ever
  deleted by age.
- **It binds only cooperating writers in this binary, and the limits are
  exact.** An *older* claude-acc without the lock, a direct hand-edit of the
  links file, or the shell integration (`claude-switch.sh`, a different
  mechanism) are **not** serialized by it. The defences against a
  non-cooperating write are layered, and so are their gaps:
  - A change visible **before** the commit's expected-state comparison, or one
    that lands **between** that comparison and the final write, is caught — by
    the token check and by a second conservative re-read taken immediately
    before the replacement — and refused with `STATE_CHANGED`. The other write
    survives.
  - A non-cooperating write in the remaining window — **after** that last
    re-read and **before** the atomic rename — is not detected in advance, and
    that write is silently overwritten. The post-write re-verification still
    catches a wrong *final* route (`POSTCONDITION_FAILED`), but it cannot
    resurrect a concurrent write it clobbered. This window is narrowed, not
    closed.
  - A directory's filesystem identity changing outside the lock (a
    reparse/junction retarget) is likewise not serialized; the token binds the
    resolved route, the complete exact-equivalence set and the directory's own
    resolved identity to catch it at comparison time — including a retarget that
    grows or shrinks which stored entries fall in `<DIR>`'s equivalence class
    without changing any stored string — but not within that final window.
  - Because the token is a state *fingerprint*, an ABA change — a value changed
    and then restored identically — is indistinguishable from no change.
  - **Operational prerequisite:** a consumer must not drive this verb against a
    store another non-cooperating writer (an older binary, the shell
    integration, a hand-edit, an external tool) is concurrently mutating. Within
    this binary's own commands the lock makes that safe; across those other
    writers it is the caller's responsibility, and the guarantee degrades to the
    layered best-effort above.

## `claude-acc account-action --json`

The machine contract for a GUI to **create** a new managed account or
**reauthenticate** an existing one, so a user never has to hand-type
`claude-acc add` / `claude-acc login` in a terminal. It is machine-only: humans
use `add`/`login`. `claude-acc` stays the sole owner of identity, credentials
and the authentication interaction throughout.

### Why this verb is shaped differently from the read-only `--json` commands

The supported Claude Code login (`claude auth login`) is **strictly
interactive**: its only options are `--claudeai/--console/--email/--sso`; there
is no `--json`, no headless/device-code mode, and no secret-safe redirected code
entry. A GUI therefore cannot observe or feed the OAuth code, and per-keystroke
progress is simply not available upstream. The honest design that follows:

- **The caller hosts the terminal.** The consumer launches
  `claude-acc account-action` in a terminal it owns; the interactive
  `claude auth login` inherits that terminal, exactly as human `add`/`login` do.
  The OAuth code is entered there, in claude-acc's own authentication surface —
  the GUI never sees it.
- **The machine channel is a file, not stdout.** Because the terminal is busy
  hosting the login, the single structured outcome document is written to the
  caller-supplied `--result-file`, never mixed into the login's stdout. It
  carries only non-secret fields (account name, the coarse outcome, the
  reconciled **public** identity email/uuid) — never a code, token, credential,
  cookie or authorization header.
- **Discovery without authenticating.** `--probe` reports the contract version
  and exactly which capabilities exist (and which do not), performing no
  authentication and touching nothing.

The outcome is **coarse by necessity**, and deliberately **truthful about what
was actually verified**. A clean login *exit* together with a local
`.claude.json` identity is **not** proof the account is authenticated now — that
file is Claude Code's own cache and can be stale from an earlier login. So
`authenticated` is reported **only** when an authoritative live audit (a token
read + profile fetch — the same substrate `add`/`login`/`doctor` use) confirms a
current credential. The full vocabulary:

- `authenticated` — a live audit confirmed a current credential.
- `login_completed` — the login exited `0` and a local identity is present, but
  it was **not** authoritatively verified (offline / no token). The consumer
  **must reconcile** (run `doctor`/`list`) before treating the account as signed
  in.
- `login_completed_no_identity` — exited `0` yet no local identity landed.
- `login_failed` — the login exited non-zero. Upstream cannot tell a user
  *cancellation* from a *failure*, and for a **reauth** this does **not** mean
  the account lost whatever authentication it already had — reconcile.
- `spawn_failed` — the login child could not be started at all.

Exactly one authoritative audit is performed per action, reused for both the
`authenticated` determination and the duplicate-account disclosure (never two
network round-trips). This limitation is reported in the probe output rather
than papered over. The `contract_version` is independent of the envelope
`schema_version`: it is the number a consumer pins to decide whether this
create/reauth contract is present, and it is `1`.

### Capability discovery — `--probe`

`claude-acc account-action --json --probe` performs no authentication and emits
the capability document on **stdout** (the one case where stdout is used, since
no login is hosted):

```json
{
  "schema_version": 1,
  "ok": true,
  "contract": "account-action",
  "contract_version": 1,
  "provider_version": "0.23.0",
  "actions": ["create", "reauth"],
  "interactive_hosting": "caller_terminal",
  "result_channel": "result_file",
  "result_file_requirements": "absolute path in a caller-owned directory outside the provider and standard Claude stores; must not already exist",
  "oauth_code_entry_feedback": "unavailable",
  "progress_granularity": "final_outcome_only",
  "distinguishes_cancellation_from_failure": false,
  "outcomes": ["authenticated", "login_completed", "login_completed_no_identity", "login_failed", "spawn_failed"],
  "authenticated_requires_live_audit": true,
  "reconciliation": "run doctor/list after any outcome other than 'authenticated' before trusting account state",
  "seed_supported_actions": ["create"],
  "secret_safe": true
}
```

### Running an action — `--action create|reauth --account <name> --result-file <path> [--seed]`

`--json` is required (it selects this machine contract). `--action`, `--account`
and `--result-file` are required unless `--probe` is given. `create` reserves a
new managed account directory, then hosts the login; `reauth` requires the
managed account to already exist. `--seed` applies **only to `create`** (it
seeds the new directory from `~/.claude/`, exactly like `add --seed`); passing it
with `reauth` is refused (`SEED_NOT_SUPPORTED`), never silently ignored.

A create/reauth requires an **already-initialized** store and never manufactures
one: on an uninitialized store it refuses with `NOT_INITIALIZED` rather than
creating routing state as a side effect of a GUI-triggered action.

**The `--result-file` is validated before any profile creation or login (F1).**
It must be an **absolute** path, naming a file in an **existing, real directory**
that is **outside** the claude-acc store and the standard Claude configuration,
and it **must not already exist**. The provider proves the directory is writable
up front, then publishes the single document atomically (unique temp + rename)
**without replacing** any file that appears at the destination. A pre-existing or
stale file is therefore never mistaken for this action's outcome. One residual
race remains and is disclosed honestly: there is no portable atomic no-clobber
rename, so a destination created in the narrow window between the final
existence re-check and the rename could still be replaced — supply a fresh,
unique destination per invocation.

The single outcome document is written to `--result-file` as the same envelope
shape every `--json` command uses. A **success** (`ok:true`) means the action
ran to a defined conclusion — *not* that authentication necessarily succeeded;
read the `outcome` field for that:

```json
{
  "schema_version": 1,
  "ok": true,
  "contract": "account-action",
  "contract_version": 1,
  "action": "create",
  "account": "personal3",
  "account_existed_before": false,
  "account_created": true,
  "outcome": "login_completed",
  "authenticated": false,
  "requires_reconciliation": true,
  "identity_source": "local_cache",
  "child_exit_code": 0,
  "identity": { "uuid": "529d66f3-…", "email": "me@example.com" },
  "duplicate_of": null,
  "pinned": true
}
```

- `outcome` ∈ `authenticated` | `login_completed` | `login_completed_no_identity`
  | `login_failed` | `spawn_failed` (see the vocabulary above).
- `authenticated` is a convenience boolean, `true` **only** for
  `outcome == "authenticated"` — i.e. only when a live audit confirmed it.
- `requires_reconciliation` is `true` for every outcome except an
  authoritatively confirmed `authenticated` and a `spawn_failed` where nothing
  ran. When it is `true`, run `doctor`/`list` before trusting the account state.
- `identity_source` ∈ `live_audit` | `local_cache` | `none` — where the
  `identity` field came from. `local_cache` means it is **unverified**.
- `child_exit_code` is the raw login exit code (or `null` when the child could
  not be started) — the only cancellation-vs-failure signal upstream provides.
- `identity` is the reconciled **public** identity (from the live audit when
  `identity_source == "live_audit"`, otherwise the local cache), or `null`.
  Never a secret.
- `duplicate_of` names an existing account that shares this identity, as a
  non-blocking advisory — only ever populated from the single live audit (so it
  is `null` whenever authentication was not authoritatively confirmed). Distinct
  managed accounts are never collapsed.
- `pinned` reports whether the account now carries an identity pin. A pin is
  only ever *added* when absent; a reauth never moves an existing pin (that swap
  is the drift the pin exists to report).

An operational refusal is the standard `ok:false` error envelope, written to the
validated `--result-file`: `RESERVED_NAME`, `NAME_INVALID`, `NAME_UNSAFE` (a
reserved device name), `ACCOUNT_EXISTS` (create never replaces an existing
account), `ACCOUNT_NOT_FOUND` (reauth target absent), `PROFILE_NOT_CONTAINED`
(the target is not a real account directory inside the managed-account root — a
symlink/junction/reparse redirect), `SEED_NOT_SUPPORTED`, `NOT_INITIALIZED`,
`ACTION_IN_PROGRESS` (another lifecycle operation for this account is already
running), `ACTION_INVALID`, `IO_ERROR`.

Errors about the **invocation shape** (`JSON_REQUIRED`, `INVALID_ARGUMENTS`) and
about the **result channel itself** (`RESULT_FILE_NOT_ABSOLUTE`,
`RESULT_FILE_EXISTS`, `RESULT_DIR_NOT_FOUND`, `RESULT_DIR_NOT_A_DIRECTORY`,
`RESULT_FILE_FORBIDDEN_LOCATION`, `RESULT_FILE_NOT_WRITABLE`) are emitted on
**stdout**, because at that point there is no trustworthy result file to write
to. `RESULT_FILE_PUBLISH_FAILED` is also emitted on stdout and is special: the
action **already ran** and may have changed account state, so it is an
**uncertain** outcome — reconcile and do **not** automatically retry.

### What the action never does

- It **never replaces** an existing account on `create`, and **never rolls back**
  a created directory when authentication fails or is cancelled — the half-born
  account is reported (`account_created:true`, `authenticated:false`), not
  deleted.
- It **never** changes the configured default, **never** mutates a directory
  routing link, and **never** silently re-pins an identity.
- It **never follows a redirected profile**: the target account must be a real
  directory contained directly within the managed-account root (not a symlink,
  junction or reparse point), so a login is never redirected into another
  account or the standard Claude configuration.

### Concurrency and locking

- A **per-account lifecycle lock** is held across the whole action, including the
  login. It is the **same** lock the human `add`/`login`/`remove` take for that
  account, so a GUI-triggered action and a human lifecycle operation on the
  **same** account cannot overlap (the second is refused as
  `ACTION_IN_PROGRESS`, or waits, rather than corrupting state). Operations on
  **different** accounts never block each other. The lock key lives on the same
  volume as the account directories, so two case-variant spellings of one
  account on a case-insensitive filesystem share one lock.
- On **macOS**, a `claude auth login` snapshots/restores the standard account's
  Keychain entries as a side effect. A **global auth-side-effect lock**
  serializes that snapshot/restore bracket across **all** cooperating logins
  (including `login default`, which deliberately rewrites those entries), so two
  logins cannot interleave and leave the standard account holding the wrong
  token. It is a no-op off macOS and is held only briefly around the login
  bracket. Lock ordering is always per-account → auth-side-effect, and the
  routing lock is disjoint — no deadlock.
- The **global routing lock** is held **only** for the brief name reservation on
  `create`, **never** across the interactive login.
- These locks bind only **cooperating current binaries**. An older claude-acc, a
  direct hand-edit, the zsh port (which implements no file locking, exactly as it
  does not for the routing lock), or any non-cooperating process is **not**
  excluded — the same scope the routing lock documents.
- It transports **no secret**: no OAuth code, token, credential, cookie or
  authorization header ever appears in the result document, the capability
  document, or any claude-acc log.

## Versioning

`schema_version` is per-command and starts at `1`. A consumer should refuse
to operate against a `schema_version` lower than it requires ("claude-acc
needs updating") and refuse one higher than it understands ("claude-acc is
newer than this build expects") — never silently best-effort parse either
direction.

The `account-action` contract additionally carries its own `contract_version`
(also starting at `1`), bumped only when an existing field's meaning changes or
a field is removed — never for an additive field, the same policy the
`schema_version` numbers follow.
