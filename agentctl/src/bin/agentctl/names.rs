//! Agent names offered to people, numbered past the ones already taken.

/// `name`, or the first of `name-1`, `name-2`, … that `taken` does not claim and that still fits
/// a sandbox name. `name` itself is returned when every numbered name is taken or too long.
pub(crate) fn numbered_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    (1..=u32::MAX)
        .map(|number| format!("{name}-{number}"))
        .take_while(|numbered| numbered.len() <= ::sandbox::MAX_SANDBOX_NAME_BYTES)
        .find(|numbered| !taken(numbered))
        .unwrap_or_else(|| name.to_owned())
}

/// `name` when `taken` does not claim it, else the first free numbered name.
pub(crate) fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if taken(name) {
        numbered_name(name, taken)
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{free_name, numbered_name};

    #[test]
    fn a_taken_name_is_numbered_past_the_taken_numbers() {
        let taken = ["worker", "worker-1"];
        let is_taken = |name: &str| taken.contains(&name);
        assert_eq!(numbered_name("worker", is_taken), "worker-2");
        assert_eq!(free_name("worker", is_taken), "worker-2");
        assert_eq!(free_name("other", is_taken), "other");
    }
}
