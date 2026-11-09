//! Definition references: what names a definition for agentd to find, as a reference names an image.

use crate::Error;

/// The namespace of the definitions agentd carries in its build, as in `bundled/agent-builder`.
pub const BUNDLED: &str = "bundled";

/// A definition reference, parsed.
///
/// A reference starts with where the definition is, then names it, as an image reference does. A
/// first segment holding a `.` or a `:`, or `localhost`, is a host, as in Docker's rule, such as
/// `kihub.digdir.no/dotnet-dev`. agentd resolves none of those yet. `bundled/<name>` is a
/// definition agentd carries, and a plain name is short for it, so `agent-builder` is
/// `bundled/agent-builder`. A plain name never means a definition on a host, so no host can take
/// over a name by publishing one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefinitionReference {
    /// A definition agentd carries in its build, by name.
    Bundled(String),
}

impl DefinitionReference {
    /// Parses `reference`, in its full form or as a plain name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when the reference is malformed or names a definition on a host,
    /// which agentd does not resolve yet.
    pub fn parse(reference: &str) -> Result<Self, Error> {
        let name = match reference.split_once('/') {
            None => reference,
            Some((BUNDLED, name)) => name,
            Some((first, _)) if first.contains(['.', ':']) || first == "localhost" => {
                return Err(Error::Invalid(format!(
                    "definition reference {reference:?} names a definition on {first}, and agentd runs only \
                     the ones it carries yet, such as `{BUNDLED}/agent-builder`"
                )));
            }
            Some((first, _)) => {
                return Err(Error::Invalid(format!(
                    "definition reference {reference:?} starts with {first:?}, which is neither a host, such as \
                     `kihub.digdir.no`, nor `{BUNDLED}`; write ./{reference} for a folder"
                )));
            }
        };
        ::sandbox::SandboxName::new(name.to_owned())
            .map_err(|error| Error::Invalid(format!("definition reference {reference:?}: its name {error}")))?;
        Ok(Self::Bundled(name.to_owned()))
    }

    /// The definition's own name, the last part of the reference, which its agents are named after.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Bundled(name) => name,
        }
    }
}

impl std::fmt::Display for DefinitionReference {
    /// Writes the reference in its full form, as agentd records and lists it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bundled(name) => write!(formatter, "{BUNDLED}/{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DefinitionReference;

    #[test]
    fn a_plain_name_is_short_for_a_bundled_definition() {
        for reference in ["agent-builder", "bundled/agent-builder"] {
            let parsed = DefinitionReference::parse(reference).expect(reference);
            assert_eq!(parsed, DefinitionReference::Bundled("agent-builder".into()));
            assert_eq!(parsed.to_string(), "bundled/agent-builder");
            assert_eq!(parsed.name(), "agent-builder");
        }
    }

    #[test]
    fn a_host_or_another_namespace_is_not_resolved_yet() {
        for (reference, expected) in [
            ("kihub.digdir.no/dotnet-dev", "names a definition on kihub.digdir.no"),
            ("localhost:5000/dev", "names a definition on localhost:5000"),
            ("localhost/dev", "names a definition on localhost"),
            ("agents/reviewer", "write ./agents/reviewer for a folder"),
            ("bundled/Agent", "its name"),
            ("bundled/agent-builder:v1", "its name"),
            ("bundled/a/b", "its name"),
            ("", "its name"),
        ] {
            let error = DefinitionReference::parse(reference).expect_err(reference);
            assert!(error.to_string().contains(expected), "{reference}: {error}");
        }
    }
}
