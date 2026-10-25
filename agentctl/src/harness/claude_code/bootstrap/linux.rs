//! Linux Sandbox configuration for mediated Claude Code authentication.

use sandbox::SandboxHandle;

use crate::{
    Error,
    sandbox::platform::{files::write_if_changed, run_checked},
};

use super::super::ACCESS_PLACEHOLDER;

/// Days before Claude Code's retention sweep deletes an untouched transcript: effectively never.
const TRANSCRIPT_RETENTION_DAYS: u32 = 36_500;

/// Claude Code reads managed settings in every process, including ones an interface such as T3 Code
/// or a login shell starts, which never see the Session launch settings. A drop-in leaves an image's
/// own `managed-settings.json` in place.
const MANAGED_SETTINGS_DIRECTORY: &str = "/etc/claude-code/managed-settings.d";
const MANAGED_SETTINGS_PATH: &str = "/etc/claude-code/managed-settings.d/10-agent-platform.json";

pub(super) async fn configure(
    sandbox: &SandboxHandle,
    home: &str,
    instructions: &[u8],
    skills: &[crate::harness::Skill],
) -> Result<(), Error> {
    let skills_path = format!("{home}/.claude/skills");
    let config = format!("{home}/.claude");
    let hooks_path = format!("{config}/hooks");
    let credentials_path = format!("{config}/.credentials.json");
    let hook_path = format!("{config}/hooks/activity-hook.mjs");
    let settings_path = format!("{config}/agent-settings.json");
    let status_line_path = format!("{config}/status-line.mjs");
    let instructions_path = super::super::instructions_path(home);
    run_checked(sandbox, "/usr/bin/mkdir", ["-p", hooks_path.as_str()]).await?;
    let credentials = serde_json::to_vec(&serde_json::json!({
        "claudeAiOauth": {
            "accessToken": ACCESS_PLACEHOLDER,
            "refreshToken": "agent-mediated-refresh-placeholder-not-a-real-credential",
            "expiresAt": 4_102_444_800_000_i64,
            "refreshTokenExpiresAt": 4_102_444_800_000_i64,
            "scopes": ["user:inference"]
        }
    }))?;
    write_if_changed(sandbox, &credentials_path, &credentials).await?;
    write_if_changed(sandbox, &instructions_path, instructions).await?;
    write_if_changed(sandbox, &hook_path, super::super::hooks::script()?.as_bytes()).await?;
    write_if_changed(sandbox, &status_line_path, super::super::status_line::script()).await?;
    let mut settings = sandbox_settings();
    // Sessions also report activity. Their hooks include the Bash approval.
    settings.insert("hooks".into(), super::super::hooks::configuration(&hook_path));
    // The platform launches every Session in bypass mode. Keep its acknowledgement in
    // these launch settings: an Agent's home overlay can replace Claude's user settings.
    settings.insert("skipDangerousModePermissionPrompt".into(), true.into());
    settings.insert(
        "statusLine".into(),
        super::super::status_line::configuration(&status_line_path),
    );
    write_if_changed(sandbox, &settings_path, &serde_json::to_vec(&settings)?).await?;
    // The launch settings above repeat these for Sessions: when an organization delivers managed
    // settings, Claude Code takes from a managed settings file only what restricts.
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["/usr/bin/mkdir", "-p", MANAGED_SETTINGS_DIRECTORY],
    )
    .await?;
    write_if_changed(
        sandbox,
        MANAGED_SETTINGS_PATH,
        &serde_json::to_vec(&sandbox_settings())?,
    )
    .await?;
    // Root owns it, and every user's Claude Code must read it.
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["/usr/bin/chmod", "644", MANAGED_SETTINGS_PATH],
    )
    .await?;
    // Runtime file transfer writes as the Sandbox supervisor (root), while
    // executions run as the image user. Correct only the directories and files
    // managed above: recursive ownership walks would traverse the growing
    // harness state tree on every reconciliation pass.
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        [
            "/usr/bin/chown",
            "agent:agent",
            config.as_str(),
            hooks_path.as_str(),
            credentials_path.as_str(),
            hook_path.as_str(),
            settings_path.as_str(),
            status_line_path.as_str(),
        ],
    )
    .await?;
    run_checked(sandbox, "/usr/bin/chmod", ["600", credentials_path.as_str()]).await?;
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        ["/usr/bin/chown", "agent:agent", instructions_path.as_str()],
    )
    .await?;
    run_checked(sandbox, "/usr/bin/chmod", ["644", instructions_path.as_str()]).await?;
    crate::harness::skills::install_linux(sandbox, &skills_path, skills).await
}

/// Settings for every Claude Code process in the Sandbox, whoever starts it.
fn sandbox_settings() -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        // HACK: the mediated setup token is inference-only, so Claude Code cannot read the
        // account's plan entitlement and gates Fable behind a usage-credits prompt. Declaring the
        // subscription type and rate-limit tier in the settings env satisfies the client-side
        // plan-inclusion check (the literal "max" tier is what the check looks for, regardless of
        // the real plan), and the server still authorizes inference independently. Both are
        // required. The type alone unblocks Max models but not Fable. Remove when
        // github.com/anthropics/claude-code#79360 ships.
        (
            "env".to_owned(),
            serde_json::json!({
                "CLAUDE_CODE_SUBSCRIPTION_TYPE": "max",
                "CLAUDE_CODE_RATE_LIMIT_TIER": "default_claude_max_5x"
            }),
        ),
        // Claude Code deletes transcripts untouched for 30 days by default, so an Idle Session
        // would resume into a fresh conversation. Discarding a conversation is the platform's call.
        ("cleanupPeriodDays".to_owned(), TRANSCRIPT_RETENTION_DAYS.into()),
        (
            "hooks".to_owned(),
            serde_json::json!({ "PermissionRequest": [super::super::hooks::bash_approval()] }),
        ),
    ])
}
