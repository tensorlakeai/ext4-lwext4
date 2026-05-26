# Tensorlake fork of ext4-lwext4

This repository is `tensorlakeai/ext4-lwext4`, a fork of
[arcboxlabs/ext4-lwext4][upstream]. It exposes a safe Rust wrapper over the
[lwext4][lwext4] C library; this fork holds Tensorlake-authored patches the
upstream maintainers have not (yet) accepted.

## Branch conventions

| Branch | What it is |
|---|---|
| `master` | Mirror of upstream `arcboxlabs/ext4-lwext4` `master`. Tracks the public project. Do **not** commit Tensorlake-specific changes here. |
| `tensorlake-master` | **The canonical Tensorlake fork tip.** Contains upstream `master` plus Tensorlake-authored patches. Downstream Tensorlake products (`tensorlakeai/compute-engine-internal`) consume this branch by name. |
| Other branches (e.g. `eugene/*`, `fix/*`) | Historical feature branches; do not depend on them. They may be deleted without notice. |

Current Tensorlake-authored patches on `tensorlake-master` (relative to upstream
`master`):

- High-level `Ext4Fs` xattr API: `set_xattr`, `get_xattr`, `list_xattr`,
  `remove_xattr` — gated under the existing `gpl-xattr` Cargo feature.
- Timestamp setters: `set_mtime`, `set_atime`, `set_ctime` on `Ext4Fs` (the
  FFI bindings were already present in `ext4-lwext4-sys`; the high-level
  wrappers were missing).
- Cargo `gpl` meta-feature: routes through the binding's own `gpl-xattr`
  and `gpl-extents` features (which in turn chain to the sys crate),
  rather than only chaining to the sys crate's `gpl`. Without this, the
  binding's own `#[cfg(feature = "gpl-xattr")]` cfg gates around the xattr
  API stay off when consumers write `features = ["gpl"]`.
- `ext4-lwext4-sys/vendor/lwext4` submodule pin to `tensorlakeai/lwext4`'s
  `tensorlake-master` branch, which carries our `lwext4` C-level patches
  (see [`tensorlakeai/lwext4` TENSORLAKE.md][lwext4-tensorlake]).
- `ext4-lwext4-sys/build.rs`: compile the new `ext4_acl.c` from the patched
  lwext4 submodule alongside `ext4_xattr.c` under the `gpl-xattr` feature
  gate.

## Rebasing / merging

`tensorlake-master` is **append-only** from downstream's perspective: future
Tensorlake patches land as new commits on top, never via force-push or history
rewrite (would invalidate every downstream `Cargo.lock` that pins this branch).
Upstream resyncs happen by merging upstream `master` into `tensorlake-master`,
not the other way around.

When the submodule pin needs to advance (e.g. a new C-level fix on
`tensorlakeai/lwext4`'s `tensorlake-master`), bump the submodule pointer in a
fresh commit on `tensorlake-master` here, then bump the `Cargo.lock` of the
downstream consumer.

[upstream]: https://github.com/arcboxlabs/ext4-lwext4
[lwext4]: https://github.com/tensorlakeai/lwext4
[lwext4-tensorlake]: https://github.com/tensorlakeai/lwext4/blob/tensorlake-master/TENSORLAKE.md
