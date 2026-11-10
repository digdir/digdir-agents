//! What a Dockerfile builds on and copies from, read from its instructions without building it.

/// The images that the stage `target` builds on and copies from, together with those of the stages it
/// does so from, in the order the Dockerfile names them. Without a target it is the last stage, which
/// a build without one builds.
///
/// An image named through a build argument, such as `${BASE}`, is left out, since only a build
/// knows its value.
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
    for instruction in instructions(dockerfile) {
        let mut words = instruction.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        if keyword.eq_ignore_ascii_case("FROM") {
            let mut arguments = words.filter(|word| !word.starts_with("--"));
            let Some(from) = arguments.next() else {
                continue;
            };
            let name = arguments
                .next()
                .filter(|word| word.eq_ignore_ascii_case("AS"))
                .and_then(|_| arguments.next())
                .map(str::to_owned);
            stages.push(Stage {
                from: from.to_owned(),
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
    fn what_only_a_build_knows_is_left_out() {
        assert_eq!(images(DOCKERFILE, Some("unused")), ["ghcr.io/example/never:1"]);
        assert_eq!(images("FROM scratch\n", None), Vec::<String>::new());
        assert_eq!(images(DOCKERFILE, Some("missing")), Vec::<String>::new());
        assert_eq!(images("", None), Vec::<String>::new());
    }
}
