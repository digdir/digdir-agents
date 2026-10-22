//! The building blocks of an agent folder: the base images an agent's Dockerfile starts from,
//! the toolchains its image can install and the Dockerfile blocks that install them.

pub(crate) mod image;
pub(crate) mod template;
pub(crate) mod toolchains;
