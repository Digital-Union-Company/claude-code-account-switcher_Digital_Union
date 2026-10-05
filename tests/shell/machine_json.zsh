#!/usr/bin/env zsh
#
# The CM0.5 machine API: `list`/`status`/`links`/`usage`/`doctor --json`.
#
# These are offline, deterministic checks against fakes/fixtures this test
# writes itself — no real account, no network. Assertions go through `jq`,
# never string matching on the raw JSON text, so a harmless key-ordering or
# whitespace change in a future `jq -n` tweak can't fail this for the wrong
# reason.
#
# Run: zsh tests/shell/machine_json.zsh

emulate -L zsh
set -u

typeset -i failures=0
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

export HOME="$scratch/home"
mkdir -p "$HOME"
export CLAUDE_ACC_LANG=en

source "${0:A:h:h:h}/claude-switch.sh" >/dev/null 2>&1

check() {
    local label="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        print -r -- "  ok   $label"
    else
        print -r -- "  FAIL $label: expected '$expected', got '$actual'"
        (( failures++ ))
    fi
}

jqf() {
    # jqf <filter> <json> — $(jq -r '<filter>' <<< "$json"), as one call site
    # so every check below reads as "field, json" instead of a repeated
    # here-string.
    jq -r "$1" <<< "$2"
}

print -r -- "claude-acc list --json:"

out=$(claude-acc list --json)
check "schema_version is 1" "1" "$(jqf '.schema_version' "$out")"
check "ok is true" "true" "$(jqf '.ok' "$out")"
check "no accounts yet" "0" "$(jqf '.accounts | length' "$out")"

mkdir -p "$CLAUDE_SWITCH_ACCOUNTS_DIR/work"
print -r -- '{"claudeAiOauth": {"accessToken": "fake-token"}}' \
    > "$CLAUDE_SWITCH_ACCOUNTS_DIR/work/.credentials.json"
print -r -- '{"claudeInChromeDefaultEnabled": true}' \
    > "$CLAUDE_SWITCH_ACCOUNTS_DIR/work/.claude.json"

out=$(claude-acc list --json)
entry=$(jq '.accounts[] | select(.name == "work")' <<< "$out")
check "managed is true" "true" "$(jqf '.managed' "$entry")"
check "auth_present true from the plaintext token file" "true" "$(jqf '.auth_present' "$entry")"
check "chrome_enabled true" "true" "$(jqf '.chrome_enabled' "$entry")"
check "no cache yet means cached_email is null" "null" "$(jqf '.cached_email' "$entry")"
check "no token/credential field leaks" "false" \
    "$(jq 'has("token") or has("token_hash") or has("credential")' <<< "$entry")"

print -r -- ""
print -r -- "claude-acc links --json:"

out=$(claude-acc links --json)
check "empty store ok" "true" "$(jqf '.ok' "$out")"
check "empty store has no links" "0" "$(jqf '.links | length' "$out")"

proj="$scratch/proj"
print -r -- "$proj=work" > "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "stored_path preserved verbatim" "$proj" "$(jqf '.links[0].stored_path' "$out")"
check "account preserved" "work" "$(jqf '.links[0].account' "$out")"
check "no conflicts for one link" "0" "$(jqf '.conflicts | length' "$out")"

print -r -- "$proj=personal1" >> "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "identical stored_path, different accounts conflicts" "1" "$(jqf '.conflicts | length' "$out")"
check "conflict code" "AMBIGUOUS_EQUIVALENT_PATHS" "$(jqf '.conflicts[0].code' "$out")"
check "conflict keeps every member" "2" "$(jqf '.conflicts[0].mappings | length' "$out")"

# A repeated stored entry for an account already in the group must not be
# collapsed away — every member stays, same as status --json's mappings.
print -r -- "$proj=work" >> "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "a repeated same-account entry is kept, not collapsed" "3" "$(jqf '.conflicts[0].mappings | length' "$out")"

print -r -- "missing-delimiter" >> "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "malformed line reports failure" "false" "$(jqf '.ok' "$out")"
check "malformed line error code" "LINKS_STORE_INVALID" "$(jqf '.error.code' "$out")"
check "malformed line is named by number" "4" "$(jqf '.error.details.lines[0].line_number' "$out")"

print -r -- ""
print -r -- "claude-acc status --json [--path]:"

: > "$CLAUDE_SWITCH_LINKS"
project="$scratch/project"
mkdir -p "$project" "$CLAUDE_SWITCH_ACCOUNTS_DIR/personal2"
(cd "$project" && claude-acc link personal2 >/dev/null)

out=$(claude-acc status --json --path "$project")
check "linked account resolves" "personal2" "$(jqf '.resolved_account' "$out")"
check "linked source" "linked" "$(jqf '.source' "$out")"
check "owning_link_path is the stored path" "$project" "$(jqf '.owning_link_path' "$out")"

unlinked="$scratch/unlinked"
mkdir -p "$unlinked"
out=$(claude-acc status --json --path "$unlinked")
check "unlinked, no default falls back to standard" "default" "$(jqf '.resolved_account' "$out")"
check "standard source" "standard" "$(jqf '.source' "$out")"
check "owning_link_path is null when unlinked" "null" "$(jqf '.owning_link_path' "$out")"

out=$(claude-acc status --json --path "$scratch/does-not-exist")
check "missing --path reports PATH_NOT_FOUND" "PATH_NOT_FOUND" "$(jqf '.error.code' "$out")"

afile="$scratch/a-file"
: > "$afile"
out=$(claude-acc status --json --path "$afile")
check "a file, not a directory, reports PATH_NOT_DIRECTORY" "PATH_NOT_DIRECTORY" "$(jqf '.error.code' "$out")"

print -r -- ""
print -r -- "claude-acc doctor --json:"

mkdir -p "$CLAUDE_SWITCH_ACCOUNTS_DIR/freshacct"
out=$(claude-acc doctor --json)
check "schema_version is 1" "1" "$(jqf '.schema_version' "$out")"
check "ok is true even with a no_token row" "true" "$(jqf '.ok' "$out")"
entry=$(jq '.accounts[] | select(.name == "freshacct")' <<< "$out")
check "no_token status" "no_token" "$(jqf '.status' "$entry")"
check "a fresh account has no pin yet" "none" "$(jqf '.lock' "$entry")"
check "no pin means pinned_uuid is null" "null" "$(jqf '.pinned_uuid' "$entry")"
check "config_dir points at the account directory" \
    "$CLAUDE_SWITCH_ACCOUNTS_DIR/freshacct" "$(jqf '.config_dir' "$entry")"

print -r -- ""
print -r -- "claude-acc usage --json:"

out=$(claude-acc usage --json)
entry=$(jq '.accounts[] | select(.name == "freshacct")' <<< "$out")
check "no token means unavailable" "unavailable" "$(jqf '.source' "$entry")"
check "unavailable has no fetched_at" "null" "$(jqf '.fetched_at' "$entry")"
check "unavailable has no windows" "null" "$(jqf '.five_hour' "$entry")"
check "usage --json never carries a plan field" "false" "$(jq 'has("plan")' <<< "$entry")"

print -r -- ""
print -r -- "usage --json no-token/cache parity (independent-review correction):"

# Regression: a config dir with no token but a real, identity-matched
# cachedUsageUtilization used to report source:"cache" in standalone zsh —
# disagreeing with identity::fetch_account_usage_detail, which returns
# Unavailable immediately on no token and never even looks at the cache.
# No live request means no token to try, which means nothing earned the
# right to fall back to the cache either.
notoken_dir="$CLAUDE_SWITCH_ACCOUNTS_DIR/notoken"
mkdir -p "$notoken_dir"
cat > "$notoken_dir/.claude.json" <<JSON
{
  "oauthAccount": {"accountUuid": "u-1"},
  "cachedUsageUtilization": {
    "accountUuid": "u-1",
    "fetchedAtMs": $(( $(date +%s) * 1000 )),
    "utilization": {
      "five_hour": {"utilization": 50, "resets_at": "2099-01-01T00:00:00Z"},
      "seven_day": {"utilization": 20, "resets_at": "2099-01-01T00:00:00Z"}
    }
  }
}
JSON
# Deliberately no .credentials.json and no keychain entry for "notoken" —
# _claude_acc_token must find nothing here.
out=$(claude-acc usage --json)
entry=$(jq '.accounts[] | select(.name == "notoken")' <<< "$out")
check "no token is unavailable even with a matching cache on disk" \
    "unavailable" "$(jqf '.source' "$entry")"
check "no token means fetched_at is null despite the cache" "null" "$(jqf '.fetched_at' "$entry")"
check "no token means five_hour is null despite the cache" "null" "$(jqf '.five_hour' "$entry")"

print -r -- ""
print -r -- "status --json ambiguity preserves every stored mapping (independent-review correction):"

# Regression: the ambiguity branch used to de-duplicate by account name
# before serializing `mappings`, which could silently drop a stored entry.
# Three stored lines, two distinct accounts, must still yield three
# mappings in stored order — matching LinkResolveError::Ambiguous, which
# never filters its `matches` list.
: > "$CLAUDE_SWITCH_LINKS"
dup_dir="$scratch/dup"
mkdir -p "$dup_dir" "$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1" "$CLAUDE_SWITCH_ACCOUNTS_DIR/personal2"
print -r -- "$dup_dir=personal1" >> "$CLAUDE_SWITCH_LINKS"
print -r -- "$dup_dir=personal1" >> "$CLAUDE_SWITCH_LINKS"
print -r -- "$dup_dir=personal2" >> "$CLAUDE_SWITCH_LINKS"

out=$(claude-acc status --json --path "$dup_dir")
check "three stored entries, two accounts, is ambiguous" "false" "$(jqf '.ok' "$out")"
check "ambiguity error code" "AMBIGUOUS_LINK" "$(jqf '.error.code' "$out")"
check "every stored mapping is preserved, including the repeat" "3" \
    "$(jqf '.error.details.mappings | length' "$out")"
check "mapping order is preserved — first personal1" \
    "personal1" "$(jqf '.error.details.mappings[0].account' "$out")"
check "mapping order is preserved — second personal1 (the repeat)" \
    "personal1" "$(jqf '.error.details.mappings[1].account' "$out")"
check "mapping order is preserved — third personal2" \
    "personal2" "$(jqf '.error.details.mappings[2].account' "$out")"

print -r -- ""
print -r -- "status --json configured-default source parity (independent-review correction):"

# Regression: a configured managed default whose account directory had
# gone stale used to collapse into source:"standard" — indistinguishable
# from no default ever having been configured at all. Rust's
# effective_route reports source:"default" for both "configured and
# usable" and "configured but stale", and only "standard" for "no
# configured default at all" — three states, not two.
: > "$CLAUDE_SWITCH_LINKS"
default_probe_dir="$scratch/default-probe"
mkdir -p "$default_probe_dir" "$CLAUDE_SWITCH_ACCOUNTS_DIR/ghost"

print -r -- "default=ghost" > "$CLAUDE_SWITCH_CONFIG"
out=$(claude-acc status --json --path "$default_probe_dir")
check "existing managed default resolves to that account" \
    "ghost" "$(jqf '.resolved_account' "$out")"
check "existing managed default source is \"default\"" \
    "default" "$(jqf '.source' "$out")"
check "existing managed default has no owning_link_path" \
    "null" "$(jqf '.owning_link_path' "$out")"

# Stale it: remove the managed account directory, leave the config
# pointing at it. The actual shim still falls back to standard — this is
# reporting parity, not a request to launch the stale account — but the
# configured default was still the routing layer that decided this.
rm -rf "$CLAUDE_SWITCH_ACCOUNTS_DIR/ghost"
out=$(claude-acc status --json --path "$default_probe_dir")
check "stale managed default effectively falls back to standard" \
    "default" "$(jqf '.resolved_account' "$out")"
check "stale managed default source is still \"default\", not \"standard\"" \
    "default" "$(jqf '.source' "$out")"
check "stale managed default has no owning_link_path" \
    "null" "$(jqf '.owning_link_path' "$out")"

# Clear the configured default entirely: now no routing layer decided
# anything, and only this state may report source:"standard".
print -r -- "default=" > "$CLAUDE_SWITCH_CONFIG"
out=$(claude-acc status --json --path "$default_probe_dir")
check "no configured default at all resolves to standard" \
    "default" "$(jqf '.resolved_account' "$out")"
check "no configured default at all source is \"standard\"" \
    "standard" "$(jqf '.source' "$out")"

print -r -- ""
print -r -- "machine link parser parity (independent-review correction):"

# A. Stored path containing its own "=" — must split on the FINAL "="
# only, same as Rust's rsplit_once('=').
: > "$CLAUDE_SWITCH_LINKS"
eq_path="$scratch/repo=a"
mkdir -p "$eq_path" "$CLAUDE_SWITCH_ACCOUNTS_DIR/personal2"
print -r -- "${eq_path}=personal2" > "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "a stored path containing its own = splits on the final = only" \
    "$eq_path" "$(jqf '.links[0].stored_path' "$out")"
check "the account after that final = is exactly personal2" \
    "personal2" "$(jqf '.links[0].account' "$out")"

# B. Delimiter-adjacent whitespace is trimmed; status --json's exact-path
# lookup must agree with what links --json reports for the same line.
padded_dir="$scratch/padded"
mkdir -p "$padded_dir"
print -r -- "  ${padded_dir}  =  personal2  " > "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "delimiter-adjacent whitespace is trimmed from the stored path" \
    "$padded_dir" "$(jqf '.links[0].stored_path' "$out")"
check "delimiter-adjacent whitespace is trimmed from the account" \
    "personal2" "$(jqf '.links[0].account' "$out")"

status_out=$(claude-acc status --json --path "$padded_dir")
check "status --json resolves the same trimmed path links --json reports" \
    "personal2" "$(jqf '.resolved_account' "$status_out")"
check "status --json reports it as linked" \
    "linked" "$(jqf '.source' "$status_out")"
check "status --json's owning_link_path is the trimmed path, not the padded raw line" \
    "$padded_dir" "$(jqf '.owning_link_path' "$status_out")"

# C. Malformed even after trimming: an empty directory or empty account.
print -r -- "   = personal2" > "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "whitespace-only directory before = is LINKS_STORE_INVALID" \
    "LINKS_STORE_INVALID" "$(jqf '.error.code' "$out")"

print -r -- "${padded_dir} =    " > "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "whitespace-only account after = is LINKS_STORE_INVALID" \
    "LINKS_STORE_INVALID" "$(jqf '.error.code' "$out")"

print -r -- ""
print -r -- "machine-mode dependency failures (independent-review correction):"

jq_bin=$(command -v jq)
if [[ -z "$jq_bin" ]]; then
    print -r -- "  SKIP — jq itself is not installed here, cannot simulate its absence"
else
    jq_dir="${jq_bin:h}"
    typeset -a saved_path
    saved_path=("${path[@]}")
    path=("${(@)path:#$jq_dir}")

    if command -v jq >/dev/null 2>&1; then
        print -r -- "  SKIP — another jq is still reachable after hiding $jq_dir, cannot isolate the dependency"
        path=("${saved_path[@]}")
    else
        raw_out=$(claude-acc list --json)
        raw_exit=$?
        doctor_out=$(claude-acc doctor --json)
        doctor_exit=$?
        # Restore PATH before using the *real* jq to parse what was
        # captured — the assertions below need a working jq even though
        # the commands under test did not have one.
        path=("${saved_path[@]}")

        check "list --json with no jq exits nonzero" "1" "$raw_exit"
        check "list --json with no jq still parses as JSON" "false" "$(jqf '.ok' "$raw_out")"
        check "list --json with no jq reports schema_version 1" "1" "$(jqf '.schema_version' "$raw_out")"
        check "list --json with no jq reports DEPENDENCY_MISSING" \
            "DEPENDENCY_MISSING" "$(jqf '.error.code' "$raw_out")"
        check "dependency detail names jq" "jq" "$(jqf '.error.details.dependency' "$raw_out")"
        if [[ "$raw_out" == *"needs"* || "$raw_out" == *"требует"* ]]; then
            print -r -- "  FAIL no human prose leaked into list --json's stdout: $raw_out"
            (( failures++ ))
        else
            print -r -- "  ok   no human prose leaked into list --json's stdout"
        fi

        # doctor --json has its own, pre-existing localized dependency
        # message (doctor_missing_dep) for the human path — this proves
        # --json never reaches it.
        check "doctor --json with no jq exits nonzero" "1" "$doctor_exit"
        check "doctor --json with no jq reports DEPENDENCY_MISSING" \
            "DEPENDENCY_MISSING" "$(jqf '.error.code' "$doctor_out")"
        if [[ "$doctor_out" == *"doctor needs"* || "$doctor_out" == *"doctor требует"* ]]; then
            print -r -- "  FAIL doctor --json leaked the human doctor_missing_dep message: $doctor_out"
            (( failures++ ))
        else
            print -r -- "  ok   doctor --json did not leak the human doctor_missing_dep message"
        fi
    fi
fi

print -r -- ""
if (( failures )); then
    print -r -- "$failures check(s) failed"
    exit 1
fi
print -r -- "all checks passed"
