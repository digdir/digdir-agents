//! Labels: keys with string values that say what an agent is for, without changing what it is.
//!
//! They follow [Kubernetes' syntax](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/),
//! so tools and people who know it can read and write them.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::Error;

/// Label keys and their values.
pub type Labels = BTreeMap<String, String>;

/// The longest name part of a key, and the longest value.
const MAX_NAME: usize = 63;
/// The longest prefix of a key, a DNS subdomain.
const MAX_PREFIX: usize = 253;

/// Checks a label as Kubernetes does.
///
/// A key is an optional DNS-subdomain prefix and `/`, then a name of at most 63 alphanumerics, `-`, `_` and `.`,
/// starting and ending with an alphanumeric. A value is empty or written like a name.
///
/// # Errors
///
/// Returns `Error::Invalid` naming the label and what is wrong with it.
pub fn validate(key: &str, value: &str) -> Result<(), Error> {
    let (prefix, name) = key
        .rsplit_once('/')
        .map_or((None, key), |(prefix, name)| (Some(prefix), name));
    if let Some(prefix) = prefix
        && !is_subdomain(prefix)
    {
        return Err(Error::Invalid(format!(
            "label key {key:?}: the prefix before `/` must be a lowercase DNS subdomain of at most {MAX_PREFIX} \
             characters"
        )));
    }
    if !is_name(name) {
        return Err(Error::Invalid(format!(
            "label key {key:?}: the name must be 1 to {MAX_NAME} letters, digits, `-`, `_` or `.`, starting and ending \
             with a letter or digit"
        )));
    }
    if !value.is_empty() && !is_name(value) {
        return Err(Error::Invalid(format!(
            "label {key}={value:?}: the value must be empty, or at most {MAX_NAME} letters, digits, `-`, `_` or `.`, \
             starting and ending with a letter or digit"
        )));
    }
    Ok(())
}

/// Checks every label in `labels`.
///
/// # Errors
///
/// Returns the first label [`validate`] refuses.
pub fn validate_all(labels: &Labels) -> Result<(), Error> {
    labels.iter().try_for_each(|(key, value)| validate(key, value))
}

fn is_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    bytes.len() <= MAX_NAME
        && first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_subdomain(prefix: &str) -> bool {
    prefix.len() <= MAX_PREFIX
        && prefix.split('.').all(|part| {
            let bytes = part.as_bytes();
            bytes.len() <= MAX_NAME
                && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
                && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        })
}

/// An agent's labels as they are stored: those its manifest declares and those set from the host with
/// `apply --label` or `agentctl label`, which outlive an `apply` that does not repeat them.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoredLabels {
    /// The labels the manifest last applied declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub declared: Labels,
    /// The labels set from the host. They win over a declared label of the same key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub host: Labels,
}

impl StoredLabels {
    /// Returns the agent's labels: the declared ones, with those set from the host over them.
    #[must_use]
    pub fn effective(&self) -> Labels {
        let mut labels = self.declared.clone();
        labels.extend(self.host.clone());
        labels
    }

    /// Applies `change`.
    pub fn apply(&mut self, change: &LabelChange) {
        if let Some(declared) = &change.declared {
            self.declared.clone_from(declared);
        }
        self.host.extend(change.set.clone());
        for key in &change.remove {
            self.declared.remove(key);
            self.host.remove(key);
        }
    }
}

/// A change to an agent's labels, which a store applies to what it holds in one write, so that changes from
/// different clients do not undo one another.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LabelChange {
    /// The labels the applied manifest declares, replacing those the last one declared.
    pub declared: Option<Labels>,
    /// Labels set from the host.
    pub set: Labels,
    /// Keys removed, whether declared or set from the host. A manifest that still declares one gives it back on the
    /// next `apply`.
    pub remove: BTreeSet<String>,
}

/// Which agents a client works with, by their labels, in Kubernetes' selector syntax: requirements separated by
/// commas, all of which an agent's labels must meet.
///
/// - `key=value` or `key==value`, and `key!=value`, which an agent without the label also meets.
/// - `key in (a,b)`, and `key notin (a,b)`, which an agent without the label also meets.
/// - `key`, the label is set, and `!key`, it is not.
///
/// An empty selector selects every agent.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selector(Vec<Requirement>);

/// One requirement of a [`Selector`].
#[derive(Clone, Debug, Eq, PartialEq)]
enum Requirement {
    In(String, BTreeSet<String>),
    NotIn(String, BTreeSet<String>),
    Exists(String),
    DoesNotExist(String),
}

impl Selector {
    /// Returns whether `labels` meet every requirement.
    #[must_use]
    pub fn matches(&self, labels: &Labels) -> bool {
        self.0.iter().all(|requirement| match requirement {
            Requirement::In(key, values) => labels.get(key).is_some_and(|value| values.contains(value)),
            Requirement::NotIn(key, values) => labels.get(key).is_none_or(|value| !values.contains(value)),
            Requirement::Exists(key) => labels.contains_key(key),
            Requirement::DoesNotExist(key) => !labels.contains_key(key),
        })
    }
}

impl std::str::FromStr for Selector {
    type Err = Error;

    fn from_str(selector: &str) -> Result<Self, Self::Err> {
        let invalid =
            |requirement: &str, why: &str| Error::Invalid(format!("selector requirement {requirement:?}: {why}"));
        let key = |requirement: &str, key: &str| {
            let key = key.trim();
            validate(key, "").map_err(|_| invalid(requirement, "the key is not a valid label key"))?;
            Ok::<_, Error>(key.to_owned())
        };
        let value = |requirement: &str, key: &str, value: &str| {
            let value = value.trim();
            validate(key, value).map_err(|_| invalid(requirement, "the value is not a valid label value"))?;
            Ok::<_, Error>(value.to_owned())
        };
        let mut requirements = Vec::new();
        for requirement in split_requirements(selector)? {
            let requirement = requirement.trim();
            if requirement.is_empty() {
                if selector.trim().is_empty() {
                    continue;
                }
                return Err(invalid(requirement, "a requirement is empty"));
            }
            if let Some(open) = requirement.find('(') {
                let Some(values) = requirement[open + 1..].strip_suffix(')') else {
                    return Err(invalid(requirement, "a set must end with `)`"));
                };
                let mut words = requirement[..open].split_whitespace();
                let (Some(name), Some(operator), None) = (words.next(), words.next(), words.next()) else {
                    return Err(invalid(
                        requirement,
                        "a set requirement is `KEY in (VALUES)` or `KEY notin (VALUES)`",
                    ));
                };
                let name = key(requirement, name)?;
                let values = values
                    .split(',')
                    .map(|item| value(requirement, &name, item))
                    .collect::<Result<BTreeSet<_>, _>>()?;
                requirements.push(match operator {
                    "in" => Requirement::In(name, values),
                    "notin" => Requirement::NotIn(name, values),
                    _ => return Err(invalid(requirement, "the operator before a set is `in` or `notin`")),
                });
            } else if let Some((name, item)) = requirement.split_once("!=") {
                let name = key(requirement, name)?;
                let item = value(requirement, &name, item)?;
                requirements.push(Requirement::NotIn(name, [item].into()));
            } else if let Some((name, item)) = requirement.split_once("==").or_else(|| requirement.split_once('=')) {
                let name = key(requirement, name)?;
                let item = value(requirement, &name, item)?;
                requirements.push(Requirement::In(name, [item].into()));
            } else if let Some(name) = requirement.strip_prefix('!') {
                requirements.push(Requirement::DoesNotExist(key(requirement, name)?));
            } else {
                requirements.push(Requirement::Exists(key(requirement, requirement)?));
            }
        }
        Ok(Self(requirements))
    }
}

/// Splits a selector at the commas between requirements, leaving those inside a set.
fn split_requirements(selector: &str) -> Result<Vec<&str>, Error> {
    let mut requirements = Vec::new();
    let mut start = 0;
    let mut in_set = false;
    for (index, character) in selector.char_indices() {
        match character {
            '(' if !in_set => in_set = true,
            ')' if in_set => in_set = false,
            '(' | ')' => {
                return Err(Error::Invalid(format!(
                    "selector {selector:?}: parentheses do not match"
                )));
            }
            ',' if !in_set => {
                requirements.push(&selector[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if in_set {
        return Err(Error::Invalid(format!("selector {selector:?}: a set is not closed")));
    }
    requirements.push(&selector[start..]);
    Ok(requirements)
}

#[cfg(test)]
mod tests {
    use super::{LabelChange, Labels, StoredLabels, validate};

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }

    #[test]
    fn keys_and_values_follow_kubernetes() {
        for (key, value) in [
            ("client", "jetbrains"),
            ("managed-by", "t3code"),
            ("agentctl.digdir.no/client", "t3code"),
            ("tier", ""),
            ("a", "A_1.b-2"),
        ] {
            assert!(validate(key, value).is_ok(), "{key}={value}");
        }
        for (key, value) in [
            ("", "x"),
            ("-client", "x"),
            ("client-", "x"),
            ("Agentctl.Digdir.no/client", "x"),
            ("/client", "x"),
            ("client", "-x"),
            ("client", "has space"),
            (&"k".repeat(64), "x"),
            ("client", &"v".repeat(64)),
        ] {
            assert!(validate(key, value).is_err(), "{key}={value}");
        }
    }

    #[test]
    fn host_labels_outlive_an_apply_and_win_over_declared_ones() {
        let mut stored = StoredLabels::default();
        stored.apply(&LabelChange {
            declared: Some(labels(&[("client", "jetbrains"), ("tier", "dev")])),
            set: labels(&[("managed-by", "t3code")]),
            ..LabelChange::default()
        });
        stored.apply(&LabelChange {
            declared: Some(labels(&[("client", "t3code")])),
            ..LabelChange::default()
        });
        assert_eq!(
            stored.effective(),
            labels(&[("client", "t3code"), ("managed-by", "t3code")]),
            "an apply replaces what the manifest declared and keeps what the host set"
        );

        stored.apply(&LabelChange {
            set: labels(&[("client", "rider")]),
            ..LabelChange::default()
        });
        stored.apply(&LabelChange {
            declared: Some(labels(&[("client", "t3code")])),
            ..LabelChange::default()
        });
        assert_eq!(stored.effective()["client"], "rider");

        stored.apply(&LabelChange {
            remove: ["client".to_owned()].into(),
            ..LabelChange::default()
        });
        assert_eq!(stored.effective(), labels(&[("managed-by", "t3code")]));
    }

    #[test]
    fn selectors_follow_kubernetes() {
        let worker = labels(&[("client", "t3code"), ("managed-by", "t3code")]);
        let mine = labels(&[("client", "jetbrains")]);
        let plain = Labels::new();
        for (selector, selected) in [
            ("", [true, true, true]),
            ("client=t3code", [true, false, false]),
            ("client==jetbrains", [false, true, false]),
            ("client!=t3code", [false, true, true]),
            ("client in (t3code, jetbrains)", [true, true, false]),
            ("client notin (t3code)", [false, true, true]),
            ("managed-by", [true, false, false]),
            ("!managed-by", [false, true, true]),
            ("!managed-by,client", [false, true, false]),
            (" client in (jetbrains,rider) , !managed-by ", [false, true, false]),
        ] {
            let parsed: super::Selector = selector.parse().expect(selector);
            assert_eq!(
                [&worker, &mine, &plain].map(|labels| parsed.matches(labels)),
                selected,
                "{selector}"
            );
        }
        for invalid in [
            "client in (a",
            "client in a)",
            "client within (a)",
            "client,,tier",
            "client=has space",
            "-client",
            "client in (a,b) x",
            "!",
        ] {
            assert!(invalid.parse::<super::Selector>().is_err(), "{invalid}");
        }
    }
}
