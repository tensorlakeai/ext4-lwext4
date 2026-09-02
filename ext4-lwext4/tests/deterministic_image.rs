//! Regression coverage for uninitialized block-cache and dirent padding bytes.
//!
//! Filesystem images assembled from the same operations must be byte-identical.
//! Cache-miss buffers and directory-name padding used to retain heap contents,
//! which leaked process memory into directory blocks and the journal.
#![cfg(feature = "gpl-extents")]

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use ext4_lwext4::{mkfs, Ext4Fs, FileBlockDevice, MkfsOptions, OpenFlags};

const IMAGE_BYTES: u64 = 64 * 1024 * 1024;

#[test]
fn repeated_directory_heavy_builds_are_byte_identical() {
    let dir =
        std::env::temp_dir().join(format!("lwext4-deterministic-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("first.ext4");
    let second = dir.join("second.ext4");

    build_image(&first);
    build_image(&second);

    assert_files_equal(&first, &second);
    let _ = std::fs::remove_dir_all(&dir);
}

fn build_image(path: &Path) {
    let device = FileBlockDevice::create(path, IMAGE_BYTES).unwrap();
    mkfs(device, &MkfsOptions::default()).unwrap();
    let device = FileBlockDevice::open(path).unwrap();
    let fs = Ext4Fs::mount(device, false).unwrap();

    if fs.exists("/lost+found") {
        fs.remove("/lost+found").unwrap();
    }
    for name in ["bin", "boot", "dev", "etc", "home", "usr", "var"] {
        fs.mkdir(&format!("/{name}"), 0o755).unwrap();
    }
    fs.mkdir("/usr/include", 0o755).unwrap();

    // Force directory-index leaf allocation and enough journal traffic to
    // expose any unwritten bytes in no-read cache buffers.
    for i in 0..512 {
        let path = format!("/usr/include/entry{i:04}");
        let mut file = fs
            .open(
                &path,
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            )
            .unwrap();
        file.write_all(format!("payload-{i:04}").as_bytes())
            .unwrap();
    }

    fs.sync().unwrap();
    fs.umount().unwrap();
}

fn assert_files_equal(first: &Path, second: &Path) {
    let mut first = BufReader::new(File::open(first).unwrap());
    let mut second = BufReader::new(File::open(second).unwrap());
    let mut first_buf = [0_u8; 64 * 1024];
    let mut second_buf = [0_u8; 64 * 1024];
    let mut offset = 0_u64;

    loop {
        let first_len = first.read(&mut first_buf).unwrap();
        let second_len = second.read(&mut second_buf).unwrap();
        assert_eq!(
            first_len, second_len,
            "image lengths differ at byte {offset}"
        );
        assert_eq!(
            &first_buf[..first_len],
            &second_buf[..second_len],
            "image contents differ at byte {offset}"
        );
        if first_len == 0 {
            break;
        }
        offset += first_len as u64;
    }
}
