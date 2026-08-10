# Packaging

Local files only — nothing here has been submitted anywhere. Review and
fix the items below before opening a GURU pull request.

## What's here

- `app-portage/portage-store/portage-store-9999.ebuild` — a live
  (`git-r3`) ebuild using `cargo.eclass`.
- `app-portage/portage-store/metadata.xml` — Gentoo's per-package
  metadata (maintainer, upstream remote-id).
- `org.gentoo.PortageStore.desktop` — freedesktop desktop entry.
- `org.gentoo.PortageStore.metainfo.xml` — AppStream metainfo (what
  GNOME Software/Discover/a future Gentoo software-center-style tool
  would show).
- `icons/org.gentoo.PortageStore.svg` — a placeholder icon.

## Before submitting, fix these

1. **`HELPER_PATH` mismatch.** `src/portage/priv_write.rs` hardcodes the
   privileged helper's path to `/usr/local/libexec/portage-store/priv-helper`
   — correct for the manual install this app was developed against, but
   `/usr/local` is reserved for locally-installed software outside the
   package manager under the FHS. The ebuild here installs the helper to
   `/usr/libexec/portage-store/priv-helper` instead (the conventional path
   for a package-managed helper), which means the binary and the ebuild
   currently *disagree* — installing via this ebuild as-is will leave
   `portage-store` looking for a helper that isn't at the path the ebuild
   put it. Fix `HELPER_PATH` (and the matching comment in
   `resources/priv-helper.sh`) to `/usr/libexec/...` before this ebuild is
   actually usable, or change the ebuild's `exeinto` to match — pick one,
   don't submit with the mismatch.
2. **`CRATES=""` is empty.** Generate the real value with
   `dev-util/cargo-ebuild` run against this repo's `Cargo.lock` (116
   crates as of when this was drafted) — too many to transcribe by hand
   without risking a wrong checksum. `cargo-ebuild` writes the whole
   `CRATES` block for you; paste it in.
3. **Placeholders**: `<maintainer>` in `metadata.xml`, `<screenshots>` in
   the metainfo, and the icon itself (a generic placeholder glyph, not
   real branding) all need real values — none were guessed here.
4. **`KEYWORDS=""`** is intentionally empty — a brand-new package starts
   keyword-masked; don't add `~amd64` etc. until it's actually been built
   and tested via the ebuild itself, not just `cargo build`.

## Testing locally

```sh
# From a local overlay (or GURU checkout) containing this package path:
ebuild app-portage/portage-store/portage-store-9999.ebuild manifest
ebuild app-portage/portage-store/portage-store-9999.ebuild merge
```

`pkgcheck scan` (from `dev-util/pkgcheck`) against the same path catches
most GURU-style QA issues before a PR does.

## Submitting to GURU

GURU (https://github.com/gentoo/guru) takes pull requests directly —
fork the repo, add this package under `app-portage/portage-store/`, open
a PR. Their CI runs `pkgcheck` automatically; expect at least one review
round. See GURU's own `CONTRIBUTING.md` for the current process, since it
occasionally changes.

## doas setup

The ebuild's `pkg_postinst` prints the exact `doas.conf` line needed —
intentionally not written automatically by the ebuild itself, since a
package installer silently editing a system auth file is not something
this app (or any ebuild) should do without the admin's own action.
