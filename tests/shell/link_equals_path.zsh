#!/usr/bin/env zsh
# Shared links format: the final `=` separates path from account.

emulate -L zsh
set -u

typeset -i failures=0
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

export HOME="$scratch/home"
mkdir -p "$HOME"
source "${0:A:h:h:h}/claude-switch.sh" >/dev/null 2>&1

mkdir -p "$CLAUDE_SWITCH_ACCOUNTS_DIR/work"
linked="$scratch/project=a=b"
mkdir -p "$linked"

check() {
    local name="$1" want="$2" got="$3"
    if [[ "$got" == "$want" ]]; then
        print -r -- "ok   — $name"
    else
        print -r -- "FAIL — $name: want '$want', got '$got'"
        (( failures++ ))
    fi
}

(cd "$linked" && claude-acc link work >/dev/null)
check 'link stores the full equals path' "$linked=work" "$(<"$CLAUDE_SWITCH_LINKS")"
check 'resolver reads account after the final delimiter' 'work' "$(_claude_find_account "$linked")"

status_output=$(cd "$linked" && claude-acc status)
if [[ "$status_output" == *"work"* ]]; then
    print -r -- 'ok   — status resolves equals path'
else
    print -r -- "FAIL — status did not resolve equals path: $status_output"
    (( failures++ ))
fi

links=$(cd "$linked" && claude-acc links)
if [[ "$links" == *"$linked"* && "$links" == *"work"* ]]; then
    print -r -- 'ok   — links preserves display path'
else
    print -r -- "FAIL — links lost equals path: $links"
    (( failures++ ))
fi

(cd "$linked" && claude-acc unlink >/dev/null)
check 'unlink removes equals path' '' "$(<"$CLAUDE_SWITCH_LINKS")"

if (( failures )); then
    print -r -- "$failures test(s) failed"
    exit 1
fi
print -r -- 'all tests passed'
