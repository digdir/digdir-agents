//! How large a new Agent's Sandbox should be on this host.
//!
//! The root filesystem's capacity is a ceiling, not a reservation: the disk image is sparse and
//! grows into the host's free space as the guest writes. A ceiling above what the host has free
//! lets a busy agent fill the host's disk, so the recommendation stays within half of it.

use std::path::Path;

const GIB: u64 = 1024 * 1024 * 1024;
/// The least disk a recommendation offers, enough for the base image's writes and a checkout.
const LEAST_DISK_GIB: u64 = 16;
/// Below this much free space, the recommendation comes with a warning.
pub(super) const LOW_DISK_GIB: u64 = 32;

/// A Sandbox's CPUs, memory and root filesystem capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Size {
    pub(super) cpus: u64,
    pub(super) memory_gib: u64,
    pub(super) disk_gib: u64,
}

impl Size {
    /// A smaller Size for a host that runs several agents or other heavy work: half of this one,
    /// with at least one CPU, 2 GiB of memory and 8 GiB of disk.
    pub(super) fn smaller(self) -> Self {
        Self {
            cpus: (self.cpus / 2).max(1),
            memory_gib: (self.memory_gib / 2).max(2),
            disk_gib: (self.disk_gib / 2).max(8),
        }
    }

    pub(super) fn describe(self) -> String {
        format!(
            "{} {}, {} GiB memory, {} GiB disk",
            self.cpus,
            if self.cpus == 1 { "CPU" } else { "CPUs" },
            self.memory_gib,
            self.disk_gib
        )
    }
}

/// What this host has to share among its agents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Host {
    pub(super) cpus: u64,
    pub(super) memory_gib: u64,
    /// Free space on the volume that holds agentd's state, where Sandbox disks live.
    pub(super) free_disk_gib: u64,
}

impl Host {
    /// Reads this host's CPUs and memory, and the free space on the volume holding `state`.
    /// Returns `None` when any of them cannot be read.
    pub(super) fn measure(state: &Path) -> Option<Self> {
        let cpus = std::thread::available_parallelism().ok()?.get();
        Some(Self {
            cpus: u64::try_from(cpus).ok()?,
            memory_gib: memory_bytes()? / GIB,
            free_disk_gib: free_bytes(state)? / GIB,
        })
    }

    /// Half of the host's cores, a quarter of its memory and half of its free disk, within
    /// `most`, and never less than one CPU, 2 GiB of memory or [`LEAST_DISK_GIB`] of disk.
    pub(super) fn recommend(self, most: Size) -> Size {
        Size {
            cpus: (self.cpus / 2).clamp(1, most.cpus.max(1)),
            memory_gib: (self.memory_gib / 4).clamp(2, most.memory_gib.max(2)),
            disk_gib: (self.free_disk_gib / 2).clamp(LEAST_DISK_GIB, most.disk_gib.max(LEAST_DISK_GIB)),
        }
    }
}

fn memory_bytes() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        return String::from_utf8_lossy(&output.stdout).trim().parse().ok();
    }
    parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_meminfo(meminfo: &str) -> Option<u64> {
    let kibibytes: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    kibibytes.checked_mul(1024)
}

/// Free space on the volume holding `path`, or its closest existing ancestor, from POSIX `df`.
fn free_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|ancestor| ancestor.exists())?;
    let output = std::process::Command::new("df")
        .arg("-Pk")
        .arg(existing)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_df(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

/// The available space in `df -Pk` output: the fourth column of its second line, in KiB.
fn parse_df(output: &str) -> Option<u64> {
    let available: u64 = output.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    available.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    const MOST: Size = Size {
        cpus: 4,
        memory_gib: 8,
        disk_gib: 64,
    };

    #[test]
    fn a_large_host_gets_the_usual_size_and_a_small_disk_caps_the_ceiling() {
        let roomy = Host {
            cpus: 16,
            memory_gib: 64,
            free_disk_gib: 500,
        };
        assert_eq!(roomy.recommend(MOST), MOST);

        let laptop = Host {
            cpus: 10,
            memory_gib: 32,
            free_disk_gib: 31,
        };
        assert_eq!(
            laptop.recommend(MOST),
            Size {
                cpus: 4,
                memory_gib: 8,
                disk_gib: 16
            }
        );

        let half_full = Host {
            cpus: 8,
            memory_gib: 16,
            free_disk_gib: 80,
        };
        assert_eq!(half_full.recommend(MOST).disk_gib, 40, "half of the free space");
    }

    #[test]
    fn a_tiny_host_still_gets_a_usable_size() {
        let tiny = Host {
            cpus: 1,
            memory_gib: 4,
            free_disk_gib: 3,
        };
        assert_eq!(
            tiny.recommend(MOST),
            Size {
                cpus: 1,
                memory_gib: 2,
                disk_gib: LEAST_DISK_GIB
            }
        );
        assert_eq!(
            tiny.recommend(MOST).smaller(),
            Size {
                cpus: 1,
                memory_gib: 2,
                disk_gib: 8
            }
        );
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(MOST.describe(), "4 CPUs, 8 GiB memory, 64 GiB disk");
        assert_eq!(MOST.smaller().describe(), "2 CPUs, 4 GiB memory, 32 GiB disk");
        assert_eq!(
            Size {
                cpus: 1,
                memory_gib: 2,
                disk_gib: 8
            }
            .describe(),
            "1 CPU, 2 GiB memory, 8 GiB disk"
        );
    }

    #[test]
    fn df_and_meminfo_output_is_read() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                  /dev/disk3s1 482797652 421502152 32505536 93% /System/Volumes/Data\n";
        assert_eq!(parse_df(df), Some(32_505_536 * 1024));
        assert_eq!(parse_df("Filesystem\n"), None);
        let meminfo = "MemTotal:       16314772 kB\nMemFree:         1000 kB\n";
        assert_eq!(parse_meminfo(meminfo), Some(16_314_772 * 1024));
        assert_eq!(parse_meminfo("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn this_host_can_be_measured() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let host = Host::measure(&directory.path().join("not").join("yet")).expect("measured");
        assert!(host.cpus >= 1 && host.memory_gib >= 1, "{host:?}");
    }
}
