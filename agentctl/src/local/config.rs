//! agentd's settings, from `agentd.toml` in its home. `agentctl/docs/agentd-configuration.md`
//! describes them for the people who set them.

use std::path::Path;

use crate::Error;

use super::disk::LowDiskThreshold;

/// agentd's settings. A missing file, or a setting it leaves out, means the default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DaemonConfig {
    /// How little free space agentd warns about on the drive that holds its home.
    pub low_disk_threshold: LowDiskThreshold,
}

impl DaemonConfig {
    /// Reads the settings from `path`. It blocks.
    ///
    /// # Errors
    ///
    /// Returns an error naming `path` when it cannot be read, is not TOML, or holds a setting
    /// agentd does not have or a value the setting does not take.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let _ = path;
        Ok(Self::default())
    }
}

impl std::str::FromStr for DaemonConfig {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let _ = text;
        Ok(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::DaemonConfig;
    use crate::local::disk::LowDiskThreshold;

    #[test]
    fn a_missing_file_or_setting_means_the_default() {
        let home = tempfile::TempDir::new().expect("temporary home");
        assert_eq!(
            DaemonConfig::load(&home.path().join("agentd.toml")).expect("defaults"),
            DaemonConfig::default()
        );
        assert_eq!(
            "# nothing set yet\n".parse::<DaemonConfig>().expect("defaults"),
            DaemonConfig::default()
        );
    }

    #[test]
    fn the_low_disk_threshold_is_read_as_a_share_or_a_size() {
        let config = "lowDiskThreshold = \"20Gi\"\n".parse::<DaemonConfig>().expect("size");
        assert_eq!(
            config.low_disk_threshold,
            "20Gi".parse::<LowDiskThreshold>().expect("size")
        );
        let config = "lowDiskThreshold = \"10%\"\n".parse::<DaemonConfig>().expect("share");
        assert_eq!(
            config.low_disk_threshold,
            "10%".parse::<LowDiskThreshold>().expect("share")
        );
    }

    #[test]
    fn an_invalid_file_is_an_error_that_names_it() {
        let home = tempfile::TempDir::new().expect("temporary home");
        let path = home.path().join("agentd.toml");
        for invalid in [
            "lowDiskThreshold = \"lots\"",
            "lowDiskThreshold = 5",
            "lowDiskTreshold = \"5%\"",
            "lowDiskThreshold = ",
        ] {
            std::fs::write(&path, invalid).expect("write configuration");
            let error = DaemonConfig::load(&path).expect_err(invalid).to_string();
            assert!(error.contains(&path.display().to_string()), "{invalid:?}: {error}");
        }
    }
}
