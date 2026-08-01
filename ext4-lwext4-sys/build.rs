use std::env;
use std::path::PathBuf;

fn main() {
    let lwext4_src = PathBuf::from("vendor/lwext4/src");
    let lwext4_inc = PathBuf::from("vendor/lwext4/include");

    // Core source files (BSD-3-Clause / MIT OR Apache-2.0 compatible)
    let mut sources = vec![
        "ext4.c",
        "ext4_balloc.c",
        "ext4_bcache.c",
        "ext4_bitmap.c",
        "ext4_block_group.c",
        "ext4_blockdev.c",
        "ext4_crc32.c",
        "ext4_debug.c",
        "ext4_dir.c",
        "ext4_dir_idx.c",
        "ext4_fs.c",
        "ext4_hash.c",
        "ext4_ialloc.c",
        "ext4_inode.c",
        "ext4_journal.c",
        "ext4_mkfs.c",
        "ext4_super.c",
        "ext4_trans.c",
    ];

    // GPL-2.0 licensed files - only included with explicit feature flags
    if env::var("CARGO_FEATURE_GPL_EXTENTS").is_ok() {
        sources.push("ext4_extent.c");
    }
    if env::var("CARGO_FEATURE_GPL_XATTR").is_ok() {
        sources.push("ext4_xattr.c");
        // POSIX ACL on-disk/userspace-xattr format conversion. BSD-3-Clause
        // itself, but only useful alongside the xattr code so it shares the
        // same feature gate.
        sources.push("ext4_acl.c");
    }

    let mut build = cc::Build::new();

    for src in &sources {
        build.file(lwext4_src.join(src));
    }

    // Helper functions (sizeof, etc.)
    build.file("vendor/helpers.c");

    build
        .include(&lwext4_inc)
        .include(lwext4_inc.join("misc"))
        .define("CONFIG_USE_DEFAULT_CFG", "1")
        .define("CONFIG_HAVE_OWN_OFLAGS", "1");

    // Disable extent support at the C preprocessor level when the GPL feature
    // is not enabled. Without this, ext4_fs.c compiles calls to extent functions
    // (guarded by CONFIG_EXTENTS_ENABLE) but ext4_extent.c is not compiled,
    // causing undefined symbol errors at link time.
    if env::var("CARGO_FEATURE_GPL_EXTENTS").is_err() {
        build.define("CONFIG_EXTENTS_ENABLE", "0");
    }

    build
        .flag_if_supported("-std=c99")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-sign-compare")
        .warnings(false)
        .compile("lwext4");

    // Emit rerun-if-changed per file (recursively). A bare directory path only
    // tracks the directory's own mtime, which does not change when a file inside
    // it is edited — so editing a vendored .c/.h would not trigger a rebuild
    // (and could ship a stale object). Walk and list every file instead.
    emit_rerun_if_changed(std::path::Path::new("vendor/lwext4/src"));
    emit_rerun_if_changed(std::path::Path::new("vendor/lwext4/include"));
    println!("cargo:rerun-if-changed=vendor/helpers.c");
}

fn emit_rerun_if_changed(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            emit_rerun_if_changed(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
