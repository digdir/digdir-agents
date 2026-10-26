//! Selection of the process responsible for initializing a sandbox.

use serde::{Deserialize, Serialize};

/// Selects which init system owns the sandbox after backend setup.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InitSystem {
    /// Keep the sandbox Backend's built-in init process.
    #[default]
    Backend,
    /// Hand initialization to the init system supplied by the Image.
    Image,
}
