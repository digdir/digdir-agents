//! The names the new session form gives a session you leave unnamed.

use agent::sessions::SessionName;
use rand_core::{OsRng, RngCore as _};

const ADJECTIVES: &[&str] = &[
    "agile", "amber", "bashful", "bouncy", "brave", "breezy", "bubbly", "cheeky", "chirpy", "clever", "cosmic", "cozy",
    "crispy", "curious", "dapper", "dizzy", "dreamy", "fancy", "fearless", "fizzy", "fluffy", "frosty", "fuzzy",
    "gentle", "giddy", "glossy", "goofy", "grumpy", "happy", "jazzy", "jolly", "jumpy", "lanky", "lucky", "merry",
    "mighty", "misty", "nimble", "peppy", "perky", "plucky", "polite", "quirky", "rusty", "sleepy", "snappy",
    "squeaky", "sunny", "swift", "tiny", "wacky", "wiggly", "wobbly", "zany", "zesty",
];

const NOUNS: &[&str] = &[
    "alpaca", "badger", "bagel", "banana", "beetle", "burrito", "cactus", "capybara", "cupcake", "dumpling", "ferret",
    "flamingo", "gecko", "gerbil", "giraffe", "hamster", "hedgehog", "kettle", "kiwi", "koala", "lemur", "llama",
    "lobster", "mango", "marmot", "meerkat", "moose", "muffin", "narwhal", "noodle", "octopus", "otter", "pancake",
    "panda", "parsnip", "pelican", "penguin", "pickle", "platypus", "pretzel", "puffin", "quokka", "raccoon", "radish",
    "sloth", "squid", "teapot", "toucan", "turnip", "waffle", "walrus", "wombat", "yak", "zucchini",
];

const PAIRS: usize = ADJECTIVES.len() * NOUNS.len();

/// A random `adjective-noun` name that is not `taken`, or `None` once every pair is.
pub(crate) fn random(taken: impl Fn(&str) -> bool) -> Option<SessionName> {
    free_from(usize::try_from(OsRng.next_u32()).unwrap_or_default(), taken)
}

/// The first pair not `taken`, counting from the pair at `start`.
fn free_from(start: usize, taken: impl Fn(&str) -> bool) -> Option<SessionName> {
    (0..PAIRS)
        .map(|offset| pair((start % PAIRS + offset) % PAIRS))
        .find(|name| !taken(name))
        .and_then(|name| SessionName::new(name).ok())
}

fn pair(index: usize) -> String {
    format!("{}-{}", ADJECTIVES[index / NOUNS.len()], NOUNS[index % NOUNS.len()])
}

/// Every name this module gives.
#[cfg(test)]
pub(super) fn all() -> impl Iterator<Item = String> {
    (0..PAIRS).map(pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pair_is_a_valid_session_name() {
        for name in all() {
            assert!(SessionName::new(name.clone()).is_ok(), "{name}");
        }
    }

    #[test]
    fn a_name_is_never_one_that_is_taken() {
        let free = pair(PAIRS / 2);
        for start in [0, PAIRS / 2 + 1, PAIRS - 1, PAIRS, usize::MAX] {
            assert_eq!(
                free_from(start, |name| name != free).map(String::from),
                Some(free.clone()),
                "from {start}"
            );
        }
        assert_eq!(free_from(0, |_| true), None);
    }

    #[test]
    fn the_name_starts_from_the_random_pair() {
        for start in [0, 1, NOUNS.len(), PAIRS - 1] {
            assert_eq!(free_from(start, |_| false).map(String::from), Some(pair(start)));
        }
    }
}
