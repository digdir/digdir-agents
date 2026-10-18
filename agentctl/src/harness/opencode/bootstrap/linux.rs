//! Linux Sandbox configuration for `OpenCode`.

use sandbox::SandboxHandle;

use crate::{
    Error,
    sandbox::platform::{files::write_if_changed, run_checked},
};

use super::super::{
    configuration_directory, instructions_path, plugin, plugin_directory, plugin_path, skills_directory,
};

pub(super) async fn configure(
    sandbox: &SandboxHandle,
    home: &str,
    instructions: Option<&[u8]>,
    skills: &[crate::harness::Skill],
) -> Result<(), Error> {
    let skills_path = skills_directory(home);
    // The plugin lives outside OpenCode's own directories, which belong to the Agent; OpenCode
    // does not discover it there, so the launch loads it. Instructions go where OpenCode reads
    // global rules, as the other harnesses' do.
    let directory = plugin_directory(home);
    let plugin_path = plugin_path(home);
    let instructions_path = instructions_path(home);
    let instructions_directory = configuration_directory(home);

    run_checked(
        sandbox,
        "/usr/bin/mkdir",
        ["-p", directory.as_str(), instructions_directory.as_str()],
    )
    .await?;
    write_if_changed(sandbox, &plugin_path, plugin::script()?.as_bytes()).await?;
    let mut managed = vec![plugin_path.as_str()];
    if let Some(instructions) = instructions {
        write_if_changed(sandbox, &instructions_path, instructions).await?;
        managed.push(instructions_path.as_str());
    }
    // Runtime file transfer writes as the Sandbox supervisor (root), while executions run as the
    // image user. Correct only the files managed above.
    let mut chown = vec!["/usr/bin/chown", "agent:agent"];
    chown.extend(managed.iter().copied());
    run_checked(sandbox, "/usr/bin/sudo", chown).await?;
    let mut chmod = vec!["644"];
    chmod.extend(managed.iter().copied());
    run_checked(sandbox, "/usr/bin/chmod", chmod).await?;
    crate::harness::skills::install_linux(sandbox, &skills_path, skills).await
}
