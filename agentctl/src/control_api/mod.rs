//! Versioned Control API with JSON-RPC 2.0/JSONL and replaceable stream transports.

mod client;
mod frames;
mod protocol;
mod relay;
mod server;
mod socket;
mod terminals;

pub use client::{AgentLease, Client, Connection, Connector, Output, RemoteTerminal, TerminalInput, TerminalOutput};
pub use frames::Ended;
pub use protocol::{DaemonInfo, PROTOCOL_VERSION, ResponseError};
pub use relay::{Terminal, TerminalProcess};
pub use server::{
    AccountsApi, AgentApi, AuthenticationApi, ConvergenceApi, ErrorHandler, ExecutionApi, SecretsApi, Server,
    SessionApi, SshAccessApi, TerminalApi, VncAccessApi, VolumeApi,
};
pub use terminals::Terminals;
