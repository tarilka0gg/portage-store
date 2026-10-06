#!/usr/bin/env bash
# Installed as /usr/libexec/portage-store/priv-helper, root:root 0755,
# not writable by the app's own user. Invoked via a passwordless doas rule
# (see /etc/doas.conf), so unlike the pkexec calls this replaces, there is
# no authentication step at all standing between "the Rust binary asked for
# this" and "it happened as root" — every subcommand below validates its
# own arguments defensively instead of trusting the caller, since that gap
# is exactly what a nopass rule removes.
#
# Nothing here is passed *as script text* through the privileged call (the
# way the old pkexec-based code passed `bash -c "$SCRIPT"`) — the script
# itself lives here, root-owned, referenced only by subcommand name. That
# closes off the whole class of bug where attacker/app-controlled bytes
# could reach a `-c` argument and get executed as root with zero friction.
set -euo pipefail

HELPER_DIR="/usr/libexec/portage-store"
TRACKED_DIR="/etc/portage"

fail() {
    echo "priv-helper: $*" >&2
    exit 1
}

# Every invocation gets exactly one line here, success or failure — the
# actual compensating control for a passwordless doas rule: there's no
# auth prompt to stand in the way of a bug or a compromised app binary,
# so this is root-appended (not by the app's own unprivileged process)
# and never silently trimmed. `LOGGED_ARGV` is captured before any
# subcommand shifts `$@` around; safe to log verbatim (every caller pipes
# file *content* via stdin, never argv — see the module-level comment
# above) and truncated only as a defensive cap, not because anything
# sensitive is expected here.
#
# The directory and file are created once, manually, as part of the same
# root-privileged setup that installs this script itself:
#   mkdir -p /var/log/portage-store && chown root:portage /var/log/portage-store && chmod 0750 /var/log/portage-store
#   : > /var/log/portage-store/priv-helper.log && chown root:portage /var/log/portage-store/priv-helper.log && chmod 0640 /var/log/portage-store/priv-helper.log
# `0640 root:portage` (not world-readable) matches this system's own
# `/var/log/emerge.log` convention rather than `/etc/portage`'s
# world-readable one — a log of every privileged action is closer to an
# operational/security record than to declarative config. If this file
# is ever deleted, the `>>` below would silently recreate it as
# `root:root` under the shell's default umask instead — worth reapplying
# the chown/chmod above if that ever happens.
LOG_FILE="/var/log/portage-store/priv-helper.log"
LOGGED_ARGV="$*"
log_line() {
    printf '%s uid=%s cmd=%s argv=%s exit=%s\n' \
        "$(date -u +%FT%TZ)" "${DOAS_USER:-?}" "${cmd:-?}" "${LOGGED_ARGV:0:500}" "$1" \
        >> "$LOG_FILE" 2>/dev/null || true
}
trap 'log_line "$?"' EXIT

# Requires $1 to be an absolute path, and its *parent* directory
# (canonicalized — the target itself may not exist yet, e.g. a brand new
# file) to be $2 or a descendant of it. Canonicalizing the parent before
# comparing (not the raw string) is what actually closes a symlink-swap
# TOCTOU: a symlink planted at some component of the path resolves to its
# real target before the comparison ever happens.
#
# No separate NUL-byte check: a NUL can never actually reach here in the
# first place — argv strings passed through execve() are C strings, so
# the kernel itself won't let one contain an embedded NUL. An earlier
# version of this function tried to check for one anyway using bash's
# `$'\0'` pattern, which — since bash strings are equally NUL-terminated
# internally — silently evaluates to the *empty* string, making the
# pattern `**` and matching every path unconditionally. Caught by testing
# this function's rejection path directly against a real, ordinary path
# and finding it rejected too.
require_under() {
    local target="$1" allowed_root="$2" parent
    case "$target" in
        /*) ;;
        *) fail "path must be absolute: $target" ;;
    esac
    parent=$(dirname -- "$target")
    parent=$(realpath -m -- "$parent") || fail "could not resolve parent of $target"
    case "$parent" in
        "$allowed_root" | "$allowed_root"/*) ;;
        *) fail "$target is not under $allowed_root" ;;
    esac
}

cmd_write_tracked() {
    local target="$1" message="$2"
    require_under "$target" "$TRACKED_DIR"
    if [ ! -d "$TRACKED_DIR/.git" ]; then
        git -C "$TRACKED_DIR" init -q
        git -C "$TRACKED_DIR" config user.name "Portage Store"
        git -C "$TRACKED_DIR" config user.email "portage-store@localhost"
        git -C "$TRACKED_DIR" add -A
        git -C "$TRACKED_DIR" commit -q -m "Baseline snapshot" --allow-empty
    fi
    cat > "$target"
    git -C "$TRACKED_DIR" add -A
    if ! git -C "$TRACKED_DIR" diff --cached --quiet; then
        git -C "$TRACKED_DIR" commit -q -m "$message"
    fi
}

# Both paths only need to be under /etc in general — CONFIG_PROTECT roots
# aren't limited to /etc/portage — but the discard path is additionally
# required to actually look like a portage-generated update file, not just
# "somewhere under /etc", since that's the one extra invariant we know
# must hold for a legitimate call.
require_cfg_shape() {
    case "$(basename -- "$1")" in
        ._cfg[0-9][0-9][0-9][0-9]_*) ;;
        *) fail "$1 does not look like a CONFIG_PROTECT update file" ;;
    esac
}

cmd_write_then_remove() {
    local live="$1" discard="$2"
    require_under "$live" "/etc"
    require_under "$discard" "/etc"
    require_cfg_shape "$discard"
    cat > "$live"
    rm -f -- "$discard"
}

cmd_write_then_remove_many() {
    [ "${1:-}" = "--" ] || fail "expected -- before the path pairs"
    shift
    while [ "$#" -ge 2 ]; do
        local live="$1" discard="$2"
        shift 2
        require_under "$live" "/etc"
        require_under "$discard" "/etc"
        require_cfg_shape "$discard"
        cp -- "$discard" "$live"
        rm -f -- "$discard"
    done
}

cmd_remove_file() {
    local target="$1"
    require_under "$target" "/etc"
    rm -f -- "$target"
}

cmd_mkdir_p() {
    local dir="$1"
    require_under "$dir" "$TRACKED_DIR"
    mkdir -p -- "$dir"
}

cmd_git_revert() {
    local target="${1:-HEAD}"
    case "$target" in
        HEAD | [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*) ;;
        *) fail "not a plausible commit-ish: $target" ;;
    esac
    git -C "$TRACKED_DIR" revert --no-edit "$target"
}

# Tags HEAD (force-moving the tag if it already exists, matching `git tag
# -f`'s normal semantics) — used to mark a known-good point right before
# a big operation like an `@world` update, so the history browser can
# find it again later. The same conservative name-shape validation
# `cmd_sandbox_build`/`cmd_enable_overlay` already use for their own
# synthetic identifiers, closing off anything that could be misread as a
# git option by starting with `-`.
cmd_git_tag() {
    local name="$1"
    case "$name" in
        # First character deliberately excludes `-`/`.` (unlike the rest
        # of the name) — a leading `-` is exactly what could get misread
        # as a git option, and `--` before `$name` in the actual `git
        # tag` call below is the real protection, but the validation
        # should say what it means rather than accidentally allowing the
        # one shape it claims to reject.
        [a-zA-Z0-9_][a-zA-Z0-9_.-]*) ;;
        *) fail "not a plausible tag name: $name" ;;
    esac
    git -C "$TRACKED_DIR" tag -f -- "$name"
}

# The source directory is this app's own extracted-bundle temp dir — not
# further ownership-checked here (a known, accepted simplification; see
# profile_bundle.rs, which now stages under $XDG_RUNTIME_DIR instead of
# the shared, world-writable /tmp specifically to narrow this) — but must
# genuinely exist as a directory before anything is copied from it.
cmd_cp_bundle() {
    local source="$1"
    case "$source" in
        /*) ;;
        *) fail "source must be absolute: $source" ;;
    esac
    [ -d "$source" ] || fail "source is not a directory: $source"
    cp -a -- "$source/." "$TRACKED_DIR/"
}

cmd_cat_log() {
    local target="$1"
    require_under "$target" "/var/tmp/portage"
    cat -- "$target"
}

# Binary allowlist: emerge, eselect, and the two gentoolkit cache cleaners.
# --jobs, if given, is only honored for emerge and is a validated integer
# used to build the throttle wrapper *here*, from scratch — never assembled
# from argv tokens the Rust side supplied, so there's no way for a caller
# to smuggle extra ionice/nice/env arguments through this path. Trailing
# argv past the binary is otherwise passed through unrestricted, same as
# it always was for emerge — this allowlist controls *which program* runs
# as root, not what flags it's given.
cmd_run() {
    local jobs=""
    if [ "${1:-}" = "--jobs" ]; then
        jobs="$2"
        shift 2
        case "$jobs" in
            '' | *[!0-9]*) fail "invalid --jobs value: $jobs" ;;
        esac
    fi
    [ "${1:-}" = "--" ] || fail "expected -- before the binary"
    shift
    local binary="$1"
    shift
    case "$binary" in
        emerge)
            # Not `exec`'d: an `exec`'d child replaces the shell's own
            # process image, which means the `trap ... EXIT` above set up
            # to log this invocation's outcome would never fire for it —
            # running it as a normal foreground command instead costs one
            # extra process (the parent shell surviving to report exit
            # status) but keeps the audit log correct for every
            # subcommand uniformly, not just the ones that don't exec.
            if [ -n "$jobs" ]; then
                ionice -c 3 nice -n 19 env "MAKEOPTS=-j$jobs -l$jobs" /usr/bin/emerge "$@"
            else
                /usr/bin/emerge "$@"
            fi
            ;;
        eselect|eclean-dist|eclean-pkg)
            "$binary" "$@"
            ;;
        *)
            fail "binary not permitted: $binary"
            ;;
    esac
}

# A synthetic "binary" name, like sandbox-build below: takes a repo/overlay
# name to validate instead of arbitrary eselect/emerge argv, and folds
# "enable" + "sync just this repo" into one privileged call instead of
# passing `bash -c "<script text>"` through the privileged call the way
# the old pkexec-based code did.
cmd_enable_overlay() {
    local name="$1"
    case "$name" in
        [a-zA-Z0-9_+-]*) ;;
        *) fail "not a plausible overlay name: $name" ;;
    esac
    eselect repository enable -- "$name"
    # Not `exec`'d — see `cmd_run`'s comment on why every subcommand here
    # runs as a normal foreground command rather than replacing the shell.
    emerge --sync --repo "$name"
}

# A synthetic "binary" name, not a real executable — dispatched separately
# from cmd_run's allowlist rather than added to it, since this one takes
# an atom to validate instead of arbitrary emerge argv.
cmd_sandbox_build() {
    local atom="$1"
    case "$atom" in
        [a-zA-Z0-9_+-]*/[a-zA-Z0-9_+.-]*) ;;
        *) fail "not a plausible atom: $atom" ;;
    esac
    # Not `exec`'d — see `cmd_run`'s comment on why every subcommand here
    # runs as a normal foreground command rather than replacing the shell.
    "$HELPER_DIR/sandbox-build.sh" "$atom"
}

cmd="${1:-}"
[ -n "$cmd" ] || fail "no subcommand given"
shift

case "$cmd" in
    --self-check) echo "priv-helper: ok" ;;
    write-tracked) cmd_write_tracked "$@" ;;
    write-then-remove) cmd_write_then_remove "$@" ;;
    write-then-remove-many) cmd_write_then_remove_many "$@" ;;
    remove-file) cmd_remove_file "$@" ;;
    mkdir-p) cmd_mkdir_p "$@" ;;
    git-revert) cmd_git_revert "$@" ;;
    git-tag) cmd_git_tag "$@" ;;
    cp-bundle) cmd_cp_bundle "$@" ;;
    cat-log) cmd_cat_log "$@" ;;
    run) cmd_run "$@" ;;
    sandbox-build) cmd_sandbox_build "$@" ;;
    enable-overlay) cmd_enable_overlay "$@" ;;
    *) fail "unknown subcommand: $cmd" ;;
esac
