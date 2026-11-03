//! The file of values an agent's manifest declares, which `agentctl apply` reads with `--env-file`
//! and agentd remembers. Setup keeps it in the control-plane home, outside any checkout, so no
//! commit and no mounted folder takes it along.

use std::{io::Write as _, path::Path};

use agent::Error;
use zeroize::Zeroizing;

/// Writes `values` to `path`, readable only by its owner.
pub(super) fn write(path: &Path, values: &[(String, Zeroizing<String>)]) -> Result<(), Error> {
    let mut contents = Zeroizing::new(String::from(
        "# Values for an agent's manifest. Secrets stay on this machine: the agent sees placeholders.\n",
    ));
    for (name, value) in values {
        assign(&mut contents, name, value)?;
    }
    write_private(path, &contents)
}

/// Appends the line that sets `name` to `value`, quoted only when the value needs it.
pub(super) fn assign(contents: &mut String, name: &str, value: &str) -> Result<(), Error> {
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
    Ok(())
}

/// Writes `contents` to `path`, readable only by its owner, since it may hold secrets. A file
/// that exists already is made so before anything is written to it, as it may have been readable
/// by others.
pub(super) fn write_private(path: &Path, contents: &str) -> Result<(), Error> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.set_len(0)?;
    file.write_all(contents.as_bytes())?;
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
        let path = folder.path().join("worker.env");
        write(&path, &values).expect("written");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.contains("GITHUB_TOKEN=github_pat_123\n"));
        assert!(contents.contains("GIT_USER_NAME=\"Ada Lovelace\"\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("metadata").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let broken = vec![("X".to_owned(), Zeroizing::new("a\nb".to_owned()))];
        assert!(write(&path, &broken).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_values_file_others_could_read_is_made_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let folder = tempfile::tempdir().expect("a folder");
        let path = folder.path().join("worker.env");
        std::fs::write(&path, "OLD=value that is longer than what replaces it\n").expect("an earlier file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("readable by others");
        super::write_private(&path, "NEW=1\n").expect("written");
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "NEW=1\n");
    }
}
