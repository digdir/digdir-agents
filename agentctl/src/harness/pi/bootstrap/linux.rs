//! Linux Sandbox configuration for Pi.

use sandbox::SandboxHandle;

use crate::{
    Error,
    sandbox::platform::{files::write_if_changed, run_checked},
};

use super::super::{extension, extension_path, instructions_path, platform_directory, skills_directory};

pub(super) async fn configure(
    sandbox: &SandboxHandle,
    home: &str,
    instructions: Option<&[u8]>,
    skills: &[crate::harness::Skill],
) -> Result<(), Error> {
    let skills_path = skills_directory(home);
    // Everything the platform adds lives here. Pi's own directories, user and project, belong
    // to the Agent: its `AGENTS.md`, `settings.json`, `models.json` and extensions are never
    // written, and Pi does not discover this directory on its own; the launch loads the extension.
    let directory = platform_directory(home);
    let extension_path = extension_path(home);
    let instructions_path = instructions_path(home);

    run_checked(sandbox, "/usr/bin/mkdir", ["-p", directory.as_str()]).await?;
    write_if_changed(
        sandbox,
        &extension_path,
        extension::script(&instructions_path)?.as_bytes(),
    )
    .await?;
    let mut managed = vec![extension_path.as_str()];
    if let Some(instructions) = instructions {
        write_if_changed(sandbox, &instructions_path, instructions).await?;
        managed.push(instructions_path.as_str());
    } else {
        // Instructions the manifest dropped must stop applying: the extension adds the file
        // when it exists.
        run_checked(sandbox, "/usr/bin/rm", ["-f", instructions_path.as_str()]).await?;
    }
    // Runtime file transfer writes as the Sandbox supervisor (root), while executions run as the
    // image user. Correct only the files managed above.
    let mut chown = vec!["/usr/bin/chown", "agent:agent"];
    chown.extend(managed.iter().copied());
    run_checked(sandbox, "/usr/bin/sudo", chown).await?;
    run_checked(sandbox, "/usr/bin/chmod", ["644", extension_path.as_str()]).await?;
    if instructions.is_some() {
        run_checked(sandbox, "/usr/bin/chmod", ["644", instructions_path.as_str()]).await?;
    }
    crate::harness::skills::install_linux(sandbox, &skills_path, skills).await
}
