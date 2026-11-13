//! The board's identity facts as the bridge publishes them: each one a topic `hello.<fact>`
//! (`docs/identity.md`), which Orion's hello document carries.
//!
//! Two sources make the list. The identity device (`system.identity/1`) serves its facts as
//! `name=value` lines, read again by [`crate::source`] on a timer. `offers` is computed here from
//! the devices lemnosd lists: the contract ids of the devices that are running now, distinct and
//! sorted, space-separated. The driver cannot see the other devices, so this is not its job.
//!
//! The limits are enforced here, once, by [`document`]: at most [`MAX_FACTS`] facts, each value
//! at most [`MAX_VALUE_BYTES`] bytes, no control character, no empty value. A fact that does not
//! fit is left out and the log says why, so it is absent rather than cut short.

use crate::log;

/// The most facts one node publishes. Orion's hello document carries at most this many too.
pub const MAX_FACTS: usize = 64;
/// The longest value of one fact, in bytes. Orion's hello document leaves a longer one out.
pub const MAX_VALUE_BYTES: usize = 256;
/// What every fact's topic name starts with: `hello.<fact>`.
pub const TOPIC_PREFIX: &str = "hello.";
/// The fact that lists the contract ids the node serves.
pub const OFFERS: &str = "offers";
/// The contract the identity device serves, and the method that answers every fact.
pub const IDENTITY_TAG: &str = "system.identity/1";
pub const FACTS_METHOD: &str = "facts";
/// The buffer `facts` needs (`system.identity/1`'s `MAX_FACTS_TEXT`). An out-buffer method's
/// first argument is the capacity its caller gives, as four little-endian bytes.
pub const FACTS_CAPACITY: u32 = 2048;

/// Splits the identity device's text into `name=value` pairs, in the order given. A line with
/// no `=` is dropped, and so is a line with an empty name or value.
pub fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            (!name.is_empty() && !value.is_empty()).then(|| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// `offers`: the distinct contract ids in `tags`, sorted and separated by one space. Empty when
/// there are none. A tag with a space or a control character is not a contract id and is left
/// out.
pub fn offers<'a>(tags: impl Iterator<Item = &'a str>) -> String {
    let mut ids: Vec<&str> = tags
        .filter(|tag| !tag.is_empty() && tag.bytes().all(|b| b > b' ' && b != 0x7f))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.join(" ")
}

/// The facts to publish, in order: the identity device's facts, then `offers`. Each one that
/// does not fit the limits is left out and logged; the rest are kept.
pub fn document(identity: Option<&[(String, String)]>, offers: &str) -> Vec<(String, String)> {
    let mut facts: Vec<(String, String)> = Vec::new();
    let candidates = identity
        .unwrap_or(&[])
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .chain((!offers.is_empty()).then_some((OFFERS, offers)));
    for (name, value) in candidates {
        if facts.iter().any(|(kept, _)| kept == name) {
            log(&format!("hello.{name} not published: named twice"));
            continue;
        }
        if facts.len() >= MAX_FACTS {
            log(&format!(
                "hello.{name} not published: {MAX_FACTS} facts already"
            ));
            continue;
        }
        if let Some(why) = refusal(name, value) {
            log(&format!("hello.{name} not published: {why}"));
            continue;
        }
        facts.push((name.to_owned(), value.to_owned()));
    }
    facts
}

/// Why a fact cannot be published, or `None` when it can.
fn refusal(name: &str, value: &str) -> Option<String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        return Some("the name is not lowercase [a-z0-9.-]".to_owned());
    }
    if value.is_empty() {
        return Some("no value".to_owned());
    }
    if value.len() > MAX_VALUE_BYTES {
        return Some(format!("{} bytes; at most {MAX_VALUE_BYTES}", value.len()));
    }
    if value.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Some("the value has a control character".to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    #[test]
    fn the_identity_text_is_name_value_lines_and_nothing_else_is_kept() {
        assert_eq!(
            parse("family=alpha\nserial=0000000000abcdef\nnot a fact\n=novalue\nempty=\n"),
            [pair("family", "alpha"), pair("serial", "0000000000abcdef")],
        );
        assert!(parse("").is_empty());
    }

    #[test]
    fn offers_are_the_distinct_contract_ids_sorted_and_space_separated() {
        let tags = ["system.slots/1", "system.identity/1", "system.slots/1", ""];
        assert_eq!(offers(tags.into_iter()), "system.identity/1 system.slots/1");
        assert_eq!(offers(std::iter::empty()), "");
        // A tag that is not one word is not a contract id.
        assert_eq!(offers(["two words/1"].into_iter()), "");
    }

    #[test]
    fn every_fact_and_offers_come_out_in_order() {
        let identity = [pair("family", "alpha"), pair("version.slot", "os-2")];
        assert_eq!(
            document(Some(&identity), "system.identity/1"),
            [
                pair("family", "alpha"),
                pair("version.slot", "os-2"),
                pair("offers", "system.identity/1"),
            ],
        );
        // No identity device: offers alone.
        assert_eq!(
            document(None, "system.identity/1"),
            [pair("offers", "system.identity/1")],
        );
        // Nothing offered and nothing known: no facts at all.
        assert!(document(None, "").is_empty());
    }

    #[test]
    fn at_most_sixty_four_facts_are_published_and_the_rest_are_left_out() {
        let identity: Vec<(String, String)> = (0..70)
            .map(|n| pair(&format!("fact-{n:02}"), "x"))
            .collect();
        let facts = document(Some(&identity), "system.identity/1");
        assert_eq!(facts.len(), MAX_FACTS, "64 facts, the rest refused");
        assert_eq!(facts[0].0, "fact-00", "the first ones are kept in order");
        assert_eq!(
            facts.iter().filter(|(n, _)| n == OFFERS).count(),
            0,
            "offers past the cut is refused too"
        );
    }

    #[test]
    fn a_value_of_more_than_256_bytes_is_left_out_and_one_of_256_is_kept() {
        let longest = "a".repeat(MAX_VALUE_BYTES);
        let too_long = "a".repeat(MAX_VALUE_BYTES + 1);
        let identity = [pair("fits", &longest), pair("too-long", &too_long)];
        let facts = document(Some(&identity), "");
        assert_eq!(facts, [pair("fits", &longest)]);
    }

    #[test]
    fn a_value_with_a_control_character_or_an_empty_name_is_left_out() {
        let identity = [
            pair("bad", "line\nbreak"),
            pair("Upper", "x"),
            pair("ok", "fine"),
        ];
        assert_eq!(document(Some(&identity), ""), [pair("ok", "fine")]);
    }

    #[test]
    fn a_name_given_twice_is_published_once() {
        let identity = [pair("family", "alpha"), pair("family", "beta")];
        assert_eq!(document(Some(&identity), ""), [pair("family", "alpha")]);
    }

    #[test]
    fn a_long_offers_list_is_left_out_as_a_whole() {
        let tags: Vec<String> = (0..40).map(|n| format!("contract{n:02}/1")).collect();
        let list = offers(tags.iter().map(String::as_str));
        assert!(list.len() > MAX_VALUE_BYTES);
        assert!(document(None, &list).is_empty());
    }
}
