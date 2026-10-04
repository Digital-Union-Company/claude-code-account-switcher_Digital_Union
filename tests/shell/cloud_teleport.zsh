#!/usr/bin/env zsh
#
# Dedicated cloud and teleport commands must keep their argv contracts while
# reusing the standalone launcher's account isolation. No real Claude process
# or network operation is involved.

emulate -L zsh
set -u

typeset -i failures=0
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

export HOME="$scratch/home"
fake_bin="$scratch/fake-bin"
mkdir -p "$HOME" "$fake_bin"

cat > "$fake_bin/claude" <<'FAKE'
#!/bin/sh
{
    printf 'config=%s\n' "${CLAUDE_CONFIG_DIR-unset}"
    printf 'anthropic=%s\n' "${ANTHROPIC_CONFIG_DIR-unset}"
    printf 'auth_present=%s\n' "${ANTHROPIC_API_KEY+yes}"
    for arg in "$@"; do
        printf 'arg=%s\n' "$arg"
    done
} > "$CLAUDE_ACC_SHELL_CAPTURE"
exit "${CLAUDE_ACC_SHELL_EXIT_CODE:-0}"
FAKE
chmod +x "$fake_bin/claude"
export PATH="$fake_bin:$PATH"

script="${0:A:h:h:h}/claude-switch.sh"
source "$script" >/dev/null 2>&1
mkdir -p "$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1"

capture="$scratch/capture"
export CLAUDE_ACC_SHELL_CAPTURE="$capture"
export CLAUDE_ACC_SHELL_EXIT_CODE=0
export ANTHROPIC_CONFIG_DIR="$scratch/global anthropic"
export ANTHROPIC_API_KEY='dummy-presence-only'

run_manager() {
    rm -f "$capture"
    claude-acc "$@" >/dev/null 2>&1
}

assert_capture() {
    local label="$1"
    shift
    local got="$(<"$capture")"
    local want="$(printf '%s\n' "$@")"
    if [[ "$got" == "$want" ]]; then
        print -r -- "ok   — $label"
    else
        print -r -- "FAIL — $label"
        print -r -- "want:\n$want"
        print -r -- "got:\n$got"
        (( failures++ ))
    fi
}

task='Continue Δ with "quotes", 50% & exact spacing'
run_manager cloud personal1 "$task"
assert_capture 'cloud named argv and profile isolation' \
    "config=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1" \
    "anthropic=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1/.anthropic" \
    'auth_present=' \
    'arg=--cloud' \
    "arg=$task" \
    'arg=--permission-mode' \
    'arg=auto'

run_manager cloud default 'Default cloud task'
assert_capture 'cloud default preserves upstream profile semantics' \
    'config=unset' \
    "anthropic=$ANTHROPIC_CONFIG_DIR" \
    'auth_present=' \
    'arg=--cloud' \
    'arg=Default cloud task' \
    'arg=--permission-mode' \
    'arg=auto'

run_manager teleport personal1 'session_opaque'
assert_capture 'teleport named argv and profile isolation' \
    "config=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1" \
    "anthropic=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1/.anthropic" \
    'auth_present=' \
    'arg=--teleport' \
    'arg=session_opaque'

run_manager teleport default 'cse_opaque'
assert_capture 'teleport default preserves upstream profile semantics' \
    'config=unset' \
    "anthropic=$ANTHROPIC_CONFIG_DIR" \
    'auth_present=' \
    'arg=--teleport' \
    'arg=cse_opaque'

export CLAUDE_ACC_SHELL_EXIT_CODE=23
for task_description in \
    session_012345 \
    cse_012345 \
    'https://claude.ai/code/session_012345' \
    'claude.ai/code/cse_012345?from=cli'; do
    run_manager cloud personal1 "$task_description"
    child_status=$?
    if (( child_status != 23 )); then
        print -r -- "FAIL — cloud locator-like task exit: $task_description (got $child_status)"
        (( failures++ ))
    elif [[ ! -e "$capture" ]]; then
        print -r -- "FAIL — cloud locator-like task did not launch Claude: $task_description"
        (( failures++ ))
    else
        assert_capture "cloud locator-like task forwarded: $task_description" \
            "config=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1" \
            "anthropic=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1/.anthropic" \
            'auth_present=' \
            'arg=--cloud' \
            "arg=$task_description" \
            'arg=--permission-mode' \
            'arg=auto'
    fi
done
export CLAUDE_ACC_SHELL_EXIT_CODE=0

if run_manager cloud personal1 task --dangerously-skip-permissions; then
    print -r -- 'FAIL — cloud bypass argument accepted'
    (( failures++ ))
elif [[ -e "$capture" ]]; then
    print -r -- 'FAIL — cloud bypass argument launched Claude'
    (( failures++ ))
else
    print -r -- 'ok   — cloud bypass argument rejected before launch'
fi

run_manager cloud personal1 'Investigate session_012345 handling'
assert_capture 'cloud prose containing a locator remains creation mode' \
    "config=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1" \
    "anthropic=$CLAUDE_SWITCH_ACCOUNTS_DIR/personal1/.anthropic" \
    'auth_present=' \
    'arg=--cloud' \
    'arg=Investigate session_012345 handling' \
    'arg=--permission-mode' \
    'arg=auto'

if (( failures )); then
    print -r -- "$failures test(s) failed"
    exit 1
fi
print -r -- 'all cloud/teleport tests passed'
