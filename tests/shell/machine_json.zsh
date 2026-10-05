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

print -r -- "missing-delimiter" >> "$CLAUDE_SWITCH_LINKS"
out=$(claude-acc links --json)
check "malformed line reports failure" "false" "$(jqf '.ok' "$out")"
check "malformed line error code" "LINKS_STORE_INVALID" "$(jqf '.error.code' "$out")"
check "malformed line is named by number" "3" "$(jqf '.error.details.lines[0].line_number' "$out")"

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
if (( failures )); then
    print -r -- "$failures check(s) failed"
    exit 1
fi
print -r -- "all checks passed"
