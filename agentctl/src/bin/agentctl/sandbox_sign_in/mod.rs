//! Sign-ins without their programs on this machine. A short-lived agent, on an image with the
//! sign-in's program, runs it with its terminal relayed here, and its sign-in page opens in this
//! machine's browser. The credential comes back to the caller, which has agentd keep
//! it; agents only ever see placeholders.

use std::{collections::BTreeSet, io::Write as _, time::Duration};

use agent::{
    Error,
    control_api::{AgentLease, Client, Ended},
    control_plane::WaitPolicy,
    local::home::ControlPlaneHome,
    sandbox::{ExecutionTarget, TerminalSpec},
    sign_in::SandboxSignIn,
};
use futures_util::StreamExt as _;
use sandbox::execution::{ExecutionEvent, Program, StartExecutionRequest};
use zeroize::Zeroizing;

use crate::CommandResult;

/// How long a sign-in agent may take to start, which includes pulling its image the first time.
const START_TIMEOUT: Duration = Duration::from_mins(20);

/// A running sign-in agent. It is ephemeral, so agentd deletes it once this is dropped or the
/// process ends, however it ends.
struct SignInAgent<'a> {
    client: &'a Client,
    home: &'a ControlPlaneHome,
    name: String,
    target: ExecutionTarget,
    _lease: AgentLease,
}

impl<'a> SignInAgent<'a> {
    /// Runs the agent `sign_in` runs in, to sign in to `service`, and waits until commands run in
    /// it, as one line about getting the sign-in ready: the agent is how the sign-in runs, not what
    /// the person asked for.
    async fn start(
        client: &'a Client,
        home: &'a ControlPlaneHome,
        service: &str,
        sign_in: &SandboxSignIn,
    ) -> crate::CommandResult<Self> {
        let wait = crate::progress::Wait::summarized(
            format!("Getting ready to sign in to {service}"),
            format!("Ready to sign in to {service}"),
        );
        let (ran, lease) = client.run_sign_in(sign_in.definition).await?;
        let name = ran.agent.metadata.name;
        crate::converge_shown(&wait, client, &name, START_TIMEOUT).await?;
        let target = client.ensure_execution(&name, WaitPolicy::UntilConverged).await?;
        wait.ready();
        Ok(Self {
            client,
            home,
            name,
            target,
            _lease: lease,
        })
    }

    /// Runs `sign_in` with this terminal relayed to it, and returns the credential it leaves. With
    /// `open_links`, each sign-in page it shows opens in this machine's browser.
    async fn run(&self, sign_in: &SandboxSignIn, open_links: bool) -> Result<Zeroizing<String>, SignInFailure> {
        let before = |error: Error| SignInFailure {
            error: Box::new(error),
            signed_in: false,
        };
        let output = self.relay(sign_in, open_links).await.map_err(before)?;
        // The program signed in, so whatever fails now leaves a credential the host never gets.
        let after = |error: Error| SignInFailure {
            error: Box::new(error),
            signed_in: true,
        };
        let artifact = match &sign_in.artifact {
            Some(path) => Some(self.read(path.as_str()).await.map_err(after)?),
            None => None,
        };
        sign_in
            .credential(&output, artifact.as_ref().map(|artifact| artifact.as_slice()))
            .map_err(after)
    }

    /// Runs `sign_in`'s program in a terminal relayed to this one until it exits, opening each sign-in page it shows
    /// in this machine's browser with `open_links`, and returns everything it printed.
    async fn relay(&self, sign_in: &SandboxSignIn, open_links: bool) -> Result<Zeroizing<Vec<u8>>, Error> {
        let Program::Command { executable, args } = sign_in.execution.program() else {
            return Err(Error::Invalid("a sign-in runs a command".into()));
        };
        let spec = TerminalSpec {
            command: std::iter::once(executable.as_str().to_owned())
                .chain(args.iter().cloned())
                .collect(),
            environment: sign_in.execution.environment().clone(),
            working_directory: sign_in
                .execution
                .working_directory()
                .map(|directory| directory.as_str().to_owned()),
        };
        let terminal = self
            .client
            .open_terminal(&self.name, spec, crate::terminal::size())
            .await?;
        let mut output = Zeroizing::new(Vec::new());
        let mut opened = BTreeSet::new();
        let ended = crate::terminal::relay(terminal, async |bytes: &[u8]| {
            output.extend_from_slice(bytes);
            if open_links {
                open_new_links(sign_in, &output, &mut opened).await;
            }
        })
        .await?;
        match ended {
            Ended::Exited(0) => Ok(output),
            Ended::Exited(code) => Err(Error::Invalid(format!(
                "the sign-in ended without signing in (exit code {code})"
            ))),
            Ended::Detached => Err(Error::Invalid("the sign-in ended without signing in".into())),
            Ended::Error(message) => Err(Error::Invalid(format!(
                "the sign-in ended without signing in: {message}"
            ))),
        }
    }

    /// The contents of a file in the sign-in agent.
    async fn read(&self, path: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
        let spec = agent::sandbox::platform::execution_spec(
            &self.target.operating_system,
            &["/bin/cat".into(), path.into()],
            false,
        )?;
        let mut execution =
            agent::sandbox::start_execution(self.home.path(), &self.target, StartExecutionRequest::new(spec)).await?;
        let mut contents = Zeroizing::new(Vec::new());
        while let Some(event) = execution.events.next().await {
            match event? {
                ExecutionEvent::Stdout(bytes) => contents.extend_from_slice(&bytes),
                ExecutionEvent::Exited(status) if status.success() => return Ok(contents),
                ExecutionEvent::Exited(_) => {
                    return Err(Error::Invalid("the sign-in left no credential file".into()));
                }
                ExecutionEvent::Failed { message } => return Err(Error::Invalid(message)),
                _ => {}
            }
        }
        Err(Error::Invalid("the sign-in agent stopped answering".into()))
    }
}

/// Opens each sign-in page `output` shows that is not in `opened` yet in this machine's browser.
async fn open_new_links(sign_in: &SandboxSignIn, output: &[u8], opened: &mut BTreeSet<String>) {
    for link in sign_in.links(output) {
        if !opened.insert(link.to_string()) {
            continue;
        }
        // Written among what the program draws, which it may draw over, but the person would
        // otherwise wait for a page that never opens.
        if let Err(error) = crate::launch::open_url(link.as_str()).await {
            let mut stdout = std::io::stdout();
            write!(
                stdout,
                "\r\n(The sign-in page did not open: {error}. Open the link in your browser.)\r\n"
            )
            .ok();
            stdout.flush().ok();
        }
    }
}

/// Why a sign-in gave no credential.
pub(crate) struct SignInFailure {
    pub(crate) error: Box<Error>,
    /// Whether the program had signed in, so that the service may have issued a credential this
    /// machine never got.
    pub(crate) signed_in: bool,
}

/// Runs `sign_in` in a short-lived agent to sign in to `service`, with the person at this terminal
/// and its sign-in page opening in this machine's browser. The outer result says whether the agent
/// started, the inner one what the sign-in gave. The agent is deleted, with everything signed in
/// inside it, before this returns.
pub(crate) async fn sign_in(
    client: &Client,
    home: &ControlPlaneHome,
    service: &str,
    sign_in: &SandboxSignIn,
) -> CommandResult<Result<Zeroizing<String>, SignInFailure>> {
    let agent = SignInAgent::start(client, home, service, sign_in).await?;
    // Where a browser here would not reach the person, such as over SSH, they open the link. Saying
    // so now keeps it out of what the program draws.
    let blocked = crate::launch::launch_blocked(|name| std::env::var_os(name));
    match &blocked {
        None => println!("Its sign-in page opens in your browser."),
        Some(reason) => println!("No browser here would reach you: {reason}. Open the link it shows in a browser."),
    }
    Ok(agent.run(sign_in, blocked.is_none()).await)
}
