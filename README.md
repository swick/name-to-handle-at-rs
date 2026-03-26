# name-to-handle-at-rs

Safe, low-level Rust bindings for Linux `name_to_handle_at` and `open_by_handle_at` system calls.

## Overview

This crate provides Rust bindings for the Linux-specific `name_to_handle_at(2)` and `open_by_handle_at(2)` system calls.

These syscalls allow you to obtain a file handle for a file and later reopen that file using the handle, even if the file has been renamed or moved within the same filesystem. It also allows you to obtain a reference to a file which is stable across reboots (unlike the common `st_dev+st_ino` reference).

## References

Consult the man page for usage, restrictions, etc.

- [`name_to_handle_at(2)` man page](https://man7.org/linux/man-pages/man2/name_to_handle_at.2.html)
- [`open_by_handle_at(2)` man page](https://man7.org/linux/man-pages/man2/open_by_handle_at.2.html)

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
name-to-handle-at = "0.1"
```

### Basic Example

```rust
use std::fs::File;
use name_to_handle_at::{name_to_handle_at, open_by_handle_at, MountId, AT_EMPTY_PATH};

fn main() -> std::io::Result<()> {
    let dir = File::open("/")?;

    // Get a file handle for the root directory
    let (handle, mount_id) = name_to_handle_at(&dir, std::path::Path::new(""), AT_EMPTY_PATH)?;

    // mount_id is always returned, either Reusable or Unique
    match mount_id {
        MountId::Reusable(id) => println!("Reusable mount ID: {}", id),
        MountId::Unique(id) => println!("Unique mount ID: {}", id),
    }

    // Later, reopen the file using the handle
    // Note: Requires CAP_DAC_READ_SEARCH capability
    let reopened = open_by_handle_at(&dir, &handle, libc::O_RDONLY)?;

    Ok(())
}
```

### With Unique Mount ID (Linux 6.8+)

```rust
use std::fs::File;
use name_to_handle_at::{name_to_handle_at, MountId, AT_EMPTY_PATH, AT_HANDLE_MNT_ID_UNIQUE};

fn main() -> std::io::Result<()> {
    let dir = File::open("/")?;

    let (handle, mount_id) = name_to_handle_at(
        &dir,
        std::path::Path::new(""),
        AT_EMPTY_PATH | AT_HANDLE_MNT_ID_UNIQUE
    )?;

    // With AT_HANDLE_MNT_ID_UNIQUE, mount_id is MountId::Unique(u64)
    match mount_id {
        MountId::Unique(id) => println!("Unique mount ID: {}", id),
        MountId::Reusable(id) => println!("Reusable mount ID: {}", id),
    }

    Ok(())
}
```

## Available Flags

### For `name_to_handle_at`:

- `AT_EMPTY_PATH` - Allow empty pathname to refer to the file descriptor itself
- `AT_SYMLINK_FOLLOW` - Follow symbolic links when resolving the path
- `AT_HANDLE_FID` - Return a file identifier instead of a file handle (Linux 6.5+)
- `AT_HANDLE_MNT_ID_UNIQUE` - Return unique mount ID that won't be reused (Linux 6.8+)

### For `open_by_handle_at`:

Use standard libc flags:
- `libc::O_RDONLY` - Open read-only
- `libc::O_WRONLY` - Open write-only
- `libc::O_RDWR` - Open read-write
- `libc::O_CLOEXEC` - Set close-on-exec flag
- `libc::O_PATH` - Open as path descriptor (no read/write access)
- `libc::O_NONBLOCK` - Open in non-blocking mode

## Testing

Tests require a filesystem that supports file handles (not tmpfs or overlayfs) and appropriate capabilities:

```bash
# Run as root (simplest)
sudo cargo test

# Or with specific capability
sudo setpriv --reuid=$(id -u) --regid=$(id -g) --clear-groups --ambient-caps=+dac_read_search cargo test
```

Note: Tests will gracefully skip if run without proper capabilities or on unsupported filesystems.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSES/MIT.txt](LICENSES/MIT.txt) or http://opensource.org/licenses/MIT)

at your option.