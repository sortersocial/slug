//! How `url::Url` behaves as `HashMap` keys (`Eq` + `Hash`).
//!
//! If `ItemId::External` stores `Url`, these tests are the contract you are buying into
//! (or the baseline before you add a custom normalization layer).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use url::Url;

fn hash_one(url: &Url) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    h.finish()
}

#[test]
fn identical_parse_strings_are_eq_and_share_hash_bucket() {
    let a = Url::parse("https://example.com/path").unwrap();
    let b = Url::parse("https://example.com/path").unwrap();
    assert_eq!(a, b);
    assert_eq!(hash_one(&a), hash_one(&b));

    let mut m: HashMap<Url, u32> = HashMap::new();
    m.insert(a, 1);
    *m.entry(b).or_default() += 10;
    assert_eq!(m.len(), 1);
    assert_eq!(m[&Url::parse("https://example.com/path").unwrap()], 11);
}

#[test]
fn host_is_ascii_lowercase_in_eq() {
    let lower = Url::parse("https://examplE.com/").unwrap();
    let upper = Url::parse("https://EXAMPLE.com/").unwrap();
    assert_eq!(lower, upper);
    assert_eq!(hash_one(&lower), hash_one(&upper));
}

#[test]
fn path_space_normalizes_to_percent_encoding_so_forms_merge() {
    let encoded = Url::parse("https://example.com/a%20b").unwrap();
    let decoded = Url::parse("https://example.com/a b").unwrap();
    // Parser normalizes both to the same internal path (`/a%20b`).
    assert_eq!(encoded, decoded);
    assert_eq!(hash_one(&encoded), hash_one(&decoded));

    let mut m: HashMap<Url, &str> = HashMap::new();
    m.insert(encoded, "first");
    assert_eq!(m.insert(decoded, "second"), Some("first"));
    assert_eq!(m.len(), 1);
    assert_eq!(m.values().next().copied(), Some("second"));
}

#[test]
fn encoded_slash_in_segment_stays_distinct_from_real_path_separator() {
    let encoded = Url::parse("https://example.com/a%2Fb").unwrap();
    let real_slash = Url::parse("https://example.com/a/b").unwrap();
    assert_ne!(encoded, real_slash);
    assert_ne!(hash_one(&encoded), hash_one(&real_slash));
}

#[test]
fn trailing_slash_on_path_is_significant_for_eq() {
    let with_slash = Url::parse("https://example.com/foo/").unwrap();
    let no_slash = Url::parse("https://example.com/foo").unwrap();
    assert_ne!(with_slash, no_slash);
    assert_ne!(hash_one(&with_slash), hash_one(&no_slash));
}

#[test]
fn default_http_port_80_is_normalized_in_representation() {
    let explicit = Url::parse("http://example.com:80/").unwrap();
    let implicit = Url::parse("http://example.com/").unwrap();
    assert_eq!(explicit, implicit);
    assert_eq!(hash_one(&explicit), hash_one(&implicit));
}

#[test]
fn default_https_port_443_is_normalized() {
    let explicit = Url::parse("https://example.com:443/foo").unwrap();
    let implicit = Url::parse("https://example.com/foo").unwrap();
    assert_eq!(explicit, implicit);
}

#[test]
fn non_default_port_is_part_of_identity() {
    let a = Url::parse("https://example.com:444/").unwrap();
    let b = Url::parse("https://example.com:445/").unwrap();
    assert_ne!(a, b);
}

#[test]
fn empty_path_vs_slash_only_path_may_differ() {
    let root = Url::parse("https://example.com").unwrap();
    let slash = Url::parse("https://example.com/").unwrap();
    // Both serialize to `https://example.com/` in practice for this crate — verify.
    assert_eq!(root, slash, "document: root and trailing-slash-only merge for this parser");
}

#[test]
fn scheme_case_is_normalized_to_lowercase() {
    let lower = Url::parse("https://example.com/").unwrap();
    let upper = Url::parse("HTTPS://example.com/").unwrap();
    assert_eq!(lower, upper);
}

#[test]
fn fragment_is_part_of_eq_and_hash() {
    let no_frag = Url::parse("https://example.com/a").unwrap();
    let frag = Url::parse("https://example.com/a#section").unwrap();
    assert_ne!(
        no_frag, frag,
        "#fragment is included in PartialEq — anchors are different HashMap keys"
    );
    assert_ne!(hash_one(&no_frag), hash_one(&frag));
}

#[test]
fn query_order_and_encoding_can_split_identity() {
    let a = Url::parse("https://example.com/?b=2&a=1").unwrap();
    let b = Url::parse("https://example.com/?a=1&b=2").unwrap();
    assert_ne!(a, b, "query pairs order is preserved in serialization");

    let plus = Url::parse("https://example.com/?q=a+b").unwrap();
    let encoded = Url::parse("https://example.com/?q=a%20b").unwrap();
    assert_ne!(plus, encoded, "space as + vs %20 — different keys unless normalized");
}
