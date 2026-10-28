//! The agent folder's `.env`, which holds the values its manifest declares. `agentctl apply` reads
//! it from beside the manifest, and agentd keeps secrets on the host.

use std::{io::Write as _, path::Path};

use agent::{Error, control_plane::ENV_FILE};
use zeroize::Zeroizing;

/// Writes `values` as `.env` in `folder`, readable only by its owner, and a `.gitignore` that
/// keeps it out of any repository the folder is put in.
pub(super) fn write(folder: &Path, values: &[(String, Zeroizing<String>)]) -> Result<(), Error> {
    let mut contents = Zeroizing::new(String::from(
        "# Values for agent.yaml. Secrets stay on this machine: the agent sees placeholders.\n",
    ));
    for (name, value) in values {
        if value.contains(['\n', '\r']) {
            return Err(Error::Invalid(format!("{name} cannot hold a line break")));
        }
        contents.push_str(name);
        contents.push('=');
        if value.contains(char::is_whitespace) || value.contains('#') {
            if value.contains('"') {
                return Err(Error::Invalid(format!("{name} cannot hold both spaces and quotes")));
            }
            contents.push('"');
            contents.push_str(value);
            contents.push('"');
        } else {
            contents.push_str(value);
        }
        contents.push('\n');
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(folder.join(ENV_FILE))?;
    file.write_all(contents.as_bytes())?;
    std::fs::write(folder.join(".gitignore"), format!("{ENV_FILE}\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use zeroize::Zeroizing;

    use super::write;

    #[test]
    fn values_are_written_quoted_only_when_they_need_it() {
        let folder = tempfile::tempdir().expect("a folder");
        let values = vec![
            ("GITHUB_TOKEN".to_owned(), Zeroizing::new("github_pat_123".to_owned())),
            ("GIT_USER_NAME".to_owned(), Zeroizing::new("Ada Lovelace".to_owned())),
        ];
        write(folder.path(), &values).expect("written");
        let contents = std::fs::read_to_string(folder.path().join(".env")).expect("read");
        assert!(contents.contains("GITHUB_TOKEN=github_pat_123\n"));
        assert!(contents.contains("GIT_USER_NAME=\"Ada Lovelace\"\n"));
        assert_eq!(
            std::fs::read_to_string(folder.path().join(".gitignore")).expect("read"),
            ".env\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(folder.path().join(".env"))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let broken = vec![("X".to_owned(), Zeroizing::new("a\nb".to_owned()))];
        assert!(write(folder.path(), &broken).is_err());
    }
}
