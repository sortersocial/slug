use slugsocial_server::dsl;

const TUTORIAL: &str = include_str!("fixtures/tutorial.sorter");
const BIG_BOOK: &str = include_str!("fixtures/big-book.sorter");

#[test]
fn parses_tutorial_fixture_with_prose() {
    // This doc is intentionally "backwards compatible with prose".
    // We want to confirm we still extract key DSL statements.
    let doc = dsl::parse_full(TUTORIAL).unwrap();

    let mut items = 0usize;
    let mut votes = 0usize;
    let mut prose = 0usize;

    for s in &doc.statements {
        match s {
            dsl::Stmt::Item { .. } => items += 1,
            dsl::Stmt::Vote { .. } => votes += 1,
            dsl::Stmt::Prose { .. } => prose += 1,
            dsl::Stmt::Aspect { .. } => {}
        }
    }

    assert!(prose > 0, "tutorial should preserve prose");
    assert!(items >= 6, "tutorial declares multiple items");
    assert!(votes >= 6, "tutorial casts multiple votes");
}

#[test]
fn parses_big_book_fixture_with_attached_bodies() {
    // This doc heavily uses the "{...}\n~/name" style with no whitespace.
    let doc = dsl::parse_full(BIG_BOOK).expect("parse_full should succeed");

    let mut items = 0usize;
    let mut votes = 0usize;

    for s in &doc.statements {
        match s {
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

    assert!(items >= 3, "fixture should contain multiple items");
    assert_eq!(votes, 0, "fixture has no votes; it's a big text import");
}

#[test]
fn parses_external_dash_vote_line() {
    let doc = dsl::parse_full("{ reason }\n-/domain.com/a 2:1 -/domain.com/b").unwrap();
    assert_eq!(
        doc.statements,
        vec![dsl::Stmt::Vote {
            item1: "-/domain.com/a".to_string(),
            item2: "-/domain.com/b".to_string(),
            ratio_left: 2,
            ratio_right: 1,
            explanation: "reason".to_string(),
            aspect: None,
        }]
    );
}


