//! An agent's `postCreate` steps, run once in its sandbox before the agent is first ready.
//!
//! Each step runs at most once per boot of the sandbox. Before a step runs, the record in the sandbox says it failed in
//! this boot, and only its end changes that, so a pass cut short while it runs, by an agentd restart or a guest that
//! stopped responding, leaves it failed rather than running it again on every pass. The record is in the sandbox,
//! rather than in the agent's status, which is stored only when a pass ends, so it is written as each step ends. The
//! guest's boot id is what tells a later pass that the sandbox has booted since.

use std::time::Duration;

use ::sandbox::{
    SandboxHandle, SandboxPath,
    execution::{ExecutionEvent, ExecutionSpec, StartExecutionRequest},
};
use futures_util::StreamExt as _;
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
/// The record of the steps that ran. The sandbox user can change it, which changes which steps run again, so what it
/// says is read as the guest's word, as the steps' output is.
const STATE: &str = "/var/lib/agent-post-create/state.json";
/// The most of [`STATE`] setup reads. Anything longer was not written by setup, and counts as no record.
const MAX_STATE: usize = 64 * 1024;
/// Changes on every boot of the guest.
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
/// The most of a failure's message the condition carries.
const MAX_MESSAGE: usize = 1024;
/// The most of a failed step's output its message quotes, from the end, so the reason and where the log is always fit.
const QUOTED_OUTPUT: usize = 400;
/// The most of the end of a failed step's output agentd keeps as the guest sends it.
const OUTPUT_TAIL: usize = 2048;
/// How long agentd waits for the guest to kill a step it stopped waiting for.
const KILL_LIMIT: Duration = Duration::from_secs(10);
/// The exit code `timeout` returns when it stopped the step for running out of time.
const TIMED_OUT: i32 = 124;
/// The exit code when the step was killed: by `timeout`, when it ignored being stopped, or for want of memory.
const KILLED: i32 = 137;
/// Runs the step's commands with nothing to read and their output in its log. On failure it prints the end of the log,
/// for the condition to quote. `--` keeps commands that start with `-` from being read as the shell's options.
const STEP_SCRIPT: &str = r#"log=$1 limit=$2 run=$3
/usr/bin/timeout --kill-after=10s "$limit" /bin/sh -e -c -- "$run" </dev/null >"$log" 2>&1 && exit 0
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
    let boot = boot_id(sandbox).await?;
    if let Some(failure) = &state.failed
        && failure.step == first.name
        && failure.boot == boot
    {
        return Ok(Some(failed(&failure.message)));
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
        state.failed = Some(Failure {
            step: step.name.clone(),
            boot: boot.clone(),
            message: format!(
                "postCreate step {:?} did not finish: setup stopped while it ran, as when agentd restarts or the guest \
                 stops responding. {}",
                step.name,
                runs_again(&log)
            ),
        });
        write_state(sandbox, &state).await?;
        let end = run_step(sandbox, execution(step, &log)?).await?;
        if matches!(end, StepEnd::Exited { code: 0, .. }) {
            state.completed.push(step.name.clone());
            state.failed = None;
            write_state(sandbox, &state).await?;
            started.complete().await;
            continue;
        }
        let message = failure_message(step, &log, &end);
        if let Some(failure) = &mut state.failed {
            failure.message.clone_from(&message);
        }
        write_state(sandbox, &state).await?;
        // Left uncompleted, so progress shows the step as failed.
        drop(started);
        return Ok(Some(failed(&message)));
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

/// How a step ended.
enum StepEnd {
    /// It exited with `code` after `elapsed`, having printed `stderr` last: the end of its log, when it failed.
    Exited {
        code: i32,
        elapsed: Duration,
        stderr: Vec<u8>,
    },
    /// agentd stopped waiting for it, and killed it.
    OutOfTime,
}

/// How long a step may run before the guest's `timeout` stops it.
fn step_limit() -> Duration {
    Duration::from_secs(u64::from(POST_CREATE_STEP_TIMEOUT_MINUTES) * 60)
}

/// Runs a step, keeping only the end of what it prints, for no longer than its limit, the 10 seconds `timeout` gives a
/// step that ignores being stopped, and half a minute to spare. The guest's `timeout` ends a step on its own. This
/// bound holds when the guest keeps one from ending, such as with a FIFO at its log or a process it left holding its
/// output.
async fn run_step(sandbox: &SandboxHandle, spec: ExecutionSpec) -> Result<StepEnd, Error> {
    let started_at = tokio::time::Instant::now();
    let deadline = started_at + step_limit() + Duration::from_secs(40);
    let Ok(started) =
        tokio::time::timeout_at(deadline, sandbox.start_execution(StartExecutionRequest::new(spec))).await
    else {
        return Ok(StepEnd::OutOfTime);
    };
    let started = started?;
    let mut events = started.events;
    let mut stderr = Vec::new();
    loop {
        let Ok(event) = tokio::time::timeout_at(deadline, events.next()).await else {
            // Whatever keeps the step running is the guest's to end; agentd only waits so long.
            let _killed = tokio::time::timeout(KILL_LIMIT, sandbox.kill_execution(&started.id)).await;
            return Ok(StepEnd::OutOfTime);
        };
        match event.transpose()? {
            Some(ExecutionEvent::Stderr(chunk)) => keep_end(&mut stderr, &chunk, OUTPUT_TAIL),
            Some(ExecutionEvent::Exited(status)) => {
                return Ok(StepEnd::Exited {
                    code: status.code,
                    elapsed: started_at.elapsed(),
                    stderr,
                });
            }
            Some(ExecutionEvent::Failed { message }) => {
                return Err(Error::SandboxSetup(format!(
                    "a postCreate step could not run: {message}"
                )));
            }
            // The step's own output goes to its log, and a start or an event a later backend adds says nothing of its end.
            Some(_) => {}
            None => {
                return Err(Error::SandboxSetup(
                    "a postCreate step ended without an exit status".into(),
                ));
            }
        }
    }
}

/// Appends `chunk` to `kept`, keeping only its last `limit` bytes.
fn keep_end(kept: &mut Vec<u8>, chunk: &[u8], limit: usize) {
    kept.extend_from_slice(&chunk[chunk.len().saturating_sub(limit)..]);
    let excess = kept.len().saturating_sub(limit);
    kept.drain(..excess);
}

/// Says which step failed and why, quoting the end of its output, and where the rest of it is.
fn failure_message(step: &PostCreateStep, log: &str, end: &StepEnd) -> String {
    let out_of_time = format!("did not finish within {POST_CREATE_STEP_TIMEOUT_MINUTES} minutes");
    let (what, stderr) = match end {
        StepEnd::OutOfTime => (out_of_time, &[][..]),
        // `timeout` returns these once the step outlasts its limit. A step can return them sooner of its own accord.
        StepEnd::Exited {
            code: TIMED_OUT | KILLED,
            elapsed,
            stderr,
        } if *elapsed >= step_limit() => (out_of_time, stderr.as_slice()),
        StepEnd::Exited {
            code: KILLED, stderr, ..
        } => (
            "was killed, as when it runs out of memory".to_owned(),
            stderr.as_slice(),
        ),
        StepEnd::Exited { code, stderr, .. } => (format!("exited with code {code}"), stderr.as_slice()),
    };
    let tail = last_lines(&String::from_utf8_lossy(stderr));
    let skipped = tail.chars().count().saturating_sub(QUOTED_OUTPUT);
    let tail = tail.chars().skip(skipped).collect::<String>();
    let tail = if tail.is_empty() {
        String::new()
    } else {
        format!(": {tail}")
    };
    format!("postCreate step {:?} {what}{tail}. {}", step.name, runs_again(log))
}

fn runs_again(log: &str) -> String {
    format!(
        "Its output is in {log}. It runs again, with the steps after it, when the sandbox next boots, such as on \
         `agentctl restart`."
    )
}

/// A failure, with its message as it may be shown in a terminal on the host. The message carries what the guest wrote,
/// in a step's output or in the record, so it is kept to one line of printable characters and a bounded length.
fn failed(message: &str) -> PostCreateOutcome {
    let mut printable = message.chars().map(printable).collect::<String>();
    if let Some((end, _)) = printable.char_indices().nth(MAX_MESSAGE) {
        printable.truncate(end);
        printable.push('…');
    }
    PostCreateOutcome::Failed(printable)
}

/// `character`, or a space in place of one that could move or recolour a terminal, or reorder or break the line it
/// shows.
const fn printable(character: char) -> char {
    if character.is_control()
        || matches!(
            character,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
    {
        ' '
    } else {
        character
    }
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

    use super::{KILLED, StepEnd, TIMED_OUT, failure_message, keep_end, step_limit};
    use crate::PostCreateStep;

    fn failed(code: i32, elapsed: std::time::Duration) -> String {
        let step = PostCreateStep {
            name: "clone".into(),
            run: "git clone x".into(),
        };
        let end = StepEnd::Exited {
            code,
            elapsed,
            stderr: b"fatal: not found\n".to_vec(),
        };
        failure_message(&step, "/var/lib/agent-post-create/clone.log", &end)
    }

    #[test]
    fn a_step_stopped_for_its_time_is_not_reported_as_exiting_on_its_own() {
        for code in [TIMED_OUT, KILLED] {
            let message = failed(code, step_limit());
            assert!(message.contains("did not finish"), "{message}");
            assert!(message.contains("fatal: not found"), "{message}");
        }
        assert!(failed(128, step_limit()).contains("exited with code 128"));
    }

    #[test]
    fn agentd_keeps_only_the_end_of_a_steps_output() {
        let mut kept = Vec::new();
        for _ in 0..1024 {
            keep_end(&mut kept, &[b'x'; 1024], 2048);
        }
        keep_end(&mut kept, b"the end", 2048);
        assert_eq!(kept.len(), 2048);
        assert!(kept.ends_with(b"the end"));
        keep_end(&mut kept, &[b'y'; 4096], 2048);
        assert_eq!(kept, [b'y'; 2048]);
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
        assert_eq!(
            step("-x", "10m").0,
            127,
            "a command that starts with - is a command, not an option"
        );
        let started = std::time::Instant::now();
        let (code, ..) = step("sleep 30", "1s");
        assert_eq!(code, super::TIMED_OUT);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "a step is stopped at its limit"
        );
        let started = std::time::Instant::now();
        let (code, ..) = step("trap '' TERM; sleep 60", "1s");
        assert_eq!(code, super::KILLED);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "a step that ignores being stopped is killed"
        );
    }
}
