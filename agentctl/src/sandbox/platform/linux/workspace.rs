//! The repositories an image clones into the workspace.
//!
//! The lists are read as the image's clone script reads them, except in two corners that the base images do not
//! use: the script reads them from `$AGENT_WORKSPACE_LISTS` when that is set, and it keeps a `\r` at the end of a
//! line, which this drops.

use ::sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};

use crate::Error;

use super::{WORKING_DIRECTORY, checked_output, run_checked};

/// Prints every list in `/etc/agent-workspace.d`, where the base images look for the repositories they clone at boot
/// (see `images/agentctl-base/headless/usr/local/libexec/agent-workspace-init`). The lists come in the order the
/// clone script reads them, each followed by a newline so that a last line without one does not run into the next.
const PRINT_LISTS: &str = "for list in /etc/agent-workspace.d/*.list; do \
                           if [ -f \"$list\" ]; then /bin/cat -- \"$list\"; echo; fi; \
                           done";

/// Creates the working directory, and reads the checkouts the image clones into it, as [`read_projects`] does.
///
/// # Errors
///
/// Returns an error when the directory cannot be created or the lists cannot be read.
pub(super) async fn prepare(sandbox: &SandboxHandle) -> Result<Vec<String>, Error> {
    // `mkdir` leaves a folder that is there alone, such as a bind mount from Windows, where
    // changing its mode is refused.
    run_checked(sandbox, "/bin/mkdir", ["-p", "-m", "0755", WORKING_DIRECTORY]).await?;
    read_projects(sandbox).await
}

/// Reads the checkouts the image's lists say it clones, as paths in the sandbox, in list order and without
/// duplicates. An image without lists clones none.
///
/// It reads what the image declares, not what is on disk: the clones are made at boot, and may still be under way
/// or have failed.
///
/// # Errors
///
/// Returns an error when the lists cannot be read.
async fn read_projects(sandbox: &SandboxHandle) -> Result<Vec<String>, Error> {
    let args = ["-c".to_owned(), PRINT_LISTS.to_owned()];
    let output = sandbox
        .run_execution(ExecutionSpec::command(SandboxPath::new("/bin/sh"), args.clone()))
        .await?;
    checked_output("/bin/sh", &args, &output)?;
    Ok(parse_projects(&String::from_utf8_lossy(&output.stdout)))
}

/// Parses list lines as the image's clone script does: `#` starts a comment, the first field names the repository
/// as `OWNER/NAME`, and the optional second is the destination below the working directory, `NAME` by default.
fn parse_projects(lists: &str) -> Vec<String> {
    let mut projects: Vec<String> = Vec::new();
    for line in lists.lines() {
        let line = line.split_once('#').map_or(line, |(before, _)| before);
        let mut fields = line.split_whitespace();
        let Some(repository) = fields.next() else {
            continue;
        };
        let destination = fields
            .next()
            .unwrap_or_else(|| repository.split_once('/').map_or(repository, |(_, name)| name));
        let project = format!("{WORKING_DIRECTORY}/{destination}");
        if !projects.contains(&project) {
            projects.push(project);
        }
    }
    projects
}

#[cfg(test)]
mod tests {
    use super::parse_projects;

    #[test]
    fn lists_are_read_as_the_clone_script_reads_them() {
        let lists = "# The team's repositories\n\
                     digdir/digdir-agents\n\
                     \n\
                     altinn/altinn-profile   profile # renamed\n\
                     digdir/digdir-agents\n\
                     altinn/altinn-studio apps/studio";
        assert_eq!(
            parse_projects(lists),
            [
                "/home/agent/code/digdir-agents",
                "/home/agent/code/profile",
                "/home/agent/code/apps/studio",
            ]
        );
        assert_eq!(parse_projects("# nothing yet\n\n"), Vec::<String>::new());
    }
}
