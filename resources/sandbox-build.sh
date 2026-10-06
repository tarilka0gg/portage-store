#!/usr/bin/env bash
# Installed as /usr/libexec/portage-store/sandbox-build.sh, root:root
# 0755. Previously this was written into the invoking user's own (user-
# writable) XDG cache directory and executed from there — under a
# passwordless privileged call, a tampered cache file would be silent,
# zero-friction root code execution, so it now lives here instead, where
# only root can modify it. Content is otherwise unchanged from the
# original `sandbox::BUILD_SCRIPT` constant.
#
# The atom to build is this script's own $1, passed through argv (never
# string-interpolated into the script source by the caller), so nothing
# about a specific build ever needs this file's content to change.
#
# Safety invariants this script is written to make structurally hard to
# violate, because it bind-mounts and overlay-mounts real host
# directories into roots that could otherwise be torn down carelessly:
# - The portage tree is mounted **read-only** into every instance; only
#   distfiles/binpkgs (intentionally shared caches) are writable.
# - Teardown only ever unmounts (each guarded by `mountpoint -q` first)
#   — it never deletes anything. The one destructive step in the whole
#   script (extracting a fresh stage3 into the base) only runs before
#   anything is mounted at all, and only ever touches SANDBOX_BASE,
#   never an instance directory.
# - Every mount this script makes gets torn down in a `trap ... EXIT`,
#   including when a build attempt fails and the script moves on to try
#   the next pool instance — so a failed attempt in instance 0 never
#   leaves it mounted while instance 1 is tried.
set -euo pipefail

ATOM="$1"
BASE="/var/lib/portage-store/sandbox-base"
INSTANCES="/var/lib/portage-store/sandbox-instances"
MAX_INSTANCES=3
REPOS_SRC="/var/db/repos"
DISTFILES_SRC="/var/cache/distfiles"
BINPKGS_SRC="/var/cache/binpkgs"
MIRROR_BASE="https://distfiles.gentoo.org/releases/amd64/autobuilds"

mkdir -p "$BASE" "$INSTANCES"

# --- one-time bootstrap of the shared, pristine base ---------------------
if [ ! -x "$BASE/usr/bin/emerge" ]; then
    echo "==> Sandbox base not set up yet — fetching the current stage3 (first run only)..."
    POINTER=$(curl -fsSL --retry 3 "$MIRROR_BASE/latest-stage3-amd64-openrc.txt")
    STAGE3_PATH=$(echo "$POINTER" | grep -E '^[0-9]{8}T[0-9]{6}Z/stage3-amd64-openrc-' | awk '{print $1}' | head -1)
    if [ -z "$STAGE3_PATH" ]; then
        echo "Could not determine the current stage3 release — aborting." >&2
        exit 1
    fi
    STAGE3_URL="$MIRROR_BASE/$STAGE3_PATH"
    STAGE3_FILE="/var/tmp/portage-store-$(basename "$STAGE3_PATH")"

    echo "==> Downloading $(basename "$STAGE3_PATH")..."
    curl -fL --retry 3 -o "$STAGE3_FILE" "$STAGE3_URL"

    echo "==> Verifying checksum..."
    DIGEST=$(curl -fsSL --retry 3 "$STAGE3_URL.sha256")
    EXPECTED=$(echo "$DIGEST" | grep -E '^[0-9a-f]{64} ' | awk '{print $1}' | head -1)
    ACTUAL=$(sha256sum "$STAGE3_FILE" | awk '{print $1}')
    if [ -z "$EXPECTED" ] || [ "$EXPECTED" != "$ACTUAL" ]; then
        echo "Checksum mismatch for the stage3 tarball — aborting without extracting anything." >&2
        rm -f "$STAGE3_FILE"
        exit 1
    fi

    echo "==> Extracting..."
    tar xpf "$STAGE3_FILE" -C "$BASE" --xattrs-include='*.*' --numeric-owner
    rm -f "$STAGE3_FILE"

    mkdir -p "$BASE/etc/portage/package.use" \
             "$BASE/etc/portage/package.accept_keywords" \
             "$BASE/etc/portage/package.mask"

    # Permissive on purpose: this whole base exists so a package that the
    # *live* system's resolver won't touch (mask, keyword mask, a conflict)
    # can still get built somewhere — there's no reason to be surgical
    # about what's relaxed in a throwaway root nothing else depends on.
    # MAKEOPTS is deliberately conservative (not the host's own, possibly
    # much higher, setting) — an unthrottled parallel build is exactly
    # what OOM-killed unrelated processes the last time this app tried a
    # large native build at full host parallelism.
    cat >> "$BASE/etc/portage/make.conf" <<'MAKECONF'
ACCEPT_KEYWORDS="~amd64"
MAKEOPTS="-j4 -l4"
FEATURES="${FEATURES} buildpkg"
MAKECONF
    echo '*/*' > "$BASE/etc/portage/package.accept_keywords/portage-store-sandbox"
    echo '-*/*' > "$BASE/etc/portage/package.mask/portage-store-sandbox"
fi

# --- per-instance overlay mount/unmount ----------------------------------
mount_instance() {
    local dir="$INSTANCES/$1"
    mkdir -p "$dir/upper" "$dir/work" "$dir/merged"
    if ! mountpoint -q "$dir/merged" 2>/dev/null; then
        mount -t overlay overlay -o "lowerdir=$BASE,upperdir=$dir/upper,workdir=$dir/work" "$dir/merged"
    fi
    mkdir -p "$dir/merged/var/db/repos" "$dir/merged/var/cache/distfiles" "$dir/merged/var/cache/binpkgs" \
             "$dir/merged/proc" "$dir/merged/sys" "$dir/merged/dev"
    mountpoint -q "$dir/merged/var/db/repos"        2>/dev/null || mount --bind -o ro "$REPOS_SRC" "$dir/merged/var/db/repos"
    mountpoint -q "$dir/merged/var/cache/distfiles" 2>/dev/null || mount --bind "$DISTFILES_SRC" "$dir/merged/var/cache/distfiles"
    mountpoint -q "$dir/merged/var/cache/binpkgs"   2>/dev/null || mount --bind "$BINPKGS_SRC" "$dir/merged/var/cache/binpkgs"
    mountpoint -q "$dir/merged/proc" 2>/dev/null || mount -t proc none "$dir/merged/proc"
    mountpoint -q "$dir/merged/sys"  2>/dev/null || mount --rbind /sys "$dir/merged/sys"
    mountpoint -q "$dir/merged/dev"  2>/dev/null || mount --rbind /dev "$dir/merged/dev"
    cp -L /etc/resolv.conf "$dir/merged/etc/resolv.conf"
}

unmount_instance() {
    local dir="$INSTANCES/$1"
    for m in "$dir/merged/dev" "$dir/merged/sys" "$dir/merged/proc" \
             "$dir/merged/var/cache/binpkgs" "$dir/merged/var/cache/distfiles" "$dir/merged/var/db/repos"; do
        if mountpoint -q "$m" 2>/dev/null; then
            umount -R "$m" 2>/dev/null || umount -l "$m" 2>/dev/null || true
        fi
    done
    if mountpoint -q "$dir/merged" 2>/dev/null; then
        umount "$dir/merged" 2>/dev/null || umount -l "$dir/merged" 2>/dev/null || true
    fi
}

CURRENT_INSTANCE=""
cleanup() {
    local status=$?
    if [ -n "$CURRENT_INSTANCE" ]; then
        echo "==> Tearing down sandbox instance $CURRENT_INSTANCE..."
        unmount_instance "$CURRENT_INSTANCE"
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM

# --- try each pool instance in turn until one actually builds it --------
# The common case never leaves instance 0: only a package that genuinely
# conflicts with what's already built there (a SLOT clash, a blocker)
# overflows to the next instance, which starts from the same clean base
# and so doesn't have whatever caused the conflict.
BUILT=0
for n in $(seq 0 $((MAX_INSTANCES - 1))); do
    CURRENT_INSTANCE="$n"
    echo "==> Trying sandbox instance $n..."
    mount_instance "$n"
    if chroot "$INSTANCES/$n/merged" /usr/bin/emerge --ask=n --buildpkg "$ATOM"; then
        BUILT=1
        echo "==> Built $ATOM in sandbox instance $n. The resulting binary package is under $BINPKGS_SRC (shared with the host)."
        break
    fi
    echo "==> Build failed in instance $n."
    unmount_instance "$n"
    CURRENT_INSTANCE=""
    if [ "$n" -lt "$((MAX_INSTANCES - 1))" ]; then
        echo "==> Retrying in a fresh sandbox instance, in case this was a conflict with something already built in instance $n..."
    fi
done

if [ "$BUILT" -ne 1 ]; then
    echo "==> $ATOM could not be built in any of the $MAX_INSTANCES sandbox instances." >&2
    exit 1
fi
