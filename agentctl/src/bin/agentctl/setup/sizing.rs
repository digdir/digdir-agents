//! Sizes offered for a new agent, measured against the host so that a preset never asks for more
//! than the machine can spare.

use std::{num::NonZeroU64, path::Path};

use agent::{Error, ResourcesManifestSpec, catalog::Size};
use sandbox::{ByteQuantity, CpuQuantity, RootFilesystem};

const GIBIBYTE: u64 = 1 << 30;

/// The smallest root filesystem a preset sets, whatever the free space.
const MINIMUM_DISK_GIB: u64 = 16;

/// What the host has to give: processors, memory and free disk space where the control-plane home
/// lives. What cannot be measured is `None` and limits nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Host {
    pub(super) cpus: u64,
    pub(super) memory_gib: Option<u64>,
    pub(super) free_disk_gib: Option<u64>,
}

impl Host {
    /// Measures this machine, with the free space of the filesystem holding `storage`.
    pub(super) fn measure(storage: &Path) -> Self {
        Self {
            cpus: std::thread::available_parallelism().map_or(1, |cpus| u64::try_from(cpus.get()).unwrap_or(1)),
            memory_gib: total_memory_bytes().map(|bytes| bytes / GIBIBYTE),
            free_disk_gib: free_disk_bytes(storage).map(|bytes| bytes / GIBIBYTE),
        }
    }

    /// Why the host is smaller than an archetype's minimum, if it is.
    pub(super) fn shortfall(&self, minimum: &Whole) -> Option<String> {
        let mut short = Vec::new();
        if self.cpus < minimum.cpus {
            short.push(format!("{} CPUs of {}", self.cpus, minimum.cpus));
        }
        if self.memory_gib.is_some_and(|memory| memory < minimum.memory_gib) {
            short.push(format!(
                "{} GiB memory of {}",
                self.memory_gib.unwrap_or_default(),
                minimum.memory_gib
            ));
        }
        if self.free_disk_gib.is_some_and(|free| free < minimum.disk_gib) {
            short.push(format!(
                "{} GiB free disk of {}",
                self.free_disk_gib.unwrap_or_default(),
                minimum.disk_gib
            ));
        }
        (!short.is_empty()).then(|| format!("this machine has {}", short.join(", ")))
    }
}

/// A size in whole processors and gibibytes, as presets are offered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Whole {
    pub(super) cpus: u64,
    pub(super) memory_gib: u64,
    pub(super) disk_gib: u64,
}

impl Whole {
    /// Rounds a catalog size up to whole processors and gibibytes.
    pub(super) const fn of(size: Size) -> Self {
        Self {
            cpus: size.cpu.millicpus().div_ceil(1000),
            memory_gib: size.memory.bytes().div_ceil(GIBIBYTE),
            disk_gib: size.disk.bytes().div_ceil(GIBIBYTE),
        }
    }

    /// Reads a size from manifest resources, rounded up.
    pub(super) const fn of_resources(resources: &ResourcesManifestSpec) -> Self {
        Self {
            cpus: resources.cpu.millicpus().div_ceil(1000),
            memory_gib: resources.memory.bytes().div_ceil(GIBIBYTE),
            disk_gib: resources.root_filesystem.capacity().bytes().div_ceil(GIBIBYTE),
        }
    }

    /// Sets this size in `resources`, keeping the root filesystem's mode and whether the agent
    /// runs sandboxes of its own.
    pub(super) fn apply(self, resources: &mut ResourcesManifestSpec) -> Result<(), Error> {
        let invalid = |what: &str| Error::Invalid(format!("{what} must be positive"));
        resources.cpu =
            CpuQuantity::from_millicpus(NonZeroU64::new(self.cpus.saturating_mul(1000)).ok_or_else(|| invalid("cpu"))?);
        resources.memory = ByteQuantity::from_gibibytes(self.memory_gib).map_err(|_| invalid("memory"))?;
        let capacity = ByteQuantity::from_gibibytes(self.disk_gib).map_err(|_| invalid("disk"))?;
        resources.root_filesystem = RootFilesystem::new(capacity, resources.root_filesystem.mode());
        Ok(())
    }

    pub(super) fn describe(self) -> String {
        format!(
            "{} CPUs, {} GiB memory, {} GiB disk",
            self.cpus, self.memory_gib, self.disk_gib
        )
    }
}

/// The small, medium and large sizes for an archetype on `host`. Medium is what the archetype
/// recommends, small half of it and large twice it, each kept within what the host can spare and
/// never below the archetype's minimum.
pub(super) fn presets(host: &Host, minimum: Whole, recommended: Whole) -> [Whole; 3] {
    let within = |value: u64, cap: Option<u64>, floor: u64| cap.map_or(value, |cap| value.min(cap)).max(floor);
    let medium = Whole {
        cpus: within(recommended.cpus, Some(host.cpus / 2), minimum.cpus),
        memory_gib: within(
            recommended.memory_gib,
            host.memory_gib.map(|memory| memory / 4),
            minimum.memory_gib,
        ),
        disk_gib: within(
            recommended.disk_gib,
            host.free_disk_gib.map(|free| free / 2),
            minimum.disk_gib.max(MINIMUM_DISK_GIB),
        ),
    };
    let small = Whole {
        cpus: (medium.cpus / 2).max(minimum.cpus),
        memory_gib: (medium.memory_gib / 2).max(minimum.memory_gib),
        disk_gib: minimum.disk_gib.max(MINIMUM_DISK_GIB).min(medium.disk_gib),
    };
    let large = Whole {
        cpus: within(recommended.cpus * 2, Some(host.cpus * 3 / 4), medium.cpus),
        memory_gib: within(
            recommended.memory_gib * 2,
            host.memory_gib.map(|memory| memory / 2),
            medium.memory_gib,
        ),
        disk_gib: within(
            recommended.disk_gib * 2,
            host.free_disk_gib.map(|free| free * 3 / 4),
            medium.disk_gib,
        ),
    };
    [small, medium, large]
}

fn total_memory_bytes() -> Option<u64> {
    let refresh = sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram());
    let memory = sysinfo::System::new_with_specifics(refresh).ok()?.total_memory();
    (memory > 0).then_some(memory)
}

/// The space left on the disk holding `storage`, or its nearest existing folder, as the
/// control-plane home may not exist yet: the disk mounted deepest above it.
fn free_disk_bytes(storage: &Path) -> Option<u64> {
    let existing = storage.ancestors().find(|path| path.is_dir())?.canonicalize().ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks
        .iter()
        .filter(|disk| existing.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().components().count())
        .map(sysinfo::Disk::available_space)
}

#[cfg(test)]
mod tests {
    use super::{Host, Whole, free_disk_bytes, presets, total_memory_bytes};

    const MINIMUM: Whole = Whole {
        cpus: 2,
        memory_gib: 6,
        disk_gib: 40,
    };
    const RECOMMENDED: Whole = Whole {
        cpus: 4,
        memory_gib: 12,
        disk_gib: 80,
    };

    #[test]
    fn a_roomy_host_gets_the_recommended_size_as_medium() {
        let host = Host {
            cpus: 16,
            memory_gib: Some(64),
            free_disk_gib: Some(1000),
        };
        let [small, medium, large] = presets(&host, MINIMUM, RECOMMENDED);
        assert_eq!(medium, RECOMMENDED);
        assert_eq!(
            small,
            Whole {
                cpus: 2,
                memory_gib: 6,
                disk_gib: 40
            }
        );
        assert_eq!(
            large,
            Whole {
                cpus: 8,
                memory_gib: 24,
                disk_gib: 160
            }
        );
    }

    #[test]
    fn a_small_host_caps_presets_but_never_below_the_minimum() {
        let host = Host {
            cpus: 4,
            memory_gib: Some(16),
            free_disk_gib: Some(60),
        };
        let [small, medium, large] = presets(&host, MINIMUM, RECOMMENDED);
        assert_eq!(
            medium,
            Whole {
                cpus: 2,
                memory_gib: 6,
                disk_gib: 40
            }
        );
        assert_eq!(small, medium);
        assert_eq!(
            large,
            Whole {
                cpus: 3,
                memory_gib: 8,
                disk_gib: 45
            }
        );
        assert!(host.shortfall(&MINIMUM).is_none());
        let tiny = Host {
            cpus: 1,
            memory_gib: Some(4),
            free_disk_gib: None,
        };
        assert!(
            tiny.shortfall(&MINIMUM)
                .is_some_and(|reason| reason.contains("1 CPUs of 2"))
        );
    }

    #[test]
    fn this_machine_has_memory_and_its_disks_measure_without_failing() {
        assert!(total_memory_bytes().is_some_and(|bytes| bytes > 0));
        // A container may not list the disk its root is on, which leaves the free space unknown.
        let _ = free_disk_bytes(&std::env::temp_dir());
    }
}
