#!/usr/bin/env bash
# Installed as /usr/local/libexec/portage-store/priv-helper, root:root 0755,
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

HELPER_DIR="/usr/local/libexec/portage-store"
TRACKED_DIR="/etc/portage"

fail() {
    echo "priv-helper: $*" >&2
    exit 1
}

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
    git -C "$TRACKED_DIR" revert --no-edit HEAD
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
            if [ -n "$jobs" ]; then
                exec ionice -c 3 nice -n 19 env "MAKEOPTS=-j$jobs -l$jobs" /usr/bin/emerge "$@"
            else
                exec /usr/bin/emerge "$@"
            fi
            ;;
        eselect|eclean-dist|eclean-pkg)
            exec "$binary" "$@"
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
    exec emerge --sync --repo "$name"
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
    exec "$HELPER_DIR/sandbox-build.sh" "$atom"
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
    cp-bundle) cmd_cp_bundle "$@" ;;
    cat-log) cmd_cat_log "$@" ;;
    run) cmd_run "$@" ;;
    sandbox-build) cmd_sandbox_build "$@" ;;
    enable-overlay) cmd_enable_overlay "$@" ;;
    *) fail "unknown subcommand: $cmd" ;;
esac
