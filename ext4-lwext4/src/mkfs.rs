//! Filesystem creation (mkfs) functionality.

use crate::blockdev::{BlockDevice, BlockDeviceWrapper};
use crate::error::{check_errno, Result};
use crate::types::FsType;
use ext4_lwext4_sys::{ext4_fs, ext4_mkfs, ext4_mkfs_info, UUID_SIZE};
use std::ffi::CString;
use std::ptr;

/// Options for creating a new ext2/3/4 filesystem.
#[derive(Debug, Clone)]
pub struct MkfsOptions {
    /// Filesystem type (ext2, ext3, or ext4)
    pub fs_type: FsType,
    /// Block size in bytes (1024, 2048, or 4096)
    pub block_size: u32,
    /// Inode size in bytes (128 or 256)
    pub inode_size: u32,
    /// Enable journaling (ext3/ext4 only)
    pub journal: bool,
    /// Filesystem label (max 16 characters)
    pub label: Option<String>,
    /// UUID for the filesystem
    pub uuid: Option<[u8; 16]>,
}

impl Default for MkfsOptions {
    fn default() -> Self {
        Self {
            fs_type: FsType::Ext4,
            block_size: 4096,
            inode_size: 256,
            journal: true,
            label: None,
            uuid: None,
        }
    }
}

impl MkfsOptions {
    /// Create options for ext2 filesystem (no journal).
    pub fn ext2() -> Self {
        Self {
            fs_type: FsType::Ext2,
            journal: false,
            ..Default::default()
        }
    }

    /// Create options for ext3 filesystem.
    pub fn ext3() -> Self {
        Self {
            fs_type: FsType::Ext3,
            ..Default::default()
        }
    }

    /// Create options for ext4 filesystem.
    pub fn ext4() -> Self {
        Self::default()
    }

    /// Set the filesystem label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the UUID.
    pub fn with_uuid(mut self, uuid: [u8; 16]) -> Self {
        self.uuid = Some(uuid);
        self
    }

    /// Set the block size.
    pub fn with_block_size(mut self, size: u32) -> Self {
        self.block_size = size;
        self
    }

    /// Enable or disable journaling.
    pub fn with_journal(mut self, enabled: bool) -> Self {
        self.journal = enabled;
        self
    }
}

/// Create a new ext2/3/4 filesystem on a block device.
///
/// # Arguments
/// * `device` - The block device to format
/// * `options` - Filesystem creation options
///
/// # Example
/// ```no_run
/// use ext4_lwext4::{mkfs, MkfsOptions, MemoryBlockDevice};
///
/// let device = MemoryBlockDevice::new(100 * 1024 * 1024, 512);
/// mkfs(device, &MkfsOptions::default()).unwrap();
/// ```
pub fn mkfs<B: BlockDevice + 'static>(device: B, options: &MkfsOptions) -> Result<()> {
    // Create the wrapper for the device
    let wrapper = BlockDeviceWrapper::new(device);

    // Get the raw pointer to the block device
    let bdev_ptr = wrapper.as_bdev_ptr();

    // Create the label CString if provided
    let label_cstring = options
        .label
        .as_ref()
        .map(|l| CString::new(l.as_str()).unwrap());

    // Prepare the mkfs info structure
    let mut info = ext4_mkfs_info {
        len: unsafe { (*bdev_ptr).part_size },
        block_size: options.block_size,
        blocks_per_group: 0, // Let lwext4 calculate
        inodes_per_group: 0, // Let lwext4 calculate
        inode_size: options.inode_size,
        inodes: 0, // Let lwext4 calculate
        journal_blocks: 0, // Let lwext4 calculate
        feat_ro_compat: 0,
        feat_compat: 0,
        feat_incompat: 0,
        bg_desc_reserve_blocks: 0,
        dsc_size: 0,
        uuid: options.uuid.unwrap_or([0u8; UUID_SIZE]),
        journal: options.journal && options.fs_type != FsType::Ext2,
        label: label_cstring
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(ptr::null()),
    };

    // Allocate a zeroed ext4_fs struct (opaque, size known only to C)
    let fs_size = unsafe { ext4_lwext4_sys::lwext4_sizeof_ext4_fs() };
    let fs_buf = vec![0u8; fs_size];
    let fs_ptr = fs_buf.as_ptr() as *mut ext4_fs;

    // Call lwext4 mkfs
    let ret = unsafe {
        ext4_mkfs(
            fs_ptr,
            bdev_ptr,
            &mut info,
            options.fs_type.to_raw(),
        )
    };

    check_errno(ret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockdev::FileBlockDevice;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    use tempfile::TempDir;

    fn read_exact_at(file: &mut File, offset: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        file.seek(SeekFrom::Start(offset)).expect("seek");
        file.read_exact(&mut buf).expect("read");
        buf
    }

    fn read_u16(buf: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(buf[offset..offset + 2].try_into().unwrap())
    }

    fn read_u32(buf: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
    }

    fn read_u64_lo_hi(buf: &[u8], lo_offset: usize, hi_offset: usize) -> u64 {
        read_u32(buf, lo_offset) as u64 | ((read_u32(buf, hi_offset) as u64) << 32)
    }

    #[test]
    fn test_mkfs_options_builder() {
        let opts = MkfsOptions::ext4()
            .with_label("test_disk")
            .with_block_size(4096)
            .with_journal(true);

        assert_eq!(opts.fs_type, FsType::Ext4);
        assert_eq!(opts.label, Some("test_disk".to_string()));
        assert_eq!(opts.block_size, 4096);
        assert!(opts.journal);
    }

    fn assert_partial_last_group_free_counts_match_bitmap(
        block_size: u32,
        real_last_group_blocks: u64,
    ) {
        let first_data_block = if block_size > 1024 { 0 } else { 1 };
        let blocks_per_group = block_size as u64 * 8;
        let image_blocks = first_data_block + blocks_per_group + real_last_group_blocks;
        let dir = TempDir::new().unwrap();
        let path = dir
            .path()
            .join(format!("partial-last-group-{}.img", block_size));
        let device =
            FileBlockDevice::create_with_block_size(&path, image_blocks * block_size as u64, 512)
                .expect("create disk");

        mkfs(
            device,
            &MkfsOptions::ext4()
                .with_block_size(block_size)
                .with_journal(false),
        )
        .expect("mkfs");

        let mut file = File::open(&path).expect("open image");
        let sb = read_exact_at(&mut file, 1024, 1024);
        let blocks = read_u64_lo_hi(&sb, 4, 0x150);
        let superblock_free = read_u64_lo_hi(&sb, 12, 0x158);
        let actual_first_data_block = read_u32(&sb, 20) as u64;
        let actual_block_size = 1024u64 << read_u32(&sb, 24);
        let actual_blocks_per_group = read_u32(&sb, 32) as u64;
        let desc_size = read_u16(&sb, 254).max(32) as usize;
        let groups = (blocks - actual_first_data_block).div_ceil(actual_blocks_per_group);
        let last_group = groups - 1;
        let actual_real_last_group_blocks =
            blocks - actual_first_data_block - actual_blocks_per_group * last_group;

        assert_eq!(actual_block_size, block_size as u64);
        assert_eq!(actual_first_data_block, first_data_block);
        assert_eq!(actual_blocks_per_group, blocks_per_group);
        assert_eq!(groups, 2);
        assert_eq!(actual_real_last_group_blocks, real_last_group_blocks);

        let descriptor_table_offset = if actual_block_size == 1024 {
            2 * actual_block_size
        } else {
            actual_block_size
        };
        let mut descriptor_free_sum = 0u64;
        let mut last_descriptor_free = 0u64;
        let mut last_bitmap_free = 0u64;
        for group in 0..groups {
            let desc = read_exact_at(
                &mut file,
                descriptor_table_offset + group * desc_size as u64,
                desc_size,
            );
            let descriptor_free = read_u16(&desc, 12) as u64
                | if desc_size > 32 {
                    (read_u16(&desc, 44) as u64) << 16
                } else {
                    0
                };
            descriptor_free_sum += descriptor_free;

            if group == last_group {
                last_descriptor_free = descriptor_free;
                let bitmap_block = read_u32(&desc, 0) as u64
                    | if desc_size > 32 {
                        (read_u32(&desc, 32) as u64) << 32
                    } else {
                        0
                    };
                let bitmap = read_exact_at(
                    &mut file,
                    bitmap_block * actual_block_size,
                    actual_block_size as usize,
                );
                for bit in 0..actual_blocks_per_group {
                    if bitmap[(bit / 8) as usize] & (1 << (bit % 8)) == 0 {
                        last_bitmap_free += 1;
                    }
                }
            }
        }

        assert_eq!(last_descriptor_free, last_bitmap_free);
        assert_eq!(superblock_free, descriptor_free_sum);
    }

    #[test]
    fn mkfs_partial_last_group_free_counts_match_bitmap_4k() {
        assert_partial_last_group_free_counts_match_bitmap(4096, 8192);
    }

    #[test]
    fn mkfs_partial_last_group_free_counts_match_bitmap_1k() {
        assert_partial_last_group_free_counts_match_bitmap(1024, 2047);
    }
}
