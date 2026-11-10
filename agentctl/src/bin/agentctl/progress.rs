//! Following an agent's provisioning from a plain terminal.
//!
//! [`Wait`] runs one Control API call while it follows the agent's progress
//! with `agents.v1.progress` and renders it to stderr. On a terminal the
//! current phase and step live on one updating line and only work that took
//! noticeable time leaves a permanent line, so a warm ensure prints nothing.
//! Without a terminal every completion and output line is printed once.
//!
//! A command for which the agent is only the means, such as a sign-in, follows
//! it as a [`Summary`] instead: one updating line that says what the command
//! gets ready, and one line once it is ready. Failures are shown in full either
//! way.

use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{self, IsTerminal as _, Write},
    pin::pin,
    time::{Duration, Instant},
};

use agent::{
    FailureKind,
    control_api::Client,
    progress::{AgentProgress, OutputPosition, Provisioning},
    resources::Revision,
};
use sandbox::{
    SandboxPhase,
    progress::{Measurement, OperationStatus, Outcome, ProgressCursor, ProgressUnit, Update},
};

/// Minimum interval between redraws of the updating line.
const REDRAW_INTERVAL: Duration = Duration::from_millis(50);
/// Completed phases faster than this leave no line on a terminal.
const NOTABLE_PHASE_MS: u64 = 100;
/// Completed steps faster than this leave no line on a terminal.
const NOTABLE_STEP_MS: u64 = 500;
const FALLBACK_WIDTH: usize = 80;
/// Output lines of the failed step shown in the failure report.
const RECENT_OUTPUT_LINES: usize = 5;
/// Longest wait for the state reached when the followed call returned.
const FINAL_READ_TIMEOUT: Duration = Duration::from_millis(500);
/// Pause before following again after a failed progress read.
const RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// One followed call: owns the renderer for its duration.
///
/// Ctrl-C is deliberately not handled here. The default SIGINT disposition ends
/// `agentctl` with status 130, the daemon keeps reconciling regardless, and the
/// renderer already tells the user so on the first failure. A handler would
/// outlive the wait (tokio cannot uninstall it) and every later phase of the
/// process would have to emulate the default by hand.
///
/// ```ignore
/// let wait = Wait::start();
/// let target = wait
///     .until(client, &agent, client.ensure_execution(&agent, WaitPolicy::UntilConverged))
///     .await?;
/// ```
pub(crate) struct Wait {
    renderer: RefCell<Renderer>,
}

impl Wait {
    pub(crate) fn start() -> Self {
        Self {
            renderer: RefCell::new(Renderer::stderr()),
        }
    }

    /// A wait shown as one line saying it is `doing` something, such as getting a sign-in ready,
    /// until [`Self::ready`] says it is `done`.
    pub(crate) fn summarized(doing: impl Into<String>, done: impl Into<String>) -> Self {
        let mut renderer = Renderer::stderr();
        renderer.summarize(Summary {
            doing: doing.into(),
            done: done.into(),
            started: Instant::now(),
        });
        Self {
            renderer: RefCell::new(renderer),
        }
    }

    /// Says that what the wait was for is done: a summarized one shows its `done` line, with how
    /// long it took.
    pub(crate) fn ready(&self) {
        self.renderer.borrow_mut().ready();
    }

    /// Runs `call` to completion while rendering `agent`'s progress, then
    /// renders the final state and settles the terminal.
    pub(crate) async fn until<T>(&self, client: &Client, agent: &str, call: impl Future<Output = T>) -> T {
        let mut call = pin!(call);
        loop {
            let (after, output) = self.renderer.borrow().position();
            let follow = async {
                let progress = client.agent_progress(agent, after, output).await.ok();
                if progress.is_none() {
                    // The agent may not be stored yet, or the daemon is restarting.
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
                progress
            };
            tokio::select! {
                biased;
                result = &mut call => {
                    let (_, output) = self.renderer.borrow().position();
                    let latest = tokio::time::timeout(FINAL_READ_TIMEOUT, client.agent_progress(agent, None, output));
                    if let Ok(Ok(progress)) = latest.await {
                        self.renderer.borrow_mut().render(&progress);
                    }
                    self.renderer.borrow_mut().finish();
                    return result;
                }
                progress = follow => {
                    if let Some(progress) = progress {
                        self.renderer.borrow_mut().render(&progress);
                    }
                }
            }
        }
    }
}

impl Drop for Wait {
    /// Clears the updating line, also when the command fails before or between the calls it
    /// follows, so its error starts on a line of its own.
    fn drop(&mut self) {
        self.renderer.get_mut().finish();
    }
}

/// What a summarized wait says while it waits and once it is ready.
struct Summary {
    doing: String,
    done: String,
    started: Instant,
}

/// Where the renderer writes, which decides between an updating line and plain lines.
#[derive(Clone, Copy)]
enum Mode {
    /// An interactive terminal of the given width.
    Terminal { width: usize },
    /// A pipe or file: every completion is printed once, nothing is redrawn.
    Plain,
}

pub(crate) struct Renderer<W: Write = io::Stderr> {
    output: W,
    mode: Mode,
    active_line: bool,
    /// The updating line last drawn.
    last_line: String,
    last_redraw: Option<Instant>,
    last_error: Option<String>,
    /// Consecutive failed passes with the current error.
    failures: u32,
    revision: Option<Revision>,
    /// Pass being rendered and the position in its progress.
    pass: Option<Revision>,
    cursor: ProgressCursor,
    /// Pass whose failure was already reported.
    reported_failure: Option<Revision>,
    /// A step of the current phase left a permanent line on the terminal.
    printed_step: bool,
    /// Output of the step in progress, shown when it fails.
    recent_output: VecDeque<String>,
    /// Ready condition already considered, so a standing failure is shown once.
    seen_condition: Option<String>,
    started: bool,
    /// Set for a summarized wait, which shows no steps.
    summary: Option<Summary>,
    /// The step downloading the image, whose count a summary shows.
    download: Option<sandbox::StepId>,
}

impl Renderer {
    pub(crate) fn stderr() -> Self {
        let output = io::stderr();
        let mode = if output.is_terminal() {
            Mode::Terminal {
                width: crossterm::terminal::size().map_or(FALLBACK_WIDTH, |(columns, _)| usize::from(columns)),
            }
        } else {
            Mode::Plain
        };
        Self::new(output, mode)
    }
}

impl<W: Write> Renderer<W> {
    const fn new(output: W, mode: Mode) -> Self {
        Self {
            output,
            mode,
            active_line: false,
            last_line: String::new(),
            last_redraw: None,
            last_error: None,
            failures: 0,
            revision: None,
            pass: None,
            cursor: ProgressCursor::new(),
            reported_failure: None,
            printed_step: false,
            recent_output: VecDeque::new(),
            seen_condition: None,
            started: false,
            summary: None,
            download: None,
        }
    }

    /// Shows only `summary`'s lines from now on, starting with the one saying what it is doing.
    fn summarize(&mut self, summary: Summary) {
        let doing = summary.doing.clone();
        self.summary = Some(summary);
        if self.interactive() {
            let _ignored = self.show_line(&format!("→ {doing}"));
        } else {
            let _ignored = writeln!(self.output, "{doing}...");
        }
    }

    fn ready(&mut self) {
        let Some(summary) = &self.summary else {
            return;
        };
        let line = format!(
            "✓ {} ({})",
            summary.done,
            duration(u64::try_from(summary.started.elapsed().as_millis()).unwrap_or(u64::MAX))
        );
        let _ignored = self.clear_active_line().and_then(|()| writeln!(self.output, "{line}"));
    }

    /// Revision to follow from and the output already rendered.
    fn position(&self) -> (Option<Revision>, Option<OutputPosition>) {
        let output = self.pass.map(|pass| OutputPosition {
            pass,
            sequence: self.cursor.output_sequence(),
        });
        (self.revision, output)
    }

    pub(crate) fn render(&mut self, progress: &AgentProgress) {
        let _ignored = self.render_inner(progress);
    }

    /// Renders the progress of a volume's copy. A failed copy is not retried, so the command
    /// reports its failure.
    pub(crate) fn render_copy(&mut self, copy: &Provisioning) {
        let following_began = !std::mem::replace(&mut self.started, true);
        let _ignored = self.render_pass(copy, following_began, false);
    }

    pub(crate) fn finish(&mut self) {
        let _ignored = self.clear_active_line();
    }

    const fn interactive(&self) -> bool {
        matches!(self.mode, Mode::Terminal { .. })
    }

    fn render_inner(&mut self, progress: &AgentProgress) -> io::Result<()> {
        let following_began = !self.started;
        self.revision = Some(progress.revision);
        self.condition(progress)?;
        let Some(provisioning) = &progress.provisioning else {
            return Ok(());
        };
        // A stalled guest ends the wait like an invalid agent does; the command reports both itself.
        let retried = progress.status.failure.unwrap_or(FailureKind::Transient) == FailureKind::Transient
            && progress.status.unresponsive().is_none();
        self.render_pass(provisioning, following_began, retried)
    }

    /// Renders one pass's progress. `retried` tells whether agentd retries a failed pass.
    fn render_pass(&mut self, provisioning: &Provisioning, following_began: bool, retried: bool) -> io::Result<()> {
        if self.pass != Some(provisioning.pass) {
            self.pass = Some(provisioning.pass);
            self.cursor = ProgressCursor::new();
            self.download = None;
            self.printed_step = false;
            self.recent_output.clear();
            // The latest pass may have ended long before following began. It is
            // history, not progress: take its position without printing it. The
            // command asks for a pass of its own, which reports its outcome.
            if following_began
                && matches!(
                    provisioning.progress.status(),
                    OperationStatus::Succeeded | OperationStatus::Failed { .. }
                )
            {
                let _ = self.cursor.updates(&provisioning.progress);
                self.reported_failure = Some(provisioning.pass);
                return Ok(());
            }
        }
        let pass = &provisioning.progress;
        let mut latest_output = None;
        for update in self.cursor.updates(pass) {
            match update {
                Update::OutputSkipped(count) => self.skipped(count)?,
                Update::Output(line) => {
                    self.step_output(&line.text)?;
                    latest_output = Some(line.text.trim());
                }
                Update::StepFinished(step) => {
                    latest_output = None;
                    // A failed step keeps its output for the failure report.
                    if step.outcome != Outcome::Failed {
                        self.recent_output.clear();
                        self.step_completed(&step.name, step.elapsed_ms)?;
                    }
                }
                Update::PhaseFinished(phase) if phase.outcome != Outcome::Failed => {
                    self.phase_completed(&phase.phase.label, phase.outcome, phase.elapsed_ms)?;
                }
                // A failed phase is reported with its pass's failure below.
                Update::PhaseFinished(_) => {}
            }
        }
        match pass.status() {
            OperationStatus::Failed { detail } if self.reported_failure != Some(provisioning.pass) => {
                self.reported_failure = Some(provisioning.pass);
                let (phase, elapsed_ms) = pass
                    .finished()
                    .iter()
                    .rfind(|phase| phase.outcome == Outcome::Failed)
                    .map_or(("Provision sandbox", 0), |phase| {
                        (phase.phase.label.as_ref(), phase.elapsed_ms)
                    });
                self.phase_failed(phase, detail, retried, elapsed_ms)
            }
            OperationStatus::Running => {
                let Some(current) = pass.current() else {
                    return Ok(());
                };
                if let Some(line) = self.summary_line(current) {
                    // A download redraws for every chunk, so only a line that moves its count on
                    // is throttled.
                    let counted = |line: &str| line.rsplit_once(", ").map(|(head, _)| head.to_owned());
                    return if counted(&line).is_some() && counted(&line) == counted(&self.last_line) {
                        self.show_line_throttled(&line)
                    } else {
                        self.show_line(&line)
                    };
                }
                let mut line = format!("→ {}", current.phase.label);
                let Some(step) = pass.current_step() else {
                    return self.show_line(&line);
                };
                line.push_str(": ");
                line.push_str(&step.name);
                let detail = step
                    .measurement
                    .map(format_measurement)
                    .or_else(|| latest_output.map(str::to_owned));
                match detail {
                    Some(detail) => self.show_line_throttled(&format!("{line}: {detail}")),
                    None => self.show_line(&line),
                }
            }
            _ => Ok(()),
        }
    }

    /// The updating line of a summarized wait while `current` runs, saying how far the image's
    /// download is; `None` when the wait shows steps.
    fn summary_line(&mut self, current: &sandbox::progress::ActivePhase) -> Option<String> {
        let doing = &self.summary.as_ref()?.doing;
        let image = [SandboxPhase::ImageResolve, SandboxPhase::ImagePrepare]
            .iter()
            .any(|image| image.phase().id == current.phase.id);
        if !image {
            return Some(format!("→ {doing}"));
        }
        // The first step that counts bytes is the download, since nothing can be unpacked before
        // it arrives. Others run beside it and count what they have seen so far, so only the
        // download's count is shown.
        if self.download.is_none() {
            self.download = current
                .steps
                .iter()
                .find(|step| {
                    matches!(
                        step.measurement,
                        Some(Measurement {
                            unit: ProgressUnit::Bytes,
                            ..
                        })
                    )
                })
                .map(|step| step.id.clone());
        }
        let measurement = current
            .steps
            .iter()
            .find(|step| Some(&step.id) == self.download.as_ref())
            .and_then(|step| step.measurement);
        Some(measurement.map_or_else(
            || format!("→ {doing}: preparing the image"),
            |measurement| format!("→ {doing}: downloading the image, {}", format_measurement(measurement)),
        ))
    }

    /// Reports a transient failure recorded outside a sandbox pass, including
    /// one that already stood when following started. Readiness itself is the
    /// command's outcome and reported by the command.
    fn condition(&mut self, progress: &AgentProgress) -> io::Result<()> {
        let started = std::mem::replace(&mut self.started, true);
        let Some(ready) = progress.status.ready_condition() else {
            return Ok(());
        };
        let detail = ready.detail();
        if self.seen_condition.as_deref() == Some(detail.as_str()) {
            return Ok(());
        }
        self.seen_condition = Some(detail.clone());
        let failing = progress.status.failure == Some(FailureKind::Transient);
        // A failed pass reports the failure itself, also when following begins after it.
        let explained_by_pass = progress.provisioning.as_ref().is_some_and(|provisioning| {
            started || matches!(provisioning.progress.status(), OperationStatus::Failed { .. })
        });
        if failing && !explained_by_pass {
            self.clear_active_line()?;
            self.error(&detail)?;
        }
        Ok(())
    }

    fn phase_completed(&mut self, label: &str, outcome: Outcome, elapsed_ms: u64) -> io::Result<()> {
        if self.summary.is_some() {
            return Ok(());
        }
        self.clear_active_line()?;
        let printed_step = std::mem::take(&mut self.printed_step);
        if !self.interactive() {
            return if outcome == Outcome::Reused {
                writeln!(self.output, "✓ {label} (reused)")
            } else {
                writeln!(self.output, "✓ {label} ({})", duration(elapsed_ms))
            };
        }
        if outcome != Outcome::Reused && (elapsed_ms >= NOTABLE_PHASE_MS || printed_step) {
            writeln!(self.output, "✓ {label} ({})", duration(elapsed_ms))?;
        }
        Ok(())
    }

    fn step_completed(&mut self, name: &str, elapsed_ms: u64) -> io::Result<()> {
        if self.summary.is_some() {
            return Ok(());
        }
        self.clear_active_line()?;
        if !self.interactive() || elapsed_ms >= NOTABLE_STEP_MS {
            self.printed_step = true;
            writeln!(self.output, "  ✓ {name} ({})", duration(elapsed_ms))?;
        }
        Ok(())
    }

    /// Closes the failed phase.
    ///
    /// An invalid configuration fails the command, which reports the error
    /// itself. A transient failure is explained once, with the failed step's
    /// last output; on a terminal further identical failures only advance a
    /// counter on the updating line, because the daemon retries every pass.
    fn phase_failed(&mut self, label: &str, detail: &str, retried: bool, elapsed_ms: u64) -> io::Result<()> {
        self.clear_active_line()?;
        if !retried {
            return writeln!(self.output, "✗ {label} ({})", duration(elapsed_ms));
        }
        // The first failed pass this command observes is always explained in full,
        // even when the standing condition already named the error.
        let explain = self.failures == 0 || self.last_error.as_deref() != Some(detail);
        if explain {
            self.failures = 0;
            writeln!(self.output, "✗ {label} ({})", duration(elapsed_ms))?;
            self.last_error = None;
            self.error(detail)?;
            let recent = std::mem::take(&mut self.recent_output);
            let skip = recent.len().saturating_sub(RECENT_OUTPUT_LINES);
            for line in recent.iter().skip(skip) {
                writeln!(self.output, "    {line}")?;
            }
            writeln!(
                self.output,
                "  agentd keeps retrying in the background; press Ctrl-C to stop waiting"
            )?;
        }
        self.failures += 1;
        if !self.interactive() {
            if !explain {
                writeln!(self.output, "✗ {label} ({})", duration(elapsed_ms))?;
            }
            return Ok(());
        }
        let count = self.failures;
        self.show_line(&format!(
            "✗ {label} failed {count}× (last {}); waiting for the next retry",
            duration(elapsed_ms)
        ))
    }

    fn step_output(&mut self, line: &str) -> io::Result<()> {
        self.recent_output.push_back(line.trim().to_owned());
        if self.interactive() || self.summary.is_some() {
            return Ok(());
        }
        writeln!(self.output, "{line}")
    }

    fn skipped(&mut self, count: u64) -> io::Result<()> {
        if self.interactive() || self.summary.is_some() {
            return Ok(());
        }
        writeln!(self.output, "… {count} output lines skipped")
    }

    /// Prints one `error:` line, suppressing a repeat of the previous diagnostic.
    fn error(&mut self, diagnostic: &str) -> io::Result<()> {
        if self.last_error.as_deref() == Some(diagnostic) {
            return Ok(());
        }
        self.last_error = Some(diagnostic.to_owned());
        writeln!(self.output, "error: {diagnostic}")
    }

    fn show_line_throttled(&mut self, line: &str) -> io::Result<()> {
        if self.last_redraw.is_some_and(|last| last.elapsed() < REDRAW_INTERVAL) {
            return Ok(());
        }
        self.show_line(line)
    }

    /// Replaces the updating line, truncated to the terminal width; no-op without a terminal.
    fn show_line(&mut self, line: &str) -> io::Result<()> {
        let Mode::Terminal { width } = self.mode else {
            return Ok(());
        };
        let line = truncate(line, width.saturating_sub(1));
        write!(self.output, "\r\x1b[2K{line}")?;
        self.output.flush()?;
        self.last_line = line;
        self.active_line = true;
        self.last_redraw = Some(Instant::now());
        Ok(())
    }

    fn clear_active_line(&mut self) -> io::Result<()> {
        if self.interactive() && self.active_line {
            write!(self.output, "\r\x1b[2K")?;
            self.output.flush()?;
            self.active_line = false;
        }
        Ok(())
    }
}

fn truncate(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width || width < 2 {
        return text.to_owned();
    }
    let mut kept: String = text.chars().take(width - 1).collect();
    kept.push('…');
    kept
}

pub(crate) fn format_measurement(Measurement { unit, completed, total }: Measurement) -> String {
    match (unit, total) {
        (ProgressUnit::Bytes, Some(total)) => format!("{} / {}", bytes(completed), bytes(total)),
        (ProgressUnit::Bytes, None) => bytes(completed),
        (ProgressUnit::Items, Some(total)) => format!("{completed} / {total}"),
        _ => completed.to_string(),
    }
}

pub(crate) fn bytes(value: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    const GIB: u64 = MIB * 1_024;
    if value >= GIB {
        scaled(value, GIB, "GiB")
    } else if value >= MIB {
        scaled(value, MIB, "MiB")
    } else if value >= KIB {
        scaled(value, KIB, "KiB")
    } else {
        format!("{value} B")
    }
}

fn scaled(value: u64, unit: u64, suffix: &str) -> String {
    let whole = value / unit;
    let decimal = (value % unit).saturating_mul(10) / unit;
    format!("{whole}.{decimal} {suffix}")
}

pub(crate) fn duration(milliseconds: u64) -> String {
    if milliseconds >= 60_000 {
        format!("{}m {:02}s", milliseconds / 60_000, milliseconds % 60_000 / 1_000)
    } else if milliseconds >= 1_000 {
        let seconds = milliseconds / 1_000;
        let tenths = milliseconds % 1_000 / 100;
        format!("{seconds}.{tenths}s")
    } else {
        format!("{milliseconds}ms")
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use agent::{Condition, ConditionStatus, Status, progress::Provisioning, resources::Changes};
    use sandbox::{OutputStream, ProgressEvent, SandboxPhase, StepId, progress::Progress};

    use super::*;

    /// Builds successive snapshots of one agent's progress, as the daemon reports them.
    struct Daemon {
        changes: Changes,
        pass: Revision,
        progress: Progress,
        status: Status,
        steps: HashMap<&'static str, StepId>,
    }

    impl Daemon {
        fn new() -> Self {
            let changes = Changes::new();
            Self {
                pass: changes.revision(),
                changes,
                progress: Progress::new(),
                status: Status::default(),
                steps: HashMap::new(),
            }
        }

        fn apply(&mut self, event: &ProgressEvent) -> &mut Self {
            self.progress.apply(event);
            self
        }

        fn phase(&mut self, phase: SandboxPhase) -> &mut Self {
            self.apply(&ProgressEvent::PhaseStarted { phase: phase.phase() })
        }

        fn end_phase(&mut self, phase: SandboxPhase, outcome: Outcome, elapsed_ms: u64) -> &mut Self {
            self.apply(&ProgressEvent::PhaseEnded {
                phase: phase.phase(),
                outcome,
                elapsed: Duration::from_millis(elapsed_ms),
            })
        }

        fn step(&mut self, name: &'static str) -> &mut Self {
            let id = StepId::generate();
            self.steps.insert(name, id.clone());
            self.apply(&ProgressEvent::StepStarted {
                id,
                name: name.into(),
                unit: None,
                total: None,
            })
        }

        fn output(&mut self, name: &'static str, text: &str) -> &mut Self {
            let id = self.steps[name].clone();
            self.apply(&ProgressEvent::StepOutput {
                id,
                stream: OutputStream::Stderr,
                bytes: text.as_bytes().to_vec().into(),
            })
        }

        fn end_step(&mut self, name: &'static str, elapsed_ms: u64) -> &mut Self {
            let id = self.steps[name].clone();
            self.apply(&ProgressEvent::StepEnded {
                id,
                outcome: Outcome::Completed,
                elapsed: Duration::from_millis(elapsed_ms),
            })
        }

        fn fail(&mut self, detail: &str) -> &mut Self {
            // End the phase in progress at a fixed time: left to the fold, its
            // duration is measured on the clock and differs between machines.
            if let Some(phase) = self.progress.current().map(|current| current.phase.clone()) {
                self.apply(&ProgressEvent::PhaseEnded {
                    phase,
                    outcome: Outcome::Failed,
                    elapsed: Duration::ZERO,
                });
            }
            self.progress.fail(detail);
            self.status.failure = Some(FailureKind::Transient);
            self.status.conditions = vec![Condition {
                kind: Condition::READY.into(),
                status: ConditionStatus::False,
                reason: "SandboxReconcileFailed".into(),
                message: detail.into(),
                last_transition_time: None,
            }];
            self
        }

        fn succeed(&mut self) -> &mut Self {
            self.progress.succeed();
            self.status.failure = None;
            self.status.conditions = vec![Condition {
                kind: Condition::READY.into(),
                status: ConditionStatus::True,
                reason: "SandboxReady".into(),
                message: String::new(),
                last_transition_time: None,
            }];
            self
        }

        fn retry(&mut self) -> &mut Self {
            self.changes.bump();
            self.pass = self.changes.revision();
            self.progress = Progress::new();
            self
        }

        fn snapshot(&self) -> AgentProgress {
            self.changes.bump();
            AgentProgress {
                revision: self.changes.revision(),
                status: self.status.clone(),
                provisioning: Some(Provisioning {
                    pass: self.pass,
                    progress: self.progress.clone(),
                }),
            }
        }
    }

    fn lines(renderer: Renderer<Vec<u8>>) -> Vec<String> {
        String::from_utf8(renderer.output)
            .expect("utf-8")
            .split(['\r', '\n'])
            .map(|part| part.trim_start_matches("\x1b[2K"))
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn renderer(interactive: bool) -> Renderer<Vec<u8>> {
        let mode = if interactive {
            Mode::Terminal { width: 80 }
        } else {
            Mode::Plain
        };
        Renderer::new(Vec::new(), mode)
    }

    #[test]
    fn a_copy_shows_the_bytes_copied_and_fails_without_a_retry_hint() {
        use agent::volumes::{COPY_SOURCE, VERIFY_SOURCE};

        let pass = Changes::new().revision();
        let copy = |progress: &Progress| Provisioning {
            pass,
            progress: progress.clone(),
        };
        let mut progress = Progress::new();
        let step = StepId::generate();
        let total = Some(2 << 20);
        progress.apply(&ProgressEvent::PhaseStarted { phase: COPY_SOURCE });
        progress.apply(&ProgressEvent::StepStarted {
            id: step.clone(),
            name: "Read files".into(),
            unit: Some(ProgressUnit::Bytes),
            total,
        });
        progress.apply(&ProgressEvent::StepProgress {
            id: step.clone(),
            completed: 1 << 20,
            total,
        });
        let mut renderer = renderer(true);
        renderer.render_copy(&copy(&progress));
        progress.apply(&ProgressEvent::StepEnded {
            id: step,
            outcome: Outcome::Completed,
            elapsed: Duration::from_millis(1_200),
        });
        progress.apply(&ProgressEvent::PhaseEnded {
            phase: COPY_SOURCE,
            outcome: Outcome::Completed,
            elapsed: Duration::from_millis(1_300),
        });
        progress.apply(&ProgressEvent::PhaseStarted { phase: VERIFY_SOURCE });
        progress.apply(&ProgressEvent::PhaseEnded {
            phase: VERIFY_SOURCE,
            outcome: Outcome::Failed,
            elapsed: Duration::from_millis(40),
        });
        progress.fail("the source changed while it was copied");
        renderer.render_copy(&copy(&progress));
        renderer.finish();

        assert_eq!(
            lines(renderer),
            vec![
                "→ Copy source: Read files: 1.0 MiB / 2.0 MiB",
                "  ✓ Read files (1.2s)",
                "✓ Copy source (1.3s)",
                "✗ Check source is unchanged (40ms)",
            ],
            "the command reports why the copy failed"
        );
    }

    #[test]
    fn a_warm_ensure_leaves_no_permanent_lines_on_a_terminal() {
        let mut renderer = renderer(true);
        let mut daemon = Daemon::new();
        renderer.render(&daemon.phase(SandboxPhase::Lookup).snapshot());
        renderer.render(
            &daemon
                .end_phase(SandboxPhase::Lookup, Outcome::Reused, 0)
                .phase(SandboxPhase::Inspect)
                .snapshot(),
        );
        renderer.render(
            &daemon
                .end_phase(SandboxPhase::Inspect, Outcome::Completed, 3)
                .snapshot(),
        );
        renderer.finish();
        assert_eq!(
            lines(renderer),
            vec!["→ Look up sandbox", "→ Inspect sandbox"],
            "only transient redraws"
        );
    }

    #[test]
    fn noticeable_work_and_failures_leave_lines_on_a_terminal() {
        let mut renderer = renderer(true);
        let mut daemon = Daemon::new();
        daemon
            .phase(SandboxPhase::ImageResolve)
            .step("Check Docker Engine")
            .end_step("Check Docker Engine", 2)
            .step("Build Docker image");
        renderer.render(&daemon.snapshot());
        daemon
            .end_step("Build Docker image", 74_000)
            .end_phase(SandboxPhase::ImageResolve, Outcome::Completed, 87_000)
            .phase(SandboxPhase::SandboxStart)
            .step("Start Microsandbox VM")
            .output("Start Microsandbox VM", "opening disk\nno such file\n");
        renderer.render(&daemon.snapshot());
        daemon.fail("VMDK missing");
        renderer.render(&daemon.snapshot());
        daemon.retry().phase(SandboxPhase::SandboxStart).fail("VMDK missing");
        renderer.render(&daemon.snapshot());
        renderer.finish();

        let lines = lines(renderer);
        let permanent: Vec<_> = lines.iter().filter(|line| !line.starts_with('→')).collect();
        assert_eq!(
            permanent,
            vec![
                "  ✓ Build Docker image (1m 14s)",
                "✓ Resolve sandbox image (1m 27s)",
                "✗ Start sandbox (0ms)",
                "error: VMDK missing",
                "    opening disk",
                "    no such file",
                "  agentd keeps retrying in the background; press Ctrl-C to stop waiting",
                "✗ Start sandbox failed 1× (last 0ms); waiting for the next retry",
                "✗ Start sandbox failed 2× (last 0ms); waiting for the next retry",
            ]
        );
    }

    fn summarized(interactive: bool) -> Renderer<Vec<u8>> {
        let mut renderer = renderer(interactive);
        renderer.summarize(Summary {
            doing: "Getting ready to sign in to GitHub".into(),
            done: "Ready to sign in to GitHub".into(),
            started: Instant::now(),
        });
        renderer
    }

    impl Daemon {
        /// Starts the step `name` counting bytes, or moves its count on.
        fn count(&mut self, name: &'static str, completed: u64, total: u64) -> &mut Self {
            if !self.steps.contains_key(name) {
                let id = StepId::generate();
                self.steps.insert(name, id.clone());
                self.apply(&ProgressEvent::StepStarted {
                    id,
                    name: name.into(),
                    unit: Some(ProgressUnit::Bytes),
                    total: Some(total),
                });
            }
            let id = self.steps[name].clone();
            self.apply(&ProgressEvent::StepProgress {
                id,
                completed,
                total: Some(total),
            })
        }
    }

    #[test]
    fn a_summary_shows_one_line_with_the_download_then_one_once_ready() {
        let mut renderer = summarized(true);
        let mut daemon = Daemon::new();
        renderer.render(
            &daemon
                .phase(SandboxPhase::ImageResolve)
                .count("Download image layers", 1 << 20, 2 << 30)
                .snapshot(),
        );
        // Unpacking starts on the first layers while the rest download, and counts only those.
        renderer.render(&daemon.count("Materialize image layers", 32, 32).snapshot());
        // A download's count is redrawn at most every REDRAW_INTERVAL.
        renderer.last_redraw = None;
        renderer.render(&daemon.count("Download image layers", 1 << 30, 2 << 30).snapshot());
        daemon.end_step("Download image layers", 74_000);
        renderer.render(&daemon.count("Materialize image layers", 1 << 30, 3 << 30).snapshot());
        daemon
            .end_step("Materialize image layers", 3_000)
            .end_phase(SandboxPhase::ImageResolve, Outcome::Completed, 77_000)
            .phase(SandboxPhase::SandboxStart)
            .step("Create Microsandbox VM");
        renderer.render(&daemon.snapshot());
        daemon
            .end_step("Create Microsandbox VM", 1_500)
            .end_phase(SandboxPhase::SandboxStart, Outcome::Completed, 1_600)
            .succeed();
        renderer.render(&daemon.snapshot());
        renderer.finish();
        renderer.ready();

        let lines = lines(renderer);
        let (done, shown) = lines.split_last().expect("lines");
        assert_eq!(
            shown,
            [
                "→ Getting ready to sign in to GitHub",
                "→ Getting ready to sign in to GitHub: downloading the image, 1.0 MiB / 2.0 GiB",
                "→ Getting ready to sign in to GitHub: downloading the image, 1.0 GiB / 2.0 GiB",
                "→ Getting ready to sign in to GitHub: preparing the image",
                "→ Getting ready to sign in to GitHub",
            ]
        );
        assert!(done.starts_with("✓ Ready to sign in to GitHub ("), "{done}");
    }

    #[test]
    fn a_summary_still_shows_a_failure_in_full() {
        let mut renderer = summarized(true);
        let mut daemon = Daemon::new();
        daemon
            .phase(SandboxPhase::SandboxStart)
            .step("Start Microsandbox VM")
            .output("Start Microsandbox VM", "opening disk\nno such file\n");
        renderer.render(&daemon.snapshot());
        daemon.fail("VMDK missing");
        renderer.render(&daemon.snapshot());
        renderer.finish();

        let lines = lines(renderer);
        let permanent: Vec<_> = lines.iter().filter(|line| !line.starts_with('→')).collect();
        assert_eq!(
            permanent,
            vec![
                "✗ Start sandbox (0ms)",
                "error: VMDK missing",
                "    opening disk",
                "    no such file",
                "  agentd keeps retrying in the background; press Ctrl-C to stop waiting",
                "✗ Start sandbox failed 1× (last 0ms); waiting for the next retry",
            ]
        );
    }

    #[test]
    fn without_a_terminal_a_summary_prints_what_it_gets_ready_and_when_it_is() {
        let mut renderer = summarized(false);
        let mut daemon = Daemon::new();
        daemon
            .phase(SandboxPhase::SandboxStart)
            .step("Create Microsandbox VM")
            .output("Create Microsandbox VM", "created\n")
            .end_step("Create Microsandbox VM", 366)
            .end_phase(SandboxPhase::SandboxStart, Outcome::Completed, 367);
        renderer.render(&daemon.snapshot());
        renderer.finish();
        renderer.ready();
        let lines = lines(renderer);
        assert_eq!(lines[0], "Getting ready to sign in to GitHub...");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].starts_with("✓ Ready to sign in to GitHub ("), "{lines:?}");
    }

    #[test]
    fn without_a_terminal_every_completion_and_output_line_is_printed_once() {
        let mut renderer = renderer(false);
        let mut daemon = Daemon::new();
        renderer.render(&daemon.phase(SandboxPhase::Lookup).snapshot());
        daemon
            .end_phase(SandboxPhase::Lookup, Outcome::Reused, 0)
            .phase(SandboxPhase::SandboxStart)
            .step("Create Microsandbox VM")
            .output("Create Microsandbox VM", "created\n");
        renderer.render(&daemon.snapshot());
        renderer.render(&daemon.snapshot());
        daemon
            .end_step("Create Microsandbox VM", 366)
            .end_phase(SandboxPhase::SandboxStart, Outcome::Completed, 367);
        renderer.render(&daemon.snapshot());
        renderer.finish();
        assert_eq!(
            lines(renderer),
            vec![
                "✓ Look up sandbox (reused)",
                "created",
                "  ✓ Create Microsandbox VM (366ms)",
                "✓ Start sandbox (367ms)",
            ]
        );
    }

    #[test]
    fn a_pass_that_succeeded_before_following_began_is_not_replayed() {
        let mut renderer = renderer(false);
        let mut daemon = Daemon::new();
        daemon
            .phase(SandboxPhase::SandboxStart)
            .step("Create Microsandbox VM")
            .output("Create Microsandbox VM", "created\n")
            .end_step("Create Microsandbox VM", 366)
            .end_phase(SandboxPhase::SandboxStart, Outcome::Completed, 367)
            .succeed();
        renderer.render(&daemon.snapshot());
        renderer.render(&daemon.snapshot());
        daemon
            .retry()
            .phase(SandboxPhase::SandboxStart)
            .step("Create Microsandbox VM")
            .end_step("Create Microsandbox VM", 12)
            .end_phase(SandboxPhase::SandboxStart, Outcome::Completed, 13);
        renderer.render(&daemon.snapshot());
        renderer.finish();
        assert_eq!(
            lines(renderer),
            vec!["  ✓ Create Microsandbox VM (12ms)", "✓ Start sandbox (13ms)"],
            "only the pass that ran while following is printed"
        );
    }

    #[test]
    fn a_pass_that_failed_before_following_began_is_not_replayed() {
        let mut renderer = renderer(false);
        let mut daemon = Daemon::new();
        daemon.phase(SandboxPhase::SandboxStart).fail("VMDK missing");
        renderer.render(&daemon.snapshot());
        daemon
            .retry()
            .phase(SandboxPhase::SandboxStart)
            .fail("VMDK still missing");
        renderer.render(&daemon.snapshot());
        renderer.finish();
        let lines = lines(renderer);
        assert!(!lines.iter().any(|line| line == "error: VMDK missing"), "{lines:#?}");
        assert_eq!(
            lines.first().map(String::as_str),
            Some("✗ Start sandbox (0ms)"),
            "the command's own pass reports its failure: {lines:#?}"
        );
    }

    #[test]
    fn a_stalled_guest_ends_the_wait_without_a_retry_hint() {
        let mut renderer = renderer(false);
        let mut daemon = Daemon::new();
        renderer.render(&daemon.snapshot());
        daemon
            .retry()
            .phase(SandboxPhase::SandboxStart)
            .fail("the guest has not reported progress for more than 15s");
        daemon.status.conditions.push(Condition {
            kind: Condition::SANDBOX_RESPONSIVE.into(),
            status: ConditionStatus::False,
            reason: "HeartbeatStale".into(),
            message: "the guest has not reported progress for more than 15s".into(),
            last_transition_time: None,
        });
        renderer.render(&daemon.snapshot());
        renderer.finish();
        assert_eq!(lines(renderer), ["✗ Start sandbox (0ms)"]);
    }

    #[test]
    fn a_standing_failure_is_reported_once_when_following_starts() {
        let mut renderer = renderer(false);
        let mut daemon = Daemon::new();
        daemon.fail("registry unavailable");
        let mut standing = daemon.snapshot();
        standing.provisioning = None;
        renderer.render(&standing);
        renderer.render(&standing);
        assert_eq!(lines(renderer), vec!["error: registry unavailable"]);
    }

    #[test]
    fn the_updating_line_is_truncated_to_the_terminal_width() {
        let mut renderer = Renderer::new(Vec::new(), Mode::Terminal { width: 24 });
        let mut daemon = Daemon::new();
        daemon.phase(SandboxPhase::ImageResolve).step("Build Docker image");
        renderer.render(&daemon.snapshot());
        let lines = lines(renderer);
        assert_eq!(lines.last().map(String::as_str), Some("→ Resolve sandbox imag…"));
    }

    #[test]
    fn replies_trimmed_to_unseen_output_render_nothing_twice_over_the_wire() {
        let mut renderer = renderer(false);
        let mut daemon = Daemon::new();
        daemon
            .phase(SandboxPhase::ImageResolve)
            .step("Build")
            .output("Build", "one\n")
            .end_step("Build", 700)
            .step("Import");
        renderer.render(&daemon.snapshot());
        for completed in [1_u64, 2, 3] {
            let id = daemon.steps["Import"].clone();
            daemon.apply(&ProgressEvent::StepOutput {
                id,
                stream: OutputStream::Stdout,
                bytes: format!("layer {completed}\n").into_bytes().into(),
            });
            let (_, output) = renderer.position();
            let mut reply = daemon.snapshot();
            if let (Some(provisioning), Some(output)) = (reply.provisioning.as_mut(), output) {
                provisioning.progress = provisioning.progress.output_from(output.sequence);
            }
            // Replies reach the renderer through the wire format.
            let reply = serde_json::from_value(serde_json::to_value(&reply).expect("reply JSON")).expect("reply");
            renderer.render(&reply);
            let (_, output) = renderer.position();
            let mut quiet = daemon.snapshot();
            if let (Some(provisioning), Some(output)) = (quiet.provisioning.as_mut(), output) {
                provisioning.progress = provisioning.progress.output_from(output.sequence);
            }
            let quiet = serde_json::from_value(serde_json::to_value(&quiet).expect("reply JSON")).expect("reply");
            renderer.render(&quiet);
        }
        assert_eq!(
            lines(renderer),
            vec!["one", "  ✓ Build (700ms)", "layer 1", "layer 2", "layer 3"]
        );
    }

    #[test]
    fn the_follow_position_names_the_rendered_pass_and_output() {
        let mut renderer = renderer(false);
        let mut daemon = Daemon::new();
        daemon
            .phase(SandboxPhase::ImageResolve)
            .step("Build")
            .output("Build", "one\ntwo\n");
        let snapshot = daemon.snapshot();
        renderer.render(&snapshot);
        assert_eq!(
            renderer.position(),
            (
                Some(snapshot.revision),
                Some(OutputPosition {
                    pass: daemon.pass,
                    sequence: 2
                })
            )
        );
    }
}
