#!/usr/bin/env zsh
#
# The standalone updater must stay on the Digital Union distribution channel.
# A fake curl records the requested URL and writes a harmless replacement into
# scratch state; no network request or real installation is involved.

emulate -L zsh
set -u

typeset -i failures=0
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

export HOME="$scratch/home"
fake_bin="$scratch/fake-bin"
installed_dir="$scratch/installed"
mkdir -p "$HOME" "$fake_bin" "$installed_dir"

cat > "$fake_bin/curl" <<'FAKE'
#!/bin/sh
: > "$CLAUDE_ACC_CURL_CAPTURE"
output=
while [ "$#" -gt 0 ]; do
    printf '%s\n' "$1" >> "$CLAUDE_ACC_CURL_CAPTURE"
    if [ "$1" = "-o" ]; then
        shift
        [ "$#" -gt 0 ] || exit 2
        output=$1
        printf '%s\n' "$1" >> "$CLAUDE_ACC_CURL_CAPTURE"
    fi
    shift
done
[ -n "$output" ] || exit 2
printf '%s\n' '#!/usr/bin/env zsh' '# Claude Code Account Switcher' '# fake update payload' > "$output"
FAKE
chmod +x "$fake_bin/curl"
export PATH="$fake_bin:$PATH"

repo_script="${0:A:h:h:h}/claude-switch.sh"
source "$repo_script" >/dev/null 2>&1

export CLAUDE_ACC_CURL_CAPTURE="$scratch/curl-args"
CLAUDE_SWITCH_SCRIPT="$installed_dir/claude-switch.sh"
cp "$repo_script" "$CLAUDE_SWITCH_SCRIPT"

expected_url='https://raw.githubusercontent.com/Digital-Union-Company/claude-code-account-switcher_Digital_Union/master/claude-switch.sh'

if claude-acc update >/dev/null 2>&1; then
    print -r -- 'ok   — standalone update completed through fake curl'
else
    print -r -- 'FAIL — standalone update failed through fake curl'
    (( failures++ ))
fi

if [[ $(grep -Fxc -- "$expected_url" "$CLAUDE_ACC_CURL_CAPTURE") == 1 ]]; then
    print -r -- 'ok   — exact Digital Union update URL requested'
else
    print -r -- 'FAIL — exact Digital Union update URL was not requested once'
    (( failures++ ))
fi

if grep -Fq -- 'Nemo-Illusionist/claude-code-account-switcher' "$CLAUDE_ACC_CURL_CAPTURE"; then
    print -r -- 'FAIL — runtime updater requested the Nemo source'
    (( failures++ ))
else
    print -r -- 'ok   — runtime updater requested no Nemo URL'
fi

if grep -Fq -- '# fake update payload' "$CLAUDE_SWITCH_SCRIPT"; then
    print -r -- 'ok   — fake payload replaced only the scratch installation'
else
    print -r -- 'FAIL — fake payload did not replace the scratch installation'
    (( failures++ ))
fi

if (( failures )); then
    print -r -- "$failures test(s) failed"
    exit 1
fi
print -r -- 'all standalone update-source tests passed'
