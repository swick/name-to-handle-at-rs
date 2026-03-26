// SPDX-FileCopyrightText: 2026 The name-to-handle-at-rs authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Safe but unopinionated, low-level Rust wrappers for the `name_to_handle_at` and
//! `open_by_handle_at` Linux syscalls.
//!
//! These system calls provide a way to obtain a file handle for a file and later reopen
//! that file using the handle, even if the file has been renamed or moved. This is useful
//! for implementing userspace file indexing, backup systems, and other applications that
//! need stable file references.
//!
//! # Example
//!
//! ```no_run
//! use std::fs::File;
//! use name_to_handle_at::{name_to_handle_at, open_by_handle_at, MountId, AT_EMPTY_PATH};
//!
//! # fn main() -> std::io::Result<()> {
//! let file = File::open("/")?;
//!
//! // Get a handle for the root directory
//! let (handle, mount_id) = name_to_handle_at(&file, std::path::Path::new(""), AT_EMPTY_PATH)?;
//!
//! match mount_id {
//!     MountId::Reusable(id) => println!("Mount ID: {}", id),
//!     MountId::Unique(id) => println!("Unique Mount ID: {}", id),
//! }
//!
//! // Later, reopen the file using the handle
//! // Warning: requires CAP_DAC_READ_SEARCH
//! let reopened = open_by_handle_at(&file, &handle, libc::O_RDONLY)?;
//! # Ok(())
//! # }
//! ```

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic;

#[cfg(test)]
mod tests;

// Flags for name_to_handle_at()

/// Follow symbolic links when resolving the path.
///
/// By default, symbolic links are not followed. This flag is equivalent to
/// `libc::AT_SYMLINK_FOLLOW`.
pub const AT_SYMLINK_FOLLOW: libc::c_int = 0x400;

/// Allow empty pathname to refer to the file descriptor itself.
///
/// When this flag is specified with an empty path, the file handle is obtained
/// for the file referenced by the `fd` parameter. This flag is equivalent to
/// `libc::AT_EMPTY_PATH`.
pub const AT_EMPTY_PATH: libc::c_int = 0x1000;

/// Return a file identifier instead of a file handle.
///
/// Requires Linux 6.5 or later. The caller indicates that the returned handle is
/// needed to identify the filesystem object, and not for opening the file later.
pub const AT_HANDLE_FID: libc::c_int = 0x200;

/// Return a unique mount ID in the mount_id field.
///
/// Requires Linux 6.8 or later. When this flag is specified, [`name_to_handle_at`]
/// returns [`MountId::Unique`] instead of [`MountId::Reusable`].
pub const AT_HANDLE_MNT_ID_UNIQUE: libc::c_int = 0x001;

#[repr(u8)]
#[derive(PartialEq)]
enum Supported {
    Unknown = 0,
    Yes = 1,
    No = 2,
}

struct AtomicSupported(atomic::AtomicU8);

impl AtomicSupported {
    const fn new(supported: Supported) -> Self {
        Self(atomic::AtomicU8::new(supported as u8))
    }

    fn load(&self) -> Supported {
        match self.0.load(atomic::Ordering::Relaxed) {
            0 => Supported::Unknown,
            1 => Supported::Yes,
            2 => Supported::No,
            _ => panic!(),
        }
    }

    fn store(&self, supported: Supported) {
        self.0.store(supported as u8, atomic::Ordering::Relaxed);
    }
}

trait IsMinusOne {
    fn is_minus_one(&self) -> bool;
}

macro_rules! impl_is_minus_one {
    ($($t:ident)*) => ($(impl IsMinusOne for $t {
        fn is_minus_one(&self) -> bool {
            *self == -1
        }
    })*)
}

impl_is_minus_one! { i8 i16 i32 i64 isize }

fn cvt<T: IsMinusOne>(t: T) -> io::Result<T> {
    if t.is_minus_one() {
        Err(io::Error::last_os_error())
    } else {
        Ok(t)
    }
}

#[repr(C)]
#[derive(Default)]
struct __IncompleteArrayField<T>(::std::marker::PhantomData<T>, [T; 0]);

#[repr(C)]
#[derive(Default)]
struct file_handle {
    handle_bytes: libc::c_uint,
    handle_type: libc::c_int,
    f_handle: __IncompleteArrayField<libc::c_uchar>,
}

/// A mount ID identifying the filesystem containing a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountId {
    /// A traditional 32-bit mount ID that may be reused after the filesystem is unmounted. Returned
    /// when [`AT_HANDLE_MNT_ID_UNIQUE`] is NOT used.
    Reusable(u32),
    /// A unique 64-bit mount ID (Linux 6.8+, same as returned by statx(2) with STATX_MNT_ID_UNIQUE)
    /// that is guaranteed to never be reused across the entire lifetime of the system. Returned when
    /// [`AT_HANDLE_MNT_ID_UNIQUE`] is used.
    Unique(u64),
}

/// A file handle that uniquely identifies a file within a filesystem.
///
/// This structure contains an opaque filesystem-specific handle that can be used
/// to reopen a file via [`open_by_handle_at`], or to get a stable reference to
/// a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHandle {
    pub handle_type: i32,
    pub handle: Vec<u8>,
}

/// Obtains a file handle and mount ID for a file.
///
/// This function returns a [`FileHandle`] that can be used to reopen the file later
/// via [`open_by_handle_at`], and a [`MountId`] identifying the filesystem.
///
/// # Arguments
///
/// * `fd` - A file descriptor referring to a directory. The path is resolved relative to this directory.
///   Use [`AT_EMPTY_PATH`] with an empty path to get a handle for the file descriptor itself.
/// * `path` - Path to the file, relative to `fd`. Can be empty if [`AT_EMPTY_PATH`] is specified.
/// * `flags` - Control flags. Can be a combination of:
///   - [`AT_EMPTY_PATH`] - Allow empty path to refer to `fd` itself
///   - [`AT_SYMLINK_FOLLOW`] - Follow symbolic links
///   - [`AT_HANDLE_FID`] - Return a file identifier instead of a handle (Linux 6.5+)
///   - [`AT_HANDLE_MNT_ID_UNIQUE`] - Return unique 64-bit mount ID (Linux 6.8+)
///
/// # Returns
///
/// A tuple of `(FileHandle, MountId)`:
/// - `FileHandle` - The file handle for the specified file
/// - `MountId` - Either `MountId::Reusable(u32)` or `MountId::Unique(u64)` depending on whether
///   [`AT_HANDLE_MNT_ID_UNIQUE`] was specified
///
/// # Errors
///
/// Returns an error if:
/// - The file does not exist
/// - The filesystem does not support file handles
/// - The system call is not supported
/// - Other I/O errors occur
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use name_to_handle_at::{name_to_handle_at, MountId, AT_EMPTY_PATH, AT_HANDLE_MNT_ID_UNIQUE};
///
/// # fn main() -> std::io::Result<()> {
/// let dir = File::open("/")?;
/// let (handle, mount_id) = name_to_handle_at(
///     &dir,
///     std::path::Path::new(""),
///     AT_EMPTY_PATH | AT_HANDLE_MNT_ID_UNIQUE
/// )?;
/// match mount_id {
///     MountId::Unique(id) => println!("Unique mount ID: {}", id),
///     MountId::Reusable(id) => println!("Reusable mount ID: {}", id),
/// }
/// # Ok(())
/// # }
/// ```
pub fn name_to_handle_at<Fd: AsFd>(
    fd: &Fd,
    path: &std::path::Path,
    flags: i32,
) -> io::Result<(FileHandle, MountId)> {
    static SUPPORTED: AtomicSupported = AtomicSupported::new(Supported::Unknown);

    let supported = SUPPORTED.load();
    if supported == Supported::No {
        return Err(io::Error::from_raw_os_error(libc::ENOSYS));
    }

    let mut handle = file_handle::default();
    let mut mount_id: u64 = 0;
    let mut path = path.as_os_str().as_bytes().to_owned();
    path.push(0);

    // SAFETY:
    // The name_to_handle_at syscall takes arguments which are mirrored here.
    // The raw fd comes from a valid fd via AsFd
    // The path is a valid, zero-terminated path.
    // The handle mirrors `struct file_handle` and is zero-initialized, this means file_handle.handle_bytes is also zero, so the allocated size for handle is correct.
    // The mount_id pointer is valid and can contain both "traditional" 32 bit and unique 64 bit mount ids.
    // The syscall handles arbitrary values of flags.
    #[allow(clippy::unnecessary_cast)]
    let err = cvt(unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            fd.as_fd().as_raw_fd() as libc::c_int,
            path.as_ptr() as *const libc::c_char,
            &raw mut handle as *mut file_handle,
            &raw mut mount_id as *mut u64 as *mut libc::c_int,
            flags as libc::c_int,
        ) as libc::c_int
    })
    .unwrap_err();

    if err.raw_os_error().unwrap() == libc::ENOSYS {
        SUPPORTED.store(Supported::No);
    } else if supported == Supported::Unknown {
        SUPPORTED.store(Supported::Yes);
    }

    if err.raw_os_error().unwrap() != libc::EOVERFLOW || handle.handle_bytes == 0 {
        return Err(err);
    }

    loop {
        let layout = Layout::new::<file_handle>();
        let buf_layout =
            Layout::array::<libc::c_uchar>(handle.handle_bytes.try_into().unwrap()).unwrap();
        let (layout, buf_offset) = layout.extend(buf_layout).unwrap();
        let layout = layout.pad_to_align();

        // SAFETY:
        // Layout has non-zero size because file_handle has non-zero size
        let buf = unsafe { alloc_zeroed(layout) };
        // SAFETY:
        // Constructing a Box from the newly allocated, zeroed memory is valid
        let mut new_handle: Box<file_handle> = unsafe { Box::from_raw(buf as _) };
        new_handle.handle_bytes = handle.handle_bytes;
        new_handle.handle_type = handle.handle_type;

        // SAFETY:
        // Same as the previous name_to_handle_at syscall, except...
        // new_handle.handle_bytes is bigger than zero, so the memory allocated for new_handle must be bigger by that amount.
        // The code above ensures we allocated the right size.
        #[allow(clippy::unnecessary_cast)]
        let res = cvt(unsafe {
            libc::syscall(
                libc::SYS_name_to_handle_at,
                fd.as_fd().as_raw_fd() as libc::c_int,
                path.as_ptr() as *const libc::c_char,
                &raw mut *new_handle as *mut file_handle,
                &raw mut mount_id as *mut u64 as *mut libc::c_int,
                flags as libc::c_int,
            ) as libc::c_int
        });

        handle.handle_bytes = new_handle.handle_bytes;
        handle.handle_type = new_handle.handle_type;
        // We leak this because the memory belongs to buf
        Box::leak(new_handle);

        match res {
            Err(e) if e.raw_os_error().unwrap() == libc::EOVERFLOW => (),
            Err(e) => {
                // SAFETY:
                // buf and layout are still valid, and we must deallocate the memory to avoid memory leaks
                unsafe { dealloc(buf, layout) };
                return Err(e);
            }
            Ok(_) => {
                let h = {
                    // SAFETY:
                    // buf_offset was created from the layout to point at the char[].
                    // We allocated enough size for handle_bytes via the layout.
                    // The [u8] is thus properly aligned and non-zero.
                    // f_handle goes out of scope before we deallocate.
                    let f_handle = unsafe {
                        std::slice::from_raw_parts(
                            buf.offset(buf_offset.try_into().unwrap()),
                            handle.handle_bytes.try_into().unwrap(),
                        )
                    };

                    FileHandle {
                        handle_type: handle.handle_type,
                        handle: f_handle.to_vec(),
                    }
                };

                // SAFETY:
                // buf and layout are still valid, and we must deallocate the memory to avoid memory leaks
                unsafe { dealloc(buf, layout) };

                let mount_id = if flags & AT_HANDLE_MNT_ID_UNIQUE != 0 {
                    MountId::Unique(mount_id)
                } else {
                    MountId::Reusable(mount_id as u32)
                };

                return Ok((h, mount_id));
            }
        }
    }
}

/// Opens a file using a file handle obtained from [`name_to_handle_at`].
///
/// This function reopens a file using its handle, regardless of whether the file
/// has been renamed or moved within the filesystem. The caller must have the
/// `CAP_DAC_READ_SEARCH` capability.
///
/// # Arguments
///
/// * `mount_fd` - A file descriptor for any file in the same mounted filesystem
///   as the file referenced by `handle`. Can be obtained by opening any file in
///   the filesystem, or by using the mount ID from [`name_to_handle_at`].
/// * `handle` - The file handle obtained from [`name_to_handle_at`]
/// * `flags` - File open flags, such as:
///   - `libc::O_RDONLY` - Open read-only
///   - `libc::O_WRONLY` - Open write-only
///   - `libc::O_RDWR` - Open read-write
///   - `libc::O_CLOEXEC` - Set close-on-exec flag
///   - `libc::O_PATH` - Open as path descriptor (no read/write access)
///   - `libc::O_NONBLOCK` - Open in non-blocking mode
///
/// # Returns
///
/// An [`OwnedFd`] for the opened file.
///
/// # Errors
///
/// Returns an error if:
/// - The caller lacks `CAP_DAC_READ_SEARCH` capability
/// - The handle is invalid or from a different filesystem
/// - The filesystem has been unmounted
/// - The system call is not supported
/// - Other I/O errors occur
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use name_to_handle_at::{name_to_handle_at, open_by_handle_at, AT_EMPTY_PATH};
///
/// # fn main() -> std::io::Result<()> {
/// let dir = File::open("/")?;
/// let (handle, _) = name_to_handle_at(&dir, std::path::Path::new(""), AT_EMPTY_PATH)?;
///
/// // Reopen the file
/// let fd = open_by_handle_at(&dir, &handle, libc::O_RDONLY)?;
/// # Ok(())
/// # }
/// ```
pub fn open_by_handle_at<Fd: AsFd>(
    mount_fd: &Fd,
    handle: &FileHandle,
    flags: i32,
) -> io::Result<OwnedFd> {
    static SUPPORTED: AtomicSupported = AtomicSupported::new(Supported::Unknown);

    let supported = SUPPORTED.load();
    if supported == Supported::No {
        return Err(io::Error::from_raw_os_error(libc::ENOSYS));
    }

    let handle_bytes: u16 = handle
        .handle
        .len()
        .try_into()
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let layout = Layout::new::<file_handle>();
    let buf_layout = Layout::array::<libc::c_uchar>(handle_bytes.into()).unwrap();
    let (layout, buf_offset) = layout.extend(buf_layout).unwrap();
    let layout = layout.pad_to_align();

    // SAFETY:
    // Layout has non-zero size because file_handle has non-zero size
    let buf = unsafe { alloc_zeroed(layout) };
    // SAFETY:
    // Constructing a Box from the newly allocated, zeroed memory is valid
    let mut new_handle: Box<file_handle> = unsafe { Box::from_raw(buf as _) };
    new_handle.handle_bytes = handle_bytes.into();
    new_handle.handle_type = handle.handle_type;

    // We leak this because the memory belongs to buf
    Box::leak(new_handle);

    // SAFETY:
    // buf_offset was created from the layout to point at the char[].
    // We allocated enough size for handle_bytes via the layout.
    unsafe {
        std::ptr::copy_nonoverlapping(
            handle.handle.as_ptr(),
            buf.offset(buf_offset.try_into().unwrap()),
            handle_bytes.into(),
        );
    }

    // SAFETY:
    // The open_by_handle_at syscall takes arguments which are mirrored here.
    // The raw fd comes from a valid fd via AsFd
    // The handle buf mirrors `struct file_handle` and is valid as per the code above.
    // The syscall handles arbitrary values of flags.
    #[allow(clippy::unnecessary_cast)]
    let res = cvt(unsafe {
        libc::syscall(
            libc::SYS_open_by_handle_at,
            mount_fd.as_fd().as_raw_fd() as libc::c_int,
            buf as *const file_handle,
            flags as libc::c_int,
        ) as libc::c_int
    });

    unsafe { dealloc(buf, layout) };

    if res.is_err() && res.as_ref().unwrap_err().raw_os_error().unwrap() == libc::ENOSYS {
        SUPPORTED.store(Supported::No);
    } else if supported == Supported::Unknown {
        SUPPORTED.store(Supported::Yes);
    }

    // SAFETY:
    // The result is either -1 with errno, or a valid fd.
    unsafe { Ok(OwnedFd::from_raw_fd(res? as libc::c_int)) }
}
