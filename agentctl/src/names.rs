//! The names things are kept under on this machine, such as an account's login or a secret,
//! which manifests use to ask for them.

/// The longest name.
pub(crate) const MAX_LENGTH: usize = 64;
/// What a name must be, said without repeating the value, which may be a misplaced credential.
pub const RULE: &str = "the name must be letters, digits and inner hyphens, at most 64 characters";

/// Whether `name` can name something kept: ASCII letters, digits and inner hyphens, at most 64
/// characters. Names become parts of secret names, so nothing else passes. They are compared in
/// lowercase.
#[must_use]
pub fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_LENGTH
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::valid;

    #[test]
    fn names_are_letters_digits_and_inner_hyphens() {
        for name in ["digdir", "Digdir", "a", "a-b", "x2", "altinn-studio", &"a".repeat(64)] {
            assert!(valid(name), "{name}");
        }
        for name in [
            "",
            "../x",
            "..",
            "a/b",
            "-a",
            "a-",
            "a_b",
            "a.b",
            "a b",
            "a%2fb",
            "æ",
            &"a".repeat(65),
        ] {
            assert!(!valid(name), "{name}");
        }
    }
}
