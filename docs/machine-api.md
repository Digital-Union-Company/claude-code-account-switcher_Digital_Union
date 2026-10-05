# Machine API: `--json` for scripts and GUIs

[← README](../README.md) · [Русский](ru/machine-api.md)

Five commands — `list`, `doctor`, `status`, `links`, `usage` — have a `--json`
mode meant for a script or another program to parse, not a human to read.
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

## Versioning

`schema_version` is per-command and starts at `1`. A consumer should refuse
to operate against a `schema_version` lower than it requires ("claude-acc
needs updating") and refuse one higher than it understands ("claude-acc is
newer than this build expects") — never silently best-effort parse either
direction.
