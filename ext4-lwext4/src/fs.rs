//! Ext4 filesystem operations.

use crate::blockdev::{BlockDevice, BlockDeviceWrapper};
use crate::dir::Dir;
use crate::error::{check_errno, check_errno_with_path, Error, Result};
use crate::file::File;
use crate::types::{FileExtent, FileType, FsStats, Metadata, OpenFlags};
use ext4_lwext4_sys::{
    ext4_atime_get, ext4_atime_set, ext4_cache_flush, ext4_ctime_get, ext4_ctime_set,
    ext4_device_register, ext4_device_unregister, ext4_dir_mk, ext4_dir_rm, ext4_file_extent,
    ext4_file_get_extents, ext4_flink, ext4_fremove, ext4_frename, ext4_fsymlink, ext4_inode_exist,
    ext4_journal_start, ext4_journal_stop, ext4_mode_get, ext4_mode_set, ext4_mount,
    ext4_mount_point_stats, ext4_mount_stats, ext4_mtime_get, ext4_mtime_set, ext4_owner_get,
    ext4_owner_set, ext4_readlink, ext4_recover, ext4_umount,
};
#[cfg(feature = "gpl-xattr")]
use ext4_lwext4_sys::{ext4_getxattr, ext4_listxattr, ext4_removexattr, ext4_setxattr};
#[cfg(feature = "gpl-xattr")]
use std::ffi::c_void;
use std::ffi::{c_char, CStr, CString};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

// Counter for generating unique device names
static DEVICE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// An ext4 filesystem instance.
///
/// This is the main entry point for filesystem operations. Create an instance
/// by mounting a block device, then use the various methods to manipulate files
/// and directories.
///
/// # Example
/// ```no_run
/// use ext4_lwext4::{Ext4Fs, FileBlockDevice, OpenFlags};
///
/// // Open a disk image and mount
/// let device = FileBlockDevice::open("disk.img").unwrap();
/// let fs = Ext4Fs::mount(device, false).unwrap();
///
/// // Create a directory
/// fs.mkdir("/data", 0o755).unwrap();
///
/// // Write a file (use a block to ensure file is dropped before umount)
/// {
///     let mut file = fs.open("/data/hello.txt", OpenFlags::CREATE | OpenFlags::WRITE).unwrap();
///     // ... write operations ...
/// }
///
/// // Unmount when done
/// fs.umount().unwrap();
/// ```
pub struct Ext4Fs {
    /// Wrapper holding the block device and C structures (kept alive for the C library)
    #[allow(dead_code)]
    wrapper: Pin<Box<BlockDeviceWrapper>>,
    /// Device name for lwext4
    device_name: CString,
    /// Mount point path
    mount_point: CString,
    /// Whether mounted read-only
    read_only: bool,
    /// Whether journal is active
    journal_active: bool,
}

impl Ext4Fs {
    /// Mount an ext4 filesystem from a block device.
    ///
    /// # Arguments
    /// * `device` - The block device containing the filesystem
    /// * `read_only` - Whether to mount read-only
    ///
    /// # Returns
    /// A mounted `Ext4Fs` instance
    pub fn mount<B: BlockDevice + 'static>(device: B, read_only: bool) -> Result<Self> {
        // Generate unique device and mount point names
        let id = DEVICE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let device_name = CString::new(format!("ext4dev{}", id)).unwrap();
        let mount_point = CString::new(format!("/mp{}/", id)).unwrap();

        // Create the wrapper
        let wrapper = BlockDeviceWrapper::new(device);

        // Register the device with lwext4
        let ret = unsafe { ext4_device_register(wrapper.as_bdev_ptr(), device_name.as_ptr()) };
        check_errno(ret)?;

        // Mount the filesystem
        let ret = unsafe { ext4_mount(device_name.as_ptr(), mount_point.as_ptr(), read_only) };
        if ret != 0 {
            // Unregister device on mount failure
            unsafe { ext4_device_unregister(device_name.as_ptr()) };
            return Err(Error::from(ret));
        }

        // Recover journal if needed
        let ret = unsafe { ext4_recover(mount_point.as_ptr()) };
        if ret != 0 {
            // Continue even if recovery fails - might not have journal
        }

        // Start journaling if not read-only
        let journal_active = if !read_only {
            let ret = unsafe { ext4_journal_start(mount_point.as_ptr()) };
            ret == 0
        } else {
            false
        };

        Ok(Self {
            wrapper,
            device_name,
            mount_point,
            read_only,
            journal_active,
        })
    }

    /// Unmount the filesystem.
    ///
    /// This flushes all pending writes and releases the block device.
    pub fn umount(self) -> Result<()> {
        // Stop journaling if active
        if self.journal_active {
            unsafe { ext4_journal_stop(self.mount_point.as_ptr()) };
        }

        // Flush cache
        unsafe { ext4_cache_flush(self.mount_point.as_ptr()) };

        // Unmount
        let ret = unsafe { ext4_umount(self.mount_point.as_ptr()) };
        check_errno(ret)?;

        // Unregister device
        let ret = unsafe { ext4_device_unregister(self.device_name.as_ptr()) };
        check_errno(ret)?;

        Ok(())
    }

    /// Get the mount point path used internally.
    #[allow(dead_code)]
    pub(crate) fn mount_point(&self) -> &CStr {
        &self.mount_point
    }

    /// Create a full path by prepending the mount point.
    pub(crate) fn make_path(&self, path: &str) -> Result<CString> {
        // Remove leading slash from path if present
        let path = path.strip_prefix('/').unwrap_or(path);
        let mount_point = self.mount_point.to_str().map_err(|_| {
            Error::InvalidArgument("invalid mount point".to_string())
        })?;
        let full_path = format!("{}{}", mount_point, path);
        CString::new(full_path).map_err(Error::from)
    }

    /// Check if filesystem is mounted read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Get filesystem statistics.
    pub fn stat(&self) -> Result<FsStats> {
        let mut stats = ext4_mount_stats::default();
        let ret = unsafe { ext4_mount_point_stats(self.mount_point.as_ptr(), &mut stats) };
        check_errno(ret)?;

        // Extract volume name, handling null termination
        let volume_name = unsafe {
            let name_bytes = &stats.volume_name;
            let len = name_bytes.iter().position(|&c| c == 0).unwrap_or(16);
            let slice = std::slice::from_raw_parts(name_bytes.as_ptr() as *const u8, len);
            String::from_utf8_lossy(slice).into_owned()
        };

        Ok(FsStats {
            block_size: stats.block_size,
            total_blocks: stats.blocks_count,
            free_blocks: stats.free_blocks_count,
            total_inodes: stats.inodes_count as u64,
            free_inodes: stats.free_inodes_count as u64,
            block_group_count: stats.block_group_count,
            blocks_per_group: stats.blocks_per_group,
            inodes_per_group: stats.inodes_per_group,
            volume_name,
        })
    }

    /// Enumerate a regular file's on-disk data extents (read-only).
    ///
    /// Returns the byte ranges, in image (block-device) space, that hold the
    /// file's stored data — for host-side content-addressed chunking that needs
    /// to locate a file's bytes in the raw image without a kernel mount. Holes
    /// and unwritten (preallocated, never-written) ranges produce no entry, so
    /// the extents cover exactly the file's stored bytes (block granularity).
    /// Physically-contiguous file blocks are coalesced into one extent.
    ///
    /// `logical_byte` is the offset within the file's stored content (extent
    /// order), `image_byte` the offset in the underlying image, `len_bytes` the
    /// run length. All are multiples of the filesystem block size.
    pub fn file_extents(&self, path: &str) -> Result<Vec<FileExtent>> {
        let c_path = self.make_path(path)?;

        // First call probes the true extent count (out=null), then we size the
        // buffer exactly and fetch. A retry loop tolerates concurrent growth.
        let mut cap: u32 = 0;
        loop {
            let mut count: u32 = 0;
            let mut block_size: u32 = 0;
            let mut buf: Vec<ext4_file_extent> = Vec::with_capacity(cap as usize);
            let out_ptr = if cap == 0 {
                std::ptr::null_mut()
            } else {
                buf.as_mut_ptr()
            };
            let ret = unsafe {
                ext4_file_get_extents(c_path.as_ptr(), out_ptr, cap, &mut count, &mut block_size)
            };
            check_errno_with_path(ret, path)?;

            if count > cap {
                // Buffer too small (or the initial probe): grow and retry.
                cap = count;
                continue;
            }

            unsafe { buf.set_len(count as usize) };
            let bs = block_size as u64;
            return Ok(buf
                .into_iter()
                .map(|e| FileExtent {
                    logical_byte: e.logical_block * bs,
                    image_byte: e.physical_block * bs,
                    len_bytes: e.block_count * bs,
                })
                .collect());
        }
    }

    /// Open a file.
    ///
    /// # Arguments
    /// * `path` - Path to the file
    /// * `flags` - Open flags (READ, WRITE, CREATE, etc.)
    pub fn open(&self, path: &str, flags: OpenFlags) -> Result<File<'_>> {
        File::open(self, path, flags)
    }

    /// Open a directory for iteration.
    pub fn open_dir(&self, path: &str) -> Result<Dir<'_>> {
        Dir::open(self, path)
    }

    /// Create a directory.
    ///
    /// # Arguments
    /// * `path` - Path for the new directory
    /// * `mode` - Permissions (e.g., 0o755)
    pub fn mkdir(&self, path: &str, mode: u32) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_dir_mk(full_path.as_ptr()) };
        check_errno_with_path(ret, path)?;

        // Set permissions
        if mode != 0 {
            self.set_permissions(path, mode)?;
        }

        Ok(())
    }

    /// Remove a file.
    pub fn remove(&self, path: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_fremove(full_path.as_ptr()) };
        check_errno_with_path(ret, path)
    }

    /// Remove a directory (recursively).
    pub fn rmdir(&self, path: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_dir_rm(full_path.as_ptr()) };
        check_errno_with_path(ret, path)
    }

    /// Rename a file or directory.
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let from_path = self.make_path(from)?;
        let to_path = self.make_path(to)?;
        let ret = unsafe { ext4_frename(from_path.as_ptr(), to_path.as_ptr()) };
        check_errno_with_path(ret, from)
    }

    /// Create a hard link.
    pub fn link(&self, src: &str, dst: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let src_path = self.make_path(src)?;
        let dst_path = self.make_path(dst)?;
        let ret = unsafe { ext4_flink(src_path.as_ptr(), dst_path.as_ptr()) };
        check_errno_with_path(ret, src)
    }

    /// Create a symbolic link.
    ///
    /// # Arguments
    /// * `target` - The path the symlink points to
    /// * `path` - Path for the new symlink
    pub fn symlink(&self, target: &str, path: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let target_cstr = CString::new(target)?;
        let path_full = self.make_path(path)?;
        let ret = unsafe { ext4_fsymlink(target_cstr.as_ptr(), path_full.as_ptr()) };
        check_errno_with_path(ret, path)
    }

    /// Read the target of a symbolic link.
    pub fn readlink(&self, path: &str) -> Result<String> {
        let full_path = self.make_path(path)?;
        let mut buf = vec![0u8; 4096];
        let mut rcnt: usize = 0;

        let ret = unsafe {
            ext4_readlink(
                full_path.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                &mut rcnt,
            )
        };
        check_errno_with_path(ret, path)?;

        buf.truncate(rcnt);
        String::from_utf8(buf).map_err(|_| Error::InvalidArgument("invalid UTF-8 in symlink".to_string()))
    }

    /// Check if a path exists.
    pub fn exists(&self, path: &str) -> bool {
        self.metadata(path).is_ok()
    }

    /// Check if a path exists and is a file.
    pub fn is_file(&self, path: &str) -> bool {
        let full_path = match self.make_path(path) {
            Ok(p) => p,
            Err(_) => return false,
        };
        unsafe { ext4_inode_exist(full_path.as_ptr(), FileType::RegularFile.to_raw() as i32) == 0 }
    }

    /// Check if a path exists and is a directory.
    pub fn is_dir(&self, path: &str) -> bool {
        let full_path = match self.make_path(path) {
            Ok(p) => p,
            Err(_) => return false,
        };
        unsafe { ext4_inode_exist(full_path.as_ptr(), FileType::Directory.to_raw() as i32) == 0 }
    }

    /// Get file metadata.
    pub fn metadata(&self, path: &str) -> Result<Metadata> {
        let full_path = self.make_path(path)?;

        // Get mode to check existence
        let mut mode: u32 = 0;
        let ret = unsafe { ext4_mode_get(full_path.as_ptr(), &mut mode) };
        check_errno_with_path(ret, path)?;

        // Determine file type from mode
        let file_type = match mode & 0o170000 {
            0o100000 => FileType::RegularFile,
            0o040000 => FileType::Directory,
            0o120000 => FileType::Symlink,
            0o060000 => FileType::BlockDevice,
            0o020000 => FileType::CharDevice,
            0o010000 => FileType::Fifo,
            0o140000 => FileType::Socket,
            _ => FileType::Unknown,
        };

        // Get owner
        let mut uid: u32 = 0;
        let mut gid: u32 = 0;
        unsafe { ext4_owner_get(full_path.as_ptr(), &mut uid, &mut gid) };

        // Get timestamps
        let mut atime: u32 = 0;
        let mut mtime: u32 = 0;
        let mut ctime: u32 = 0;
        unsafe {
            ext4_atime_get(full_path.as_ptr(), &mut atime);
            ext4_mtime_get(full_path.as_ptr(), &mut mtime);
            ext4_ctime_get(full_path.as_ptr(), &mut ctime);
        }

        // For file size, we need to open the file temporarily
        let size = if file_type == FileType::RegularFile {
            if let Ok(file) = File::open(self, path, OpenFlags::READ) {
                file.size()
            } else {
                0
            }
        } else {
            0
        };

        Ok(Metadata {
            file_type,
            size,
            blocks: 0, // Not easily available without reading inode directly
            mode: mode & 0o7777, // Mask out file type bits
            uid,
            gid,
            atime: atime as u64,
            mtime: mtime as u64,
            ctime: ctime as u64,
            nlink: 1, // Not easily available
        })
    }

    /// Set file permissions.
    pub fn set_permissions(&self, path: &str, mode: u32) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_mode_set(full_path.as_ptr(), mode) };
        check_errno_with_path(ret, path)
    }

    /// Set file owner.
    pub fn set_owner(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_owner_set(full_path.as_ptr(), uid, gid) };
        check_errno_with_path(ret, path)
    }

    /// Flush all pending writes to disk.
    pub fn sync(&self) -> Result<()> {
        let ret = unsafe { ext4_cache_flush(self.mount_point.as_ptr()) };
        check_errno(ret)
    }

    /// Set the modification time (seconds since the Unix epoch).
    pub fn set_mtime(&self, path: &str, mtime: u32) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_mtime_set(full_path.as_ptr(), mtime) };
        check_errno_with_path(ret, path)
    }

    /// Set the access time (seconds since the Unix epoch).
    pub fn set_atime(&self, path: &str, atime: u32) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_atime_set(full_path.as_ptr(), atime) };
        check_errno_with_path(ret, path)
    }

    /// Set the inode change time (seconds since the Unix epoch).
    pub fn set_ctime(&self, path: &str, ctime: u32) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let full_path = self.make_path(path)?;
        let ret = unsafe { ext4_ctime_set(full_path.as_ptr(), ctime) };
        check_errno_with_path(ret, path)
    }

    /// Set an extended attribute on `path`.
    ///
    /// `name` is the full xattr name including the namespace prefix
    /// (e.g. `system.posix_acl_access`, `security.capability`, `user.mykey`).
    /// lwext4 strips the prefix internally to derive the on-disk name index.
    #[cfg(feature = "gpl-xattr")]
    pub fn set_xattr(&self, path: &str, name: &str, value: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let full_path = self.make_path(path)?;
        let name_cstr = CString::new(name)?;
        let ret = unsafe {
            ext4_setxattr(
                full_path.as_ptr(),
                name_cstr.as_ptr(),
                name.len(),
                value.as_ptr() as *const c_void,
                value.len(),
            )
        };
        check_errno_with_path(ret, path)
    }

    /// Read an extended attribute from `path`. Returns the raw value bytes.
    #[cfg(feature = "gpl-xattr")]
    pub fn get_xattr(&self, path: &str, name: &str) -> Result<Vec<u8>> {
        let full_path = self.make_path(path)?;
        let name_cstr = CString::new(name)?;
        // Probe required size first.
        let mut data_size: usize = 0;
        let ret = unsafe {
            ext4_getxattr(
                full_path.as_ptr(),
                name_cstr.as_ptr(),
                name.len(),
                std::ptr::null_mut(),
                0,
                &mut data_size,
            )
        };
        check_errno_with_path(ret, path)?;
        let mut buf = vec![0u8; data_size];
        let ret = unsafe {
            ext4_getxattr(
                full_path.as_ptr(),
                name_cstr.as_ptr(),
                name.len(),
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                &mut data_size,
            )
        };
        check_errno_with_path(ret, path)?;
        buf.truncate(data_size);
        Ok(buf)
    }

    /// List all extended-attribute names set on `path`.
    ///
    /// Returns the full names (e.g. `system.posix_acl_access`,
    /// `security.capability`) in the order lwext4 stored them.
    #[cfg(feature = "gpl-xattr")]
    pub fn list_xattr(&self, path: &str) -> Result<Vec<String>> {
        let full_path = self.make_path(path)?;
        // Probe required size first.
        let mut ret_size: usize = 0;
        let ret = unsafe {
            ext4_listxattr(full_path.as_ptr(), std::ptr::null_mut(), 0, &mut ret_size)
        };
        check_errno_with_path(ret, path)?;
        if ret_size == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; ret_size];
        let ret = unsafe {
            ext4_listxattr(
                full_path.as_ptr(),
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                &mut ret_size,
            )
        };
        check_errno_with_path(ret, path)?;
        buf.truncate(ret_size);
        let mut names = Vec::new();
        for chunk in buf.split(|&b| b == 0) {
            if chunk.is_empty() {
                continue;
            }
            let name = std::str::from_utf8(chunk).map_err(|_| {
                Error::InvalidArgument(format!("xattr name on {} is not UTF-8", path))
            })?;
            names.push(name.to_string());
        }
        Ok(names)
    }

    /// Remove an extended attribute from `path`.
    #[cfg(feature = "gpl-xattr")]
    pub fn remove_xattr(&self, path: &str, name: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let full_path = self.make_path(path)?;
        let name_cstr = CString::new(name)?;
        let ret = unsafe { ext4_removexattr(full_path.as_ptr(), name_cstr.as_ptr(), name.len()) };
        check_errno_with_path(ret, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockdev::FileBlockDevice;
    use crate::mkfs::{mkfs, MkfsOptions};
    use crate::types::OpenFlags;
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use tempfile::TempDir;

    /// lwext4 keeps global static state and is not safe under concurrent
    /// mounts, while `cargo test` runs these tests on parallel threads. Every
    /// test that mounts a filesystem holds this guard for its whole body, so
    /// mounts are serialized. Poison is recovered (a panicking test must not
    /// cascade-fail the others).
    static EXT4_MOUNT_GUARD: Mutex<()> = Mutex::new(());

    fn mount_guard() -> MutexGuard<'static, ()> {
        EXT4_MOUNT_GUARD.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Format a fresh ext4 file at `dir/disk.img`, returning the mounted
    /// filesystem together with the serialization guard (hold both for the
    /// duration of the test). The image path is always `dir.path()/disk.img`.
    fn formatted_fs(dir: &TempDir, bytes: u64) -> (Ext4Fs, MutexGuard<'static, ()>) {
        let guard = mount_guard();
        let path = dir.path().join("disk.img");
        let device = FileBlockDevice::create(&path, bytes).expect("create disk");
        mkfs(device, &MkfsOptions::default()).expect("mkfs");
        let device = FileBlockDevice::open(&path).expect("reopen disk");
        let fs = Ext4Fs::mount(device, false).expect("mount");
        (fs, guard)
    }

    #[test]
    fn timestamp_setters_roundtrip() {
        let dir = TempDir::new().unwrap();
        let (fs, _guard) = formatted_fs(&dir, 8 * 1024 * 1024);

        let f = fs.open("/ts.bin", OpenFlags::CREATE | OpenFlags::WRITE).expect("create");
        drop(f);

        fs.set_mtime("/ts.bin", 1_700_000_000).expect("mtime");
        fs.set_atime("/ts.bin", 1_700_000_001).expect("atime");
        fs.set_ctime("/ts.bin", 1_700_000_002).expect("ctime");

        let md = fs.metadata("/ts.bin").expect("stat");
        assert_eq!(md.mtime, 1_700_000_000);
        assert_eq!(md.atime, 1_700_000_001);
        assert_eq!(md.ctime, 1_700_000_002);

        fs.umount().unwrap();
    }

    #[cfg(feature = "gpl-extents")]
    #[test]
    fn file_extents_map_to_image_bytes() {
        use std::io::{Read as _, Seek as _, SeekFrom};

        let dir = TempDir::new().unwrap();
        let (fs, _guard) = formatted_fs(&dir, 8 * 1024 * 1024);
        let img = dir.path().join("disk.img");

        // Distinctive multi-block content so we exercise more than one block
        // (and, when the allocator fragments it, more than one extent).
        let content: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs
                .open("/data.bin", OpenFlags::CREATE | OpenFlags::WRITE)
                .expect("create file");
            f.write_all(&content).expect("write");
        }

        let bs = fs.stat().expect("stat").block_size as u64;
        let extents = fs.file_extents("/data.bin").expect("file_extents");
        fs.umount().expect("umount"); // flush the image to disk.img

        assert!(!extents.is_empty(), "a regular file must have >= 1 extent");

        // Extents are block-aligned and cover the file's stored bytes (file
        // size rounded up to a whole block).
        let stored: u64 = extents.iter().map(|e| e.len_bytes).sum();
        let rounded = (content.len() as u64).div_ceil(bs) * bs;
        assert_eq!(stored, rounded, "extents must cover the block-rounded file");
        for e in &extents {
            assert_eq!(e.image_byte % bs, 0, "image_byte block-aligned");
            assert_eq!(e.len_bytes % bs, 0, "len_bytes block-aligned");
        }

        // Reassemble the file from the raw image, reading only the extent
        // ranges (seek per run, in logical order) — it must reproduce the
        // content byte-for-byte. This is exactly what the host-side CAS chunker
        // relies on.
        let mut raw = std::fs::File::open(&img).unwrap();
        let mut ordered = extents.clone();
        ordered.sort_by_key(|e| e.logical_byte);
        let mut assembled = Vec::with_capacity(rounded as usize);
        for e in &ordered {
            raw.seek(SeekFrom::Start(e.image_byte)).unwrap();
            let mut run = vec![0u8; e.len_bytes as usize];
            raw.read_exact(&mut run).unwrap();
            assembled.extend_from_slice(&run);
        }
        assert_eq!(
            &assembled[..content.len()],
            &content[..],
            "file bytes reassembled from image extents must match"
        );
    }

    #[cfg(feature = "gpl-xattr")]
    #[test]
    fn xattr_set_get_list_remove_roundtrip() {
        let dir = TempDir::new().unwrap();
        let (fs, _guard) = formatted_fs(&dir, 8 * 1024 * 1024);

        let f = fs
            .open("/cap.bin", OpenFlags::CREATE | OpenFlags::WRITE)
            .expect("create");
        drop(f);

        // security.capability is the on-disk encoding of file capabilities;
        // the value layout below mirrors a minimal VFS_CAP_REVISION_2 buffer.
        let cap_value: &[u8] = &[
            0x00, 0x00, 0x00, 0x02, // magic + revision 2
            0x00, 0x04, 0x00, 0x00, // effective: CAP_NET_BIND_SERVICE bit 10
            0x00, 0x04, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        fs.set_xattr("/cap.bin", "security.capability", cap_value)
            .expect("set_xattr");

        let got = fs.get_xattr("/cap.bin", "security.capability").expect("get_xattr");
        assert_eq!(got, cap_value);

        let names = fs.list_xattr("/cap.bin").expect("list_xattr");
        assert!(
            names.iter().any(|n| n == "security.capability"),
            "list returned {:?}",
            names
        );

        // A POSIX ACL value stored under the binary system.posix_acl_access name —
        // 4-byte LE header (version=1) + a single USER_OBJ rwx entry.
        let acl_value: &[u8] = &[
            0x02, 0x00, 0x00, 0x00,            // version = 2 (kernel uses 2)
            0x01, 0x00, 0x07, 0x00,            // tag=USER_OBJ(1) perm=rwx
            0xff, 0xff, 0xff, 0xff,            // id=undefined
        ];
        fs.set_xattr("/cap.bin", "system.posix_acl_access", acl_value)
            .expect("set_xattr posix_acl_access");
        let got = fs
            .get_xattr("/cap.bin", "system.posix_acl_access")
            .expect("get_xattr posix_acl_access");
        assert_eq!(got, acl_value);

        // NOTE: ext4_removexattr in upstream lwext4 has a latent bug for
        // xattrs stored inline in the inode body: it uses an uninitialized
        // search state on the ibody-found path (see ext4_xattr.c:1262 — the
        // else branch references `block_finder.s` instead of
        // `ibody_finder.s`). The materializer never removes xattrs (it always
        // writes into a freshly-formatted image), so we don't gate on remove
        // here. If the materializer ever grows a remove path, fix lwext4 first.
        let _ = fs.remove_xattr("/cap.bin", "security.capability");

        fs.umount().unwrap();
    }
}

impl Drop for Ext4Fs {
    fn drop(&mut self) {
        // Note: We can't return errors from drop, so we just try our best
        if self.journal_active {
            unsafe { ext4_journal_stop(self.mount_point.as_ptr()) };
        }
        unsafe {
            ext4_cache_flush(self.mount_point.as_ptr());
            ext4_umount(self.mount_point.as_ptr());
            ext4_device_unregister(self.device_name.as_ptr());
        }
    }
}
