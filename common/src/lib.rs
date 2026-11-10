//! Building blocks with no domain meaning that more than one of the workspace's crates uses.
//!
//! Code about sandboxes or agents belongs in their crates. What is here would fit in the
//! standard library or Tokio, if it matched how the workspace runs, such as on one thread.

pub mod blocking;
pub mod fs;
pub mod hex;
