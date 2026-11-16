//! Free space on the host's filesystems, and when it is too little.

use std::{fmt, io, path::Path, str::FromStr};

use ::sandbox::ByteQuantity;

use crate::Error;

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

/// How little free space agentd warns about on the filesystem holding the sandboxes' disks: a
/// share of the filesystem, such as `5%`, or a size, such as `20Gi`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LowDiskThreshold {
    /// Hundredths of a percent of the drive's size, more than none and less than all of it.
    Share(u16),
    /// A size, in the quantities manifests use.
    Size(ByteQuantity),
}

impl LowDiskThreshold {
    /// The free space, in bytes, below which a drive of `total` bytes is low.
    #[must_use]
    pub fn bytes(self, total: u64) -> u64 {
        match self {
            Self::Share(hundredths) => {
                let bytes = u128::from(total) * u128::from(hundredths) / 10_000;
                u64::try_from(bytes).unwrap_or(u64::MAX)
            }
            Self::Size(size) => size.bytes(),
        }
    }

    /// Whether `space` has less free than this threshold allows.
    #[must_use]
    pub fn is_low(self, space: DiskSpace) -> bool {
        space.available < self.bytes(space.total)
    }
}

impl Default for LowDiskThreshold {
    fn default() -> Self {
        Self::Share(500)
    }
}

impl FromStr for LowDiskThreshold {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            Error::Invalid(format!(
                "{value:?} is not a share of the drive below 100%, such as 5% or 2.5%, or a size, such as 20Gi"
            ))
        };
        let Some(percent) = value.strip_suffix('%') else {
            return value.parse().map(Self::Size).map_err(|_| invalid());
        };
        let (whole, fraction) = match percent.split_once('.') {
            Some((_, "")) => return Err(invalid()),
            Some(parts) => parts,
            None => (percent, ""),
        };
        let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        if whole.is_empty() || whole.len() > 2 || fraction.len() > 2 || !digits(whole) || !digits(fraction) {
            return Err(invalid());
        }
        let whole = whole.parse::<u16>().map_err(|_| invalid())?;
        let fraction = format!("{fraction:0<2}").parse::<u16>().map_err(|_| invalid())?;
        match whole * 100 + fraction {
            0 => Err(invalid()),
            hundredths => Ok(Self::Share(hundredths)),
        }
    }
}

impl fmt::Display for LowDiskThreshold {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Share(hundredths) if hundredths % 100 == 0 => write!(formatter, "{}%", hundredths / 100),
            Self::Share(hundredths) => {
                let fraction = format!("{:02}", hundredths % 100);
                write!(formatter, "{}.{}%", hundredths / 100, fraction.trim_end_matches('0'))
            }
            Self::Size(size) => write!(formatter, "{size}"),
        }
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
    use super::{DiskSpace, LowDiskThreshold};

    const GIB: u64 = 1 << 30;

    fn threshold(value: &str) -> LowDiskThreshold {
        value.parse().unwrap_or_else(|error| panic!("{value:?}: {error}"))
    }

    #[test]
    fn a_threshold_is_a_share_of_the_drive_or_a_size() {
        let drive = 1_000 * GIB;
        assert_eq!(threshold("5%").bytes(drive), 50 * GIB);
        assert_eq!(threshold("2.5%").bytes(drive), 25 * GIB);
        assert_eq!(threshold("0.25%").bytes(drive), 5 * GIB / 2);
        assert_eq!(threshold("20Gi").bytes(drive), 20 * GIB);
        assert_eq!(threshold("20G").bytes(drive), 20_000_000_000);
        assert_eq!(LowDiskThreshold::default(), threshold("5%"));
        for written in ["5%", "2.5%", "0.25%", "20Gi"] {
            assert_eq!(threshold(written).to_string(), written);
        }
    }

    #[test]
    fn a_threshold_that_is_no_share_below_all_of_the_drive_or_no_size_is_refused() {
        for invalid in [
            "", "%", "0%", "100%", "150%", "-5%", "5.%", ".5%", "1.255%", "5 %", "five", "0", "-1Gi",
        ] {
            assert!(
                invalid.parse::<LowDiskThreshold>().is_err(),
                "{invalid:?} should be refused"
            );
        }
    }

    #[test]
    fn a_drive_is_low_below_the_threshold() {
        let five_percent = threshold("5%");
        let space = |available| DiskSpace {
            available,
            total: 100 * GIB,
        };
        assert!(five_percent.is_low(space(5 * GIB - 1)));
        assert!(!five_percent.is_low(space(5 * GIB)));
    }

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

    /// A home where no sandbox has been created yet has no sandboxes folder, and its filesystem
    /// must still be measured.
    #[test]
    fn a_folder_not_created_yet_is_measured_on_the_filesystem_it_would_be_created_on() {
        let home = tempfile::TempDir::new().expect("temporary home");
        let sandboxes = home.path().join("microsandbox").join("runtime").join("sandboxes");

        let existing = DiskSpace::of(home.path()).expect("free space of the home");

        let space = DiskSpace::of(&sandboxes);
        assert!(
            matches!(space, Ok(space) if space.total == existing.total),
            "the home's filesystem, {existing}, should be measured, not {space:?}"
        );
    }
}
