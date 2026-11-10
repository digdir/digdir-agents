//! What a Dockerfile builds on and copies from, read from its instructions without building it.

use std::collections::HashMap;

/// The images that the stage `target` builds on and copies from, together with those of the stages it
/// does so from, in the order the Dockerfile names them. Without a target it is the last stage, which
/// a build without one builds.
///
/// A stage that builds on an image named through a build argument, such as `FROM ${BASE}`, builds
/// on its default, as an `ARG` before the first `FROM` gives it, since agentctl passes no build
/// arguments. An image named through an argument without one is left out.
#[must_use]
pub(crate) fn images(dockerfile: &str, target: Option<&str>) -> Vec<String> {
    let stages = stages(dockerfile);
    let Some(index) = target.map_or_else(
        || stages.len().checked_sub(1),
        |target| stages.iter().position(|stage| stage.is_named(target)),
    ) else {
        return Vec::new();
    };
    let mut images = Vec::new();
    let mut visited = vec![false; stages.len()];
    visit(&stages, index, &mut visited, &mut images);
    images
}

/// A stage of a Dockerfile: what it builds on, its name, and what it copies from.
struct Stage {
    from: String,
    name: Option<String>,
    copies: Vec<String>,
}

impl Stage {
    /// Whether `reference` names this stage, which Docker matches without regard to case.
    fn is_named(&self, reference: &str) -> bool {
        self.name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(reference))
    }
}

/// Adds what the stage at `index` builds on and copies from to `images`: an image itself, or what
/// the stage it names does.
fn visit(stages: &[Stage], index: usize, visited: &mut [bool], images: &mut Vec<String>) {
    if std::mem::replace(&mut visited[index], true) {
        return;
    }
    let stage = &stages[index];
    for reference in std::iter::once(&stage.from).chain(&stage.copies) {
        // A stage is named by its name or, for COPY --from, by its number, and only an earlier one.
        let named = stages[..index]
            .iter()
            .position(|earlier| earlier.is_named(reference))
            .or_else(|| reference.parse::<usize>().ok().filter(|&number| number < index));
        match named {
            Some(earlier) => visit(stages, earlier, visited, images),
            None if reference.contains('$') || reference == "scratch" || images.contains(reference) => {}
            None => images.push(reference.clone()),
        }
    }
}

/// The stages of `dockerfile`, from its FROM and COPY --from instructions.
fn stages(dockerfile: &str) -> Vec<Stage> {
    let mut stages: Vec<Stage> = Vec::new();
    // The build arguments declared before the first stage, which only FROM can use, with their
    // defaults.
    let mut arguments = HashMap::<&str, Option<String>>::new();
    let instructions = instructions(dockerfile);
    for instruction in &instructions {
        let mut words = instruction.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        if keyword.eq_ignore_ascii_case("ARG") && stages.is_empty() {
            for declaration in words {
                let (name, default) = declaration
                    .split_once('=')
                    .map_or((declaration, None), |(name, default)| {
                        // A default can name the arguments declared before it.
                        (name, Some(expand(unquoted(default), &arguments)))
                    });
                arguments.insert(name, default);
            }
        } else if keyword.eq_ignore_ascii_case("FROM") {
            let mut operands = words.filter(|word| !word.starts_with("--"));
            let Some(from) = operands.next() else {
                continue;
            };
            let from = expand(from, &arguments);
            let name = operands
                .next()
                .filter(|word| word.eq_ignore_ascii_case("AS"))
                .and_then(|_| operands.next())
                .map(str::to_owned);
            stages.push(Stage {
                from,
                name,
                copies: Vec::new(),
            });
        } else if keyword.eq_ignore_ascii_case("COPY")
            && let Some(stage) = stages.last_mut()
        {
            stage.copies.extend(
                words
                    .take_while(|word| word.starts_with("--"))
                    .filter_map(|flag| flag.strip_prefix("--from="))
                    .map(str::to_owned),
            );
        }
    }
    stages
}

/// `reference` with the build `arguments` it names, as `$NAME`, `${NAME}` or `${NAME:-fallback}`,
/// replaced by their defaults. One that names an argument without a default, or a form this does not
/// read, is left as it is, so that it still holds a `$`.
fn expand(reference: &str, arguments: &HashMap<&str, Option<String>>) -> String {
    let mut expanded = String::new();
    let mut rest = reference;
    while let Some(start) = rest.find('$') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let (name, fallback, length) = if let Some(braced) = after.strip_prefix('{') {
            let Some(end) = braced.find('}') else {
                return reference.to_owned();
            };
            let inner = &braced[..end];
            let (name, fallback) = inner
                .split_once(":-")
                .map_or((inner, None), |(name, fallback)| (name, Some(fallback)));
            (name, fallback, end + 2)
        } else {
            let end = after
                .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
                .unwrap_or(after.len());
            (&after[..end], None, end)
        };
        let value = arguments
            .get(name)
            .cloned()
            .flatten()
            .filter(|value| !value.is_empty())
            .or_else(|| fallback.map(str::to_owned));
        match value {
            Some(value) if !name.is_empty() => expanded.push_str(&value),
            _ => return reference.to_owned(),
        }
        rest = &after[length..];
    }
    expanded.push_str(rest);
    expanded
}

/// `text` without the quotes around it, if any.
fn unquoted(text: &str) -> &str {
    ['"', '\'']
        .iter()
        .find_map(|quote| text.strip_prefix(*quote)?.strip_suffix(*quote))
        .unwrap_or(text)
}

/// The instructions of `dockerfile`, each on one line: lines a backslash continues joined, and
/// comments left out.
fn instructions(dockerfile: &str) -> Vec<String> {
    let mut instructions = Vec::new();
    let mut current = String::new();
    for line in dockerfile.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(continued) = trimmed.strip_suffix('\\') {
            current.push_str(continued);
            current.push(' ');
        } else {
            current.push_str(trimmed);
            instructions.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        instructions.push(current);
    }
    instructions
}

#[cfg(test)]
mod tests {
    use super::images;

    const DOCKERFILE: &str = "\
# syntax=docker/dockerfile:1
ARG BASE=ghcr.io/example/base:1
FROM ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:2ee5 AS minimal
RUN apt-get install --yes \\
    # what the tests need
    make
COPY --from=ghcr.io/example/tools:2 /tool /usr/local/bin/

FROM --platform=linux/amd64 ${BASE} AS unused
COPY --from=ghcr.io/example/never:1 /x /x

from minimal as desktop
COPY --chown=agent \\
    --from=ghcr.io/digdir/agentctl:v0.1.0-preview.13@sha256:5451 \\
    /agentctl /agentd /usr/local/bin/
COPY --from=0 /etc/hosts /etc/hosts
";

    #[test]
    fn a_stage_builds_on_and_copies_from_what_the_stages_it_names_do() {
        assert_eq!(
            images(DOCKERFILE, Some("Desktop")),
            [
                "ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:2ee5",
                "ghcr.io/example/tools:2",
                "ghcr.io/digdir/agentctl:v0.1.0-preview.13@sha256:5451",
            ]
        );
        assert_eq!(
            images(DOCKERFILE, Some("minimal")),
            [
                "ghcr.io/digdir/agentctl-base:ubuntu-26.04@sha256:2ee5",
                "ghcr.io/example/tools:2"
            ]
        );
        assert_eq!(
            images(DOCKERFILE, None),
            images(DOCKERFILE, Some("desktop")),
            "the last stage"
        );
    }

    #[test]
    fn a_stage_builds_on_the_default_of_the_argument_it_names() {
        let dockerfile = "\
ARG REGISTRY=ghcr.io/digdir RELEASE=\"v0.1.0-preview.14\"
ARG BASE=${REGISTRY}/agentctl-base:ubuntu-26.04-${RELEASE}
ARG UNSET
FROM $BASE AS base
FROM ${UNSET:-ghcr.io/example/fallback:1} AS fallback
FROM ${UNSET} AS unset
ARG LATE=ghcr.io/example/late:1
FROM ${LATE}
";
        assert_eq!(
            images(dockerfile, Some("base")),
            ["ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.14"]
        );
        assert_eq!(images(dockerfile, Some("fallback")), ["ghcr.io/example/fallback:1"]);
        assert_eq!(images(dockerfile, Some("unset")), Vec::<String>::new());
        assert_eq!(
            images(dockerfile, None),
            Vec::<String>::new(),
            "an argument declared in a stage is not the FROM's"
        );
    }

    #[test]
    fn what_only_a_build_knows_is_left_out() {
        assert_eq!(
            images(DOCKERFILE, Some("unused")),
            ["ghcr.io/example/base:1", "ghcr.io/example/never:1"],
            "BASE's default"
        );
        assert_eq!(images("FROM scratch\n", None), Vec::<String>::new());
        assert_eq!(images(DOCKERFILE, Some("missing")), Vec::<String>::new());
        assert_eq!(images("", None), Vec::<String>::new());
    }
}
