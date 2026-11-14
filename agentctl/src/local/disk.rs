//! Free space on the host's filesystems.

/// The space on one host filesystem, in bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskSpace {
    /// What agentd may still write. Space the filesystem keeps for the system is not in it.
    pub available: u64,
    /// The size of the filesystem.
    pub total: u64,
}
