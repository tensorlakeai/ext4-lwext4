//! Regression test for the `ext4_fwrite` dirty-slack bug.
//!
//! A file's final partial block, when written into a freshly-allocated block
//! that the allocator recycled from a recently-freed block, used to keep the
//! freed block's stale bytes in the slack past EOF (lwext4 wrote only `size`
//! bytes and never zeroed `[size, block_size)`). That leaked stale on-disk
//! bytes which the kernel then exposed in the final mmap page — crashing
//! libclang/bindgen on materialized rootfs images
//! (tensorlakeai/compute-engine-internal#1554).
//!
//! This test fills blocks with a marker byte, frees them, writes many
//! non-block-aligned files that reuse those freed blocks, then inspects each
//! file's on-disk final-block slack directly in the image and asserts it is
//! all zero.
//!
//! Requires the `gpl-extents` feature: writing regular files on an ext4 image
//! goes through the extent code, which is only compiled under that feature.
#![cfg(feature = "gpl-extents")]

use std::io::Read;

use ext4_lwext4::{Ext4Fs, FileBlockDevice, MkfsOptions, OpenFlags, mkfs};

const BLOCK_SIZE: usize = 4096;
const MARKER: u8 = 0xAA;
const VICTIMS: usize = 200;
const VICTIM_LEN: usize = 100; // deliberately not a multiple of BLOCK_SIZE

fn victim_content(i: usize) -> Vec<u8> {
    // Unique, findable header so we can locate each file's data block in the
    // raw image without debugfs; padded with a recognizable body.
    let mut v = format!("VICSLACK{i:06}").into_bytes();
    v.resize(VICTIM_LEN, b'Z');
    v
}

#[test]
fn fwrite_zeroes_freshly_allocated_tail_slack() {
    let dir = std::env::temp_dir().join(format!("lwext4-slack-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("out.ext4");

    {
        let device = FileBlockDevice::create(&img, 64 * 1024 * 1024).unwrap();
        mkfs(device, &MkfsOptions::default()).unwrap();
        let device = FileBlockDevice::open(&img).unwrap();
        let fs = Ext4Fs::mount(device, false).unwrap();

        // 1) Fill many blocks with MARKER, then free them so the allocator can
        //    hand them back with MARKER still in the tail slack.
        {
            let filler = vec![MARKER; 16 * 1024 * 1024];
            let mut f = fs
                .open("/filler", OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE)
                .unwrap();
            f.write_all(&filler).unwrap();
            f.sync().unwrap();
        }
        fs.remove("/filler").unwrap();

        // 2) Write many small, non-block-aligned files into the recycled blocks.
        for i in 0..VICTIMS {
            let mut f = fs
                .open(
                    &format!("/victim{i:03}"),
                    OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
                )
                .unwrap();
            f.write_all(&victim_content(i)).unwrap();
            f.sync().unwrap();
        }

        fs.sync().unwrap();
        fs.umount().unwrap();
    }

    // 3) Inspect the raw image: for every victim, the bytes from EOF to the end
    //    of its final block must be zero.
    let mut raw = Vec::new();
    std::fs::File::open(&img)
        .unwrap()
        .read_to_end(&mut raw)
        .unwrap();

    let mut dirty = 0usize;
    for i in 0..VICTIMS {
        let content = victim_content(i);
        let header = &content[..14]; // "VICSLACK000123"
        let Some(pos) = find_subslice(&raw, header) else {
            panic!("victim{i:03} content not found in image");
        };
        // The data block starts at `pos` (files begin at a block boundary);
        // slack is [pos + VICTIM_LEN, pos + BLOCK_SIZE).
        let slack = &raw[pos + VICTIM_LEN..pos + BLOCK_SIZE];
        if slack.iter().any(|&b| b != 0) {
            dirty += 1;
            if dirty == 1 {
                eprintln!("victim{i:03} dirty slack sample: {:02x?}", &slack[..16.min(slack.len())]);
            }
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        dirty, 0,
        "{dirty}/{VICTIMS} files left non-zero slack past EOF (ext4_fwrite tail not zeroed)"
    );
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
