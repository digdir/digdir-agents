//! An agent's `postCreate` steps, run once in its sandbox before the agent is first ready.
//!
//! The record of which steps have run is written in the sandbox as each step ends, rather than kept in the agent's
//! status, which is stored only when a pass ends: a pass cut short by an agentd restart or a stop would otherwise run
//! a finished step again. The guest's boot id, kept with a failure, is what tells a later pass that the sandbox has
//! booted since.

use ::sandbox::{SandboxHandle, SandboxPath, execution::ExecutionSpec};
use serde::{Deserialize, Serialize};

use super::{
    HOME, USER,
    command::{checked_output, last_lines, path_exists, run_checked},
    execution_spec,
};
use crate::{
    Error, PostCreateStep, control_plane,
    manifest::POST_CREATE_STEP_TIMEOUT_MINUTES,
    sandbox::PostCreateOutcome,
    sandbox::platform::files::{read_existing, write_if_changed},
};

/// Holds each step's output and the record of the steps that ran. The sandbox user owns it, since the steps, which
/// run as that user, write their output there.
const DIRECTORY: &str = "/var/lib/agent-post-create";
/// The record of the steps that ran. The sandbox user can change it, which changes only which steps run again.
const STATE: &str = "/var/lib/agent-post-create/state.json";
/// The most of [`STATE`] setup reads. Anything longer was not written by setup, and counts as no record.
const MAX_STATE: usize = 64 * 1024;
/// Changes on every boot of the guest.
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
/// The exit code `timeout` returns when it stopped the step for running out of time.
const TIMED_OUT: i32 = 124;
/// The exit code when the step was killed: by `timeout`, when it ignored being stopped, or for want of memory.
const KILLED: i32 = 137;
/// Runs the step's commands with nothing to read and their output in its log. On failure it prints the end of the log,
/// for the condition to quote. Only that end reaches agentd, so a step that prints without end fills the guest's disk
/// rather than agentd's memory.
const STEP_SCRIPT: &str = r#"log=$1 limit=$2 run=$3
/usr/bin/timeout --kill-after=10s "$limit" /bin/sh -e -c "$run" </dev/null >"$log" 2>&1 && exit 0
status=$?
/usr/bin/tail -c 2048 "$log" >&2
exit "$status""#;

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct State {
    /// The steps that succeeded, by name.
    #[serde(default)]
    completed: Vec<String>,
    /// The step that failed last, if it has not succeeded since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed: Option<Failure>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Failure {
    step: String,
    /// The guest's boot it failed in.
    boot: String,
    message: String,
}

/// Runs the agent's `postCreate` steps that have not run, in order, and reports how far they got: `None` for an agent
/// that declares none.
///
/// # Errors
///
/// Returns an error when the sandbox cannot run a step or keep its record. A step that fails is no error, but an
/// outcome.
pub(super) async fn run(
    record: &control_plane::AgentRecord,
    sandbox: &SandboxHandle,
    progress: &::sandbox::SandboxProgress,
) -> Result<Option<PostCreateOutcome>, Error> {
    let declared = &record.agent.spec.post_create;
    if declared.is_empty() {
        return Ok(None);
    }
    let Some(mut state) = read_state(sandbox).await? else {
        return Ok(Some(PostCreateOutcome::Failed(format!(
            "The record of the postCreate steps that ran, {STATE}, cannot be read, so none of them runs again. Remove \
             it to run them all again."
        ))));
    };
    let pending: Vec<&PostCreateStep> = declared
        .iter()
        .filter(|step| !state.completed.contains(&step.name))
        .collect();
    let Some(first) = pending.first() else {
        return Ok(Some(PostCreateOutcome::Completed));
    };
    if let Some(failure) = &state.failed
        && failure.step == first.name
        && failure.boot == boot_id(sandbox).await?
    {
        return Ok(Some(PostCreateOutcome::Failed(failure.message.clone())));
    }
    run_checked(
        sandbox,
        "/usr/bin/sudo",
        [
            "-n",
            "/usr/bin/install",
            "-d",
            "-m",
            "0755",
            "-o",
            USER,
            "-g",
            USER,
            DIRECTORY,
        ],
    )
    .await?;
    for step in pending {
        let started = progress.start_step(format!("Run postCreate step {}", step.name)).await;
        let log = format!("{DIRECTORY}/{}.log", step.name);
        let output = sandbox.run_execution(execution(step, &log)?).await?;
        if output.status.success() {
            state.completed.push(step.name.clone());
            state.failed = None;
            write_state(sandbox, &state).await?;
            started.complete().await;
            continue;
        }
        let message = failure_message(step, &log, output.status.code, &output.stderr);
        state.failed = Some(Failure {
            step: step.name.clone(),
            boot: boot_id(sandbox).await?,
            message: message.clone(),
        });
        write_state(sandbox, &state).await?;
        started.complete().await;
        return Ok(Some(PostCreateOutcome::Failed(message)));
    }
    Ok(Some(PostCreateOutcome::Completed))
}

/// Runs `step` as the sandbox user in its home, with its output in `log`.
fn execution(step: &PostCreateStep, log: &str) -> Result<ExecutionSpec, Error> {
    let command = [
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        STEP_SCRIPT.to_owned(),
        "postCreate".to_owned(),
        log.to_owned(),
        format!("{POST_CREATE_STEP_TIMEOUT_MINUTES}m"),
        step.run.clone(),
    ];
    Ok(execution_spec(&command, false)?.with_working_directory(SandboxPath::new(HOME)))
}

/// Says which step failed and why, quoting the end of its output, and where the rest of it is.
fn failure_message(step: &PostCreateStep, log: &str, code: i32, stderr: &[u8]) -> String {
    let what = match code {
        TIMED_OUT => format!("did not finish within {POST_CREATE_STEP_TIMEOUT_MINUTES} minutes"),
        KILLED => format!("was killed, after {POST_CREATE_STEP_TIMEOUT_MINUTES} minutes or for want of memory"),
        code => format!("exited with code {code}"),
    };
    // The output comes from the guest and is shown in a terminal on the host, so it may carry no control characters.
    let tail = last_lines(&String::from_utf8_lossy(stderr))
        .chars()
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect::<String>();
    let tail = if tail.is_empty() {
        String::new()
    } else {
        format!(": {tail}")
    };
    format!(
        "postCreate step {:?} {what}{tail}. Its output is in {log}. It runs again, with the steps after it, when the \
         sandbox next boots, such as on `agentctl restart`.",
        step.name
    )
}

async fn boot_id(sandbox: &SandboxHandle) -> Result<String, Error> {
    let args = [BOOT_ID.to_owned()];
    let output = sandbox
        .run_execution(ExecutionSpec::command(SandboxPath::new("/bin/cat"), args.clone()))
        .await?;
    checked_output("/bin/cat", &args, &output)?;
    let boot = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if boot.is_empty() {
        return Err(Error::SandboxSetup(format!(
            "{BOOT_ID} is empty, so whether a failed postCreate step may run again is unknown"
        )));
    }
    Ok(boot)
}

/// The record of the steps that ran: empty when there is none, and `None` when there is one that cannot be read, since
/// the steps it records must not run again.
async fn read_state(sandbox: &SandboxHandle) -> Result<Option<State>, Error> {
    let state = read_existing(sandbox, STATE, MAX_STATE)
        .await
        .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok());
    if state.is_some() {
        return Ok(state);
    }
    Ok((!path_exists(sandbox, "-e", STATE).await?).then(State::default))
}

async fn write_state(sandbox: &SandboxHandle, state: &State) -> Result<(), Error> {
    let contents = serde_json::to_vec(state)
        .map_err(|error| Error::SandboxSetup(format!("cannot record the postCreate steps that ran: {error}")))?;
    write_if_changed(sandbox, STATE, &contents).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{KILLED, TIMED_OUT, failure_message};
    use crate::PostCreateStep;

    fn failed(code: i32) -> String {
        let step = PostCreateStep {
            name: "clone".into(),
            run: "git clone x".into(),
        };
        failure_message(
            &step,
            "/var/lib/agent-post-create/clone.log",
            code,
            b"fatal: not found\n",
        )
    }

    #[test]
    fn a_step_stopped_for_its_time_is_not_reported_as_exiting_on_its_own() {
        for code in [TIMED_OUT, KILLED] {
            let message = failed(code);
            assert!(!message.contains("exited with code"), "{message}");
            assert!(message.contains("fatal: not found"), "{message}");
        }
        assert!(failed(128).contains("exited with code 128"));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod shell_tests {
    #![allow(clippy::expect_used)]

    use std::{io::Write as _, process::Stdio};

    use super::STEP_SCRIPT;

    /// Runs `run` as a step with a time limit of `limit`, as the guest would, here: its exit code, its log, and what
    /// it printed for agentd.
    fn step(run: &str, limit: &str) -> (i32, String, String) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = directory.path().join("step.log");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", STEP_SCRIPT, "postCreate"])
            .arg(&log)
            .args([limit, run])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the step's shell");
        // Input a step must never see.
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(b"input\n")
            .expect("write stdin");
        let output = child.wait_with_output().expect("the step's result");
        (
            output.status.code().expect("an exit code"),
            std::fs::read_to_string(&log).expect("the step's log"),
            String::from_utf8(output.stderr).expect("UTF-8"),
        )
    }

    #[test]
    #[ignore = "runs the step's shell with this machine's /bin/sh, GNU timeout and tail"]
    fn the_step_script_runs_like_the_guest_runs_it() {
        assert_eq!(
            step("echo out; echo err >&2", "10m"),
            (0, "out\nerr\n".into(), String::new()),
            "a step that succeeds prints nothing for agentd"
        );
        assert_eq!(
            step("echo one; false; echo never", "10m"),
            (1, "one\n".into(), "one\n".into()),
            "sh -e stops at the first command that fails, and the end of the log is printed"
        );
        assert_eq!(
            step("read line || echo no input; exit 3", "10m"),
            (3, "no input\n".into(), "no input\n".into()),
            "a step has nothing to read, and keeps its exit code"
        );
        let started = std::time::Instant::now();
        let (code, ..) = step("sleep 30", "1s");
        assert_eq!(code, super::TIMED_OUT);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "a step is stopped at its limit"
        );
    }
}
