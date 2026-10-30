//! Versioned Control API with JSON-RPC 2.0/JSONL and replaceable stream transports.

mod client;
mod protocol;
mod server;
mod socket;
mod streams;

pub use client::{Client, Connection, Connector};
pub use protocol::{DaemonInfo, PROTOCOL_VERSION, ResponseError};
pub use server::{
    AgentApi, AuthenticationApi, ConvergenceApi, ErrorHandler, ExecutionApi, GuestTcpApi, Server, SessionApi,
    SshAccessApi, VncAccessApi,
};
pub use streams::{ByteStream, Detached, GuestTcpTarget, ServedConnection, StreamConnection};
