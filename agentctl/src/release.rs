//! Releases of the platform, such as `v0.1.0-preview.14`, which the `agentctl/v*` tags and the tags of
//! the images each release publishes name.

/// The repositories of the images each release publishes under a tag that names it.
const REPOSITORIES: &[&str] = &[
    "ghcr.io/digdir/agentctl-base",
    "ghcr.io/digdir/agentctl",
    "ghcr.io/digdir/agentctl-agent-builder",
];

/// The tag of `reference` when it names an image a release publishes, empty when it names one by
/// digest alone, or `None` for any other image.
#[must_use]
pub fn published_tag(reference: &str) -> Option<&str> {
    let named = reference.split_once('@').map_or(reference, |(named, _digest)| named);
    let (repository, tag) = match named.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => (repository, tag),
        _ => (named, ""),
    };
    REPOSITORIES.contains(&repository).then_some(tag)
}

/// A release, ordered as releases follow each other: its previews before it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Release {
    core: (u64, u64, u64),
    stage: Stage,
}

/// Where in its line a release is. A preview comes before the release it previews, so it orders
/// first.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Stage {
    Preview(u64),
    Final,
}

impl Release {
    /// The release `version` names exactly, such as `v0.1.0` or `v0.1.0-preview.13`, or `None` for a
    /// build of `main`, a pull request or a local one, whose versions extend a release's.
    #[must_use]
    pub fn parse(version: &str) -> Option<Self> {
        let version = version.strip_prefix('v')?;
        let (core, preview) = version
            .split_once('-')
            .map_or((version, None), |(core, prerelease)| (core, Some(prerelease)));
        let mut numbers = core.split('.').map(number);
        let core = (numbers.next()??, numbers.next()??, numbers.next()??);
        if numbers.next().is_some() {
            return None;
        }
        let stage = match preview {
            None => Stage::Final,
            Some(preview) => Stage::Preview(number(preview.strip_prefix("preview.")?)?),
        };
        Some(Self { core, stage })
    }

    /// The release the build `version` comes from: the release itself, or the one a build of `main`
    /// extends, as in `v0.1.0-preview.13.main.g1a2b3c4`. A local build names none.
    #[must_use]
    pub fn of_build(version: &str) -> Option<Self> {
        Self::parse(version).or_else(|| Self::parse(version.split_once(".main.")?.0))
    }

    /// The release a tag of an image the platform publishes names, such as
    /// `ubuntu-26.04-desktop-v0.1.0-preview.14` or `v0.1.0`. `main`'s tags, such as `ubuntu-26.04`
    /// or `main`, name none.
    #[must_use]
    pub fn of_tag(tag: &str) -> Option<Self> {
        tag.char_indices()
            .filter(|&(index, character)| character == 'v' && (index == 0 || tag[..index].ends_with('-')))
            .find_map(|(index, _)| Self::parse(&tag[index..]))
    }
}

impl std::fmt::Display for Release {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (major, minor, patch) = self.core;
        write!(formatter, "v{major}.{minor}.{patch}")?;
        match self.stage {
            Stage::Preview(number) => write!(formatter, "-preview.{number}"),
            Stage::Final => Ok(()),
        }
    }
}

/// `text` as a number written without a sign or leading zeros.
fn number(text: &str) -> Option<u64> {
    (!text.is_empty() && (text == "0" || !text.starts_with('0')) && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::Release;

    fn release(version: &str) -> Release {
        Release::parse(version).expect(version)
    }

    #[test]
    fn releases_follow_their_previews_and_each_other() {
        let ordered = [
            "v0.1.0-preview.2",
            "v0.1.0-preview.13",
            "v0.1.0-preview.14",
            "v0.1.0",
            "v0.1.1",
            "v1.0.0",
        ]
        .map(release);
        assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]), "{ordered:?}");
        assert_eq!(release("v0.1.0-preview.14").to_string(), "v0.1.0-preview.14");
    }

    #[test]
    fn only_a_release_is_one() {
        for version in [
            "0.1.0",
            "v0.1",
            "v0.1.0.1",
            "v0.1.0-rc.1",
            "v0.1.0-preview.01",
            "v0.1.0-preview.13.main.gabc1234",
            "v0.1.1-main.gabc1234",
            "v0.0.1-dev.20261108",
        ] {
            assert_eq!(Release::parse(version), None, "{version}");
        }
    }

    #[test]
    fn a_build_of_main_comes_from_the_release_it_extends() {
        assert_eq!(
            Release::of_build("v0.1.0-preview.13.main.gabc1234"),
            Some(release("v0.1.0-preview.13"))
        );
        assert_eq!(Release::of_build("v0.1.0"), Some(release("v0.1.0")));
        assert_eq!(Release::of_build("v0.0.1-dev.20261108"), None);
    }

    #[test]
    fn only_the_platforms_images_have_tags_that_name_releases() {
        assert_eq!(
            super::published_tag("ghcr.io/digdir/agentctl-base:ubuntu-26.04-v0.1.0-preview.14@sha256:3a5c"),
            Some("ubuntu-26.04-v0.1.0-preview.14")
        );
        assert_eq!(super::published_tag("ghcr.io/digdir/agentctl:main"), Some("main"));
        assert_eq!(super::published_tag("ghcr.io/digdir/agentctl@sha256:3a5c"), Some(""));
        assert_eq!(super::published_tag("ghcr.io/digdir/agentctl-other:v0.1.0"), None);
        assert_eq!(super::published_tag("localhost:5000/digdir/agentctl:v0.1.0"), None);
    }

    #[test]
    fn an_image_tag_names_the_release_it_was_published_for() {
        assert_eq!(
            Release::of_tag("ubuntu-26.04-desktop-v0.1.0-preview.14"),
            Some(release("v0.1.0-preview.14"))
        );
        assert_eq!(Release::of_tag("v0.1.0-preview.13"), Some(release("v0.1.0-preview.13")));
        for tag in [
            "ubuntu-26.04",
            "ubuntu-26.04-desktop",
            "main",
            "ubuntu-26.04-sha-abc1234",
            "dev",
        ] {
            assert_eq!(Release::of_tag(tag), None, "{tag}");
        }
    }
}
