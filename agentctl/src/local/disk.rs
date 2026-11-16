//! Free space on the host's filesystems.

use std::{fmt, io, path::Path};

/// Below this many bytes free a filesystem is full, however small it is.
const FULL_BELOW_BYTES: u64 = 1 << 30;
/// Below this share of a filesystem free, in percent, it is full, however large it is.
const FULL_BELOW_PERCENT: u64 = 1;

/// The space on one host filesystem, in bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskSpace {
    /// What agentd may still write. Space the filesystem keeps for the system is not in it.
    pub available: u64,
    /// The size of the filesystem.
    pub total: u64,
}

impl DiskSpace {
    /// Reads the space on the filesystem holding `path`, which must exist. Blocks.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when it cannot tell.
    pub fn of(path: &Path) -> io::Result<Self> {
        query(path)
    }

    /// Whether so little is free that writes to the filesystem fail: under 1 GiB, or under 1% of
    /// a large one. A large filesystem counts as full sooner, since one sandbox can write
    /// gigabytes between a check and a failure.
    #[must_use]
    pub const fn is_full(self) -> bool {
        let share = self.total / 100 * FULL_BELOW_PERCENT;
        let threshold = if share > FULL_BELOW_BYTES {
            share
        } else {
            FULL_BELOW_BYTES
        };
        self.available < threshold
    }
}

impl fmt::Display for DiskSpace {
    /// Formats it as `512.0 MiB of 934.0 GiB free`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} of {} free",
            crate::progress::bytes(self.available),
            crate::progress::bytes(self.total)
        )
    }
}

#[cfg(unix)]
fn query(path: &Path) -> io::Result<DiskSpace> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(DiskSpace {
        available: stat.f_bavail.saturating_mul(stat.f_frsize),
        total: stat.f_blocks.saturating_mul(stat.f_frsize),
    })
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Windows reports free space only through GetDiskFreeSpaceExW, which has no safe wrapper"
)]
fn query(path: &Path) -> io::Result<DiskSpace> {
    use std::os::windows::ffi::OsStrExt as _;

    let wide = path.as_os_str().encode_wide().chain([0]).collect::<Vec<u16>>();
    let mut available = 0_u64;
    let mut total = 0_u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call, the first two out
    // pointers point at live `u64`s, and the third may be null.
    let succeeded = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut available,
            &raw mut total,
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(DiskSpace { available, total })
}

#[cfg(not(any(unix, windows)))]
fn query(_path: &Path) -> io::Result<DiskSpace> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::DiskSpace;

    const GIB: u64 = 1 << 30;

    #[test]
    fn a_disk_is_full_under_one_gibibyte_or_one_percent_free() {
        let full = |available, total| DiskSpace { available, total }.is_full();

        assert!(full(GIB - 1, 20 * GIB), "under 1 GiB on a small disk");
        assert!(!full(GIB, 20 * GIB), "1 GiB on a small disk");
        assert!(full(1_400_000_000, 934 * GIB), "1.4 GB on a large disk is under 1%");
        assert!(!full(10 * GIB, 934 * GIB), "over 1% of a large disk");
    }

    #[test]
    fn the_space_of_an_existing_directory_can_be_read() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let space = DiskSpace::of(directory.path()).expect("free space");

        assert!(space.total > 0);
        assert!(space.available <= space.total);
    }
}
