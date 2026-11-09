//! Sandbox-platform-specific Claude Code configuration.

mod linux;

pub(super) async fn configure_linux(
    sandbox: &sandbox::SandboxHandle,
    home: &str,
    working_directory: &str,
    instructions: &[u8],
    skills: &[crate::harness::Skill],
) -> Result<(), crate::Error> {
    linux::configure(sandbox, home, working_directory, instructions, skills).await
}
