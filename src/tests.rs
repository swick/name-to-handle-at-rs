// SPDX-FileCopyrightText: 2026 The name-to-handle-at-rs authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

fn skip_if_unsupported<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(val) => Ok(Some(val)),
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {
            eprintln!("Syscall not supported on this system, skipping test");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn skip_if_unsupported_or_einval<T>(result: io::Result<T>, feature: &str) -> io::Result<Option<T>> {
    match result {
        Ok(val) => Ok(Some(val)),
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {
            eprintln!("Syscall not supported on this system, skipping test");
            Ok(None)
        }
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
            eprintln!(
                "{} not supported (requires newer kernel), skipping test",
                feature
            );
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn skip_if_unsupported_or_perm<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(val) => Ok(Some(val)),
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {
            eprintln!("Syscall not supported on this system, skipping test");
            Ok(None)
        }
        Err(e) if e.raw_os_error() == Some(libc::EPERM) => {
            eprintln!("Operation requires CAP_DAC_READ_SEARCH capability, skipping test");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn skip_if_unsupported_or_not_found<T>(result: io::Result<T>, path: &str) -> io::Result<Option<T>> {
    match result {
        Ok(val) => Ok(Some(val)),
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {
            eprintln!("Syscall not supported on this system, skipping test");
            Ok(None)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("{} not found, skipping test", path);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

#[test]
fn test_name_to_handle_at_basic() -> io::Result<()> {
    let dir = File::open("/tmp")?;
    let result = name_to_handle_at(&dir, std::path::Path::new(""), AT_EMPTY_PATH);

    if let Some((handle, mount_id)) = skip_if_unsupported(result)? {
        assert!(!handle.handle.is_empty());
        // Without AT_HANDLE_MNT_ID_UNIQUE, we get a 32-bit Reusable mount ID
        assert!(matches!(mount_id, MountId::Reusable(_)));
    }
    Ok(())
}

#[test]
fn test_name_to_handle_at_with_unique_mount_id() -> io::Result<()> {
    let dir = File::open("/tmp")?;
    let result = name_to_handle_at(
        &dir,
        std::path::Path::new(""),
        AT_EMPTY_PATH | AT_HANDLE_MNT_ID_UNIQUE,
    );

    if let Some((handle, mount_id)) =
        skip_if_unsupported_or_einval(result, "AT_HANDLE_MNT_ID_UNIQUE")?
    {
        assert!(!handle.handle.is_empty());
        // With AT_HANDLE_MNT_ID_UNIQUE, we get a 64-bit Unique mount ID
        assert!(matches!(mount_id, MountId::Unique(_)));
    }
    Ok(())
}

#[test]
fn test_name_to_handle_at_with_path() -> io::Result<()> {
    let dir = File::open("/")?;
    let result = name_to_handle_at(&dir, std::path::Path::new("tmp"), 0);

    if let Some((handle, _)) = skip_if_unsupported_or_not_found(result, "/tmp")? {
        assert!(!handle.handle.is_empty());
    }
    Ok(())
}

#[test]
fn test_open_by_handle_at_roundtrip() -> io::Result<()> {
    let dir = File::open("/tmp")?;

    // Get a handle for the root directory
    let (handle, _) = match skip_if_unsupported(name_to_handle_at(
        &dir,
        std::path::Path::new(""),
        AT_EMPTY_PATH,
    ))? {
        Some(h) => h,
        None => return Ok(()),
    };

    // Try to reopen using the handle
    let result = open_by_handle_at(&dir, &handle, libc::O_RDONLY);

    if let Some(fd) = skip_if_unsupported_or_perm(result)? {
        // Verify we got a valid file descriptor
        assert!(fd.as_raw_fd() >= 0);
    }
    Ok(())
}

#[test]
fn test_open_by_handle_at_with_o_path() -> io::Result<()> {
    let dir = File::open("/tmp")?;

    let (handle, _) = match skip_if_unsupported(name_to_handle_at(
        &dir,
        std::path::Path::new(""),
        AT_EMPTY_PATH,
    ))? {
        Some(h) => h,
        None => return Ok(()),
    };

    // Try to open with O_PATH flag (doesn't require read/write permission)
    let result = open_by_handle_at(&dir, &handle, libc::O_PATH);

    if let Some(fd) = skip_if_unsupported_or_perm(result)? {
        assert!(fd.as_raw_fd() >= 0);
    }
    Ok(())
}
