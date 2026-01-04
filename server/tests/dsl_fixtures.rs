use slugsocial_server::dsl;

const TUTORIAL: &str = include_str!("fixtures/tutorial.sorter");
const BIG_BOOK: &str = include_str!("fixtures/big-book.sorter");

#[test]
fn parses_tutorial_fixture_with_prose() {
    // This doc is intentionally "backwards compatible with prose".
    // We want to confirm we still extract key DSL statements.
    let doc = dsl::parse_full(TUTORIAL);

    let mut hashtags = 0usize;
    let mut items = 0usize;
    let mut votes = 0usize;
    let mut attrs = 0usize;
    let mut prose = 0usize;

    for s in &doc.statements {
        match s {
            dsl::Stmt::Hashtag { .. } => hashtags += 1,
            dsl::Stmt::Actor { .. } => {}
            dsl::Stmt::Item { .. } => items += 1,
            dsl::Stmt::Vote { .. } => votes += 1,
            dsl::Stmt::Attribute { .. } => attrs += 1,
            dsl::Stmt::Prose { .. } => prose += 1,
            dsl::Stmt::Email { .. } => {}
        }
    }

    assert!(prose > 0, "tutorial should preserve prose");
    assert!(hashtags >= 2, "tutorial declares multiple tags");
    assert!(attrs >= 1, "tutorial declares at least one aspect");
    assert!(items >= 6, "tutorial declares multiple items");
    assert!(votes >= 6, "tutorial casts multiple votes");
}

#[test]
fn parses_big_book_fixture_with_attached_bodies() {
    // This doc heavily uses the "/name{...}" style with no whitespace.
    let doc = dsl::parse_lines(BIG_BOOK).expect("parse_lines should succeed");

    let mut hashtags = 0usize;
    let mut items = 0usize;
    let mut votes = 0usize;

    for s in &doc.statements {
        match s {
            dsl::Stmt::Hashtag { name } => {
                hashtags += 1;
                assert!(
                    name.to_lowercase().contains("big-book"),
                    "expected #Big-Book tag, got #{name}"
                );
            }
            dsl::Stmt::Item { title, body } => {
                items += 1;
                assert!(!title.contains("__BLOCK_"), "title should not include block tokens");
                assert!(
                    body.as_ref().map(|b| !b.is_empty()).unwrap_or(false),
                    "items in big book should have bodies"
                );
            }
            dsl::Stmt::Vote { .. } => votes += 1,
            _ => {}
        }
    }

    assert!(hashtags >= 1);
    assert!(items >= 3, "fixture should contain multiple items");
    assert_eq!(votes, 0, "fixture has no votes; it's a big text import");
}


