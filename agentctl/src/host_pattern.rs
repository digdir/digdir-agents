//! Host patterns, such as `api.example.com` or `*.example.com`, as manifests, stored secrets and
//! the policy engine name the hosts a rule applies to.

/// Whether `pattern` is a host pattern: a host name, or `*.` followed by one, which matches every
/// host below it.
pub(crate) fn valid(pattern: &str) -> bool {
    !pattern.is_empty()
        && !pattern.contains(['/', ':', '\\'])
        && pattern
            .strip_prefix("*.")
            .unwrap_or(pattern)
            .split('.')
            .all(|label| !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
}

/// Whether `pattern` is an IP address, which [`valid`] accepts since its labels are digits, rather
/// than a name.
pub(crate) fn is_address(pattern: &str) -> bool {
    pattern.parse::<std::net::IpAddr>().is_ok()
}

/// Whether `host` is one that `pattern` matches, ignoring case and a trailing dot.
pub(crate) fn matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    pattern.strip_prefix("*.").map_or_else(
        || host == pattern,
        |suffix| {
            host.len() > suffix.len()
                && host.ends_with(suffix)
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        },
    )
}

/// Whether every host that `inner` matches is one that `outer` matches too.
pub(crate) fn covers(outer: &str, inner: &str) -> bool {
    // `*.suffix` is every host below the domain `suffix`: covered by the same pattern, or by one
    // matching the domain itself as a host below its own.
    inner.strip_prefix("*.").map_or_else(
        || matches(outer, inner),
        |suffix| outer.eq_ignore_ascii_case(inner) || (outer.starts_with("*.") && matches(outer, suffix)),
    )
}

#[cfg(test)]
mod tests {
    use super::{covers, is_address, valid};

    #[test]
    fn a_pattern_covers_the_hosts_it_matches_and_the_patterns_below_it() {
        for (outer, inner) in [
            ("api.example.com", "api.example.com"),
            ("*.example.com", "api.example.com"),
            ("*.example.com", "*.example.com"),
            ("*.example.com", "*.api.example.com"),
            ("*.EXAMPLE.com", "API.example.com"),
        ] {
            assert!(covers(outer, inner), "{outer} covers {inner}");
        }
        for (outer, inner) in [
            ("api.example.com", "*.example.com"),
            ("api.example.com", "www.example.com"),
            ("*.example.com", "example.com"),
            ("*.api.example.com", "*.example.com"),
        ] {
            assert!(!covers(outer, inner), "{outer} does not cover {inner}");
        }
    }

    #[test]
    fn an_address_is_a_valid_pattern_but_not_a_name() {
        assert!(valid("203.0.113.7") && is_address("203.0.113.7"));
        assert!(is_address("2001:db8::1") && !valid("2001:db8::1"));
        assert!(valid("api.example.com") && !is_address("api.example.com"));
        assert!(!is_address("*.example.com"));
    }
}
