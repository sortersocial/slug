use super::{
    access::content_for_garden_view,
    external::{external_frame_allowed, external_resolver_status_markup, external_source_href},
    item_page::build_item_page_view_model,
    vote::{
        canonical_edge_items, edge_vote_count_for_pair, edge_vote_entries_for_pair,
        ratios_for_compare_page, sort_votes_for_compare_display, suggest_next_vote_pair,
        vote_compare_item_card,
    },
};
use crate::{
    events::{Event, Ingest},
    html::forum::ThreadNav,
    path_types::ItemId,
    reducer::{ReducerState, ScopeId},
};

fn apply_ingest(state: &mut ReducerState, ts: i64, raw: &str) {
    state.apply_event(Event::Ingest(Ingest {
        ts,
        id: format!("ing-{ts}"),
        raw: raw.to_string(),
        principal: "testuser".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        room_id: "public".to_string(),
        thread_tag: String::new(),
    }));
}

fn apply_ingest_room(state: &mut ReducerState, ts: i64, room_id: &str, raw: &str) {
    state.apply_event(Event::Ingest(Ingest {
        ts,
        id: format!("ing-{ts}"),
        raw: raw.to_string(),
        principal: "testuser".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        room_id: room_id.to_string(),
        thread_tag: String::new(),
    }));
}

#[test]
fn vote_edge_compare_sorts_rows_by_strength_for_page_left() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         {weak for a}\n             ~/topic/a 1:9 ~/topic/b\n\
         {strong for a}\n             ~/topic/a 8:2 ~/topic/b\n",
    );
    let content = content_for_garden_view(&reduced, &ScopeId::Public);
    let page_left = ItemId::parse("~/topic/a").unwrap().normalized_storage();
    let page_right = ItemId::parse("~/topic/b").unwrap().normalized_storage();
    let raw = edge_vote_entries_for_pair(content, &page_left, &page_right);
    assert_eq!(raw.len(), 2);
    let sorted = sort_votes_for_compare_display(raw, &page_left, &page_right);
    let (r0, _) = ratios_for_compare_page(&sorted[0], &page_left, &page_right);
    assert_eq!(r0, 8, "stronger left weight should sort first");
    let (r1, _) = ratios_for_compare_page(&sorted[1], &page_left, &page_right);
    assert_eq!(r1, 1);
}

#[test]
fn edge_vote_count_for_pair_matches_votes_for_edge_len() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         {first vote}\n\
         ~/topic/a 3:2 ~/topic/b\n\
         {second vote}\n\
         ~/topic/b 2:3 ~/topic/a\n",
    );
    let content = content_for_garden_view(&reduced, &ScopeId::Public);
    let a = ItemId::parse("~/topic/a").unwrap().normalized_storage();
    let b = ItemId::parse("~/topic/b").unwrap().normalized_storage();
    assert_eq!(
        edge_vote_count_for_pair(content, &a, &b),
        edge_vote_entries_for_pair(content, &a, &b).len()
    );
    assert_eq!(edge_vote_entries_for_pair(content, &a, &b).len(), 2);
}

#[test]
fn suggest_next_vote_pair_prefers_unvoted_sibling_pair() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         ~/topic/c {gamma}\n\
         {a beats b}\n             ~/topic/a 2:1 ~/topic/b\n",
    );
    let content = content_for_garden_view(&reduced, &ScopeId::Public);
    let a = ItemId::parse("~/topic/a").unwrap().normalized_storage();
    let b = ItemId::parse("~/topic/b").unwrap().normalized_storage();
    let next = suggest_next_vote_pair(content, &a, &b).expect("next sibling pair");
    assert_ne!(
        canonical_edge_items(&next.0, &next.1),
        canonical_edge_items(&a, &b)
    );
    assert!(
        next.0.as_str().ends_with("/c") || next.1.as_str().ends_with("/c"),
        "next pair should include the unvoted sibling: {next:?}"
    );
}

#[test]
fn external_resolver_status_markup_reports_success_and_refresh() {
    let html = external_resolver_status_markup(Ok(2), "/-/github.com/o/r").into_string();
    assert!(html.contains("Imported 2 GitHub items."));
    assert!(html.contains("href=\"/-/github.com/o/r\""));
}

#[test]
fn item_page_model_includes_body_and_unranked_without_votes() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {topic body}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n",
    );

    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a", 1);
    assert_eq!(model.body.as_deref(), Some("alpha"));
    assert!(model.sibling_nav.is_some());
    assert!(model.child_rankings.component_rankings.is_empty());
}

#[test]
fn item_page_model_computes_sibling_nav_in_component() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {topic body}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         ~/topic/c {gamma}\n\
         {a beats b}\n             ~/topic/a 2:1 ~/topic/b\n",
    );

    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a", 1);
    let nav = model.sibling_nav.expect("expected sibling nav");
    assert_eq!(nav.groups.len(), 2);
    assert_eq!(nav.groups[0].links.len(), 2);
    assert_eq!(nav.groups[1].links.len(), 1);
}

#[test]
fn sibling_nav_splits_each_unranked_into_its_own_group() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {topic body}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         ~/topic/c {gamma}\n\
         ~/topic/d {delta}\n\
         {a beats b}\n             ~/topic/a 2:1 ~/topic/b\n",
    );

    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a", 1);
    let nav = model.sibling_nav.expect("expected sibling nav");
    assert_eq!(nav.groups.len(), 3);
    assert_eq!(nav.groups[0].links.len(), 2);
    assert_eq!(nav.groups[1].links.len(), 1);
    assert_eq!(nav.groups[2].links.len(), 1);
}

#[test]
fn item_page_model_builds_ranked_child_components() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         {a beats b}\n             ~/topic/a 3:1 ~/topic/b\n\
         ~/topic/kid1 {k1}\n\
         ~/topic/kid2 {k2}\n\
         ~/topic/kid1/leaf {leaf}\n",
    );

    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic", 1);
    assert_eq!(model.child_rankings.component_rankings.len(), 1);
    assert_eq!(model.child_rankings.component_rankings[0].pairs, 1);
    let names: Vec<&str> = model.child_rankings.component_rankings[0]
        .ranked
        .iter()
        .map(|r| r.item.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "https://slug.social/~/topic/a",
            "https://slug.social/~/topic/b"
        ]
    );
    use crate::path_types::ItemId;
    assert!(
        model
            .child_rankings
            .unranked_items
            .contains(&ItemId::parse("https://slug.social/~/topic/kid1").unwrap())
            || model
                .child_rankings
                .unranked_items
                .contains(&ItemId::parse("https://slug.social/~/topic/kid2").unwrap())
    );
}

#[test]
fn item_page_room_scope_root_lists_top_level_children() {
    let mut reduced = ReducerState::default();
    apply_ingest_room(
        &mut reduced,
        1,
        "9ab12cd/my-room",
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/t1 {a}\n~/t2 {b}\n",
    );
    use crate::path_types::ItemId;
    let root = ItemId::ontology_root();
    let model = build_item_page_view_model(
        &reduced,
        &ScopeId::Room("9ab12cd/my-room".to_string()),
        root.as_str(),
        1,
    );
    assert!(!model.item_has_parent);
    assert_eq!(model.child_rankings.unranked_items.len(), 2);
    let set: std::collections::HashSet<&str> = model
        .child_rankings
        .unranked_items
        .iter()
        .map(|u| u.as_str())
        .collect();
    assert!(set.contains("https://slug.social/~/t1"));
    assert!(set.contains("https://slug.social/~/t2"));
}

/// Top-level `~/a` vs `~/b` votes form one ranked component under the ontology root.
#[test]
fn item_page_room_scope_root_shows_ranked_child_group() {
    let mut reduced = ReducerState::default();
    apply_ingest_room(
        &mut reduced,
        1,
        "9ab12cd/my-room",
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/a {a}\n~/b {b}\n{because}\n~/a 2:1 ~/b\n",
    );
    use crate::path_types::ItemId;
    let root = ItemId::ontology_root();
    let model = build_item_page_view_model(
        &reduced,
        &ScopeId::Room("9ab12cd/my-room".to_string()),
        root.as_str(),
        1,
    );
    assert_eq!(model.child_rankings.component_rankings.len(), 1);
    assert_eq!(model.child_rankings.component_rankings[0].pairs, 1);
    let names: Vec<&str> = model.child_rankings.component_rankings[0]
        .ranked
        .iter()
        .map(|r| r.item.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["https://slug.social/~/a", "https://slug.social/~/b"]
    );
    assert!(model.child_rankings.unranked_items.is_empty());
}

/// Legacy `https://slug.social/~/` spelling still resolves children under the real root key.
#[test]
fn item_page_model_normalizes_legacy_tilde_root_storage_url() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n~/x {x}\n",
    );
    let model =
        build_item_page_view_model(&reduced, &ScopeId::Public, "https://slug.social/~/", 1);
    assert_eq!(model.child_rankings.unranked_items.len(), 1);
    assert_eq!(
        model.child_rankings.unranked_items[0].as_str(),
        "https://slug.social/~/x"
    );
}

#[test]
fn item_page_model_depth_includes_descendants() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/a/leaf {leaf}\n\
         ~/topic/b {beta}\n",
    );

    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic", 2);
    let items: std::collections::HashSet<&str> = model
        .child_rankings
        .unranked_items
        .iter()
        .map(|u| u.as_str())
        .collect();
    assert!(items.contains("https://slug.social/~/topic/a"));
    assert!(items.contains("https://slug.social/~/topic/a/leaf"));
    assert!(items.contains("https://slug.social/~/topic/b"));
}

#[test]
fn vote_compare_item_card_renders_github_import_markup() {
    let nav = ThreadNav::public();
    let item = ItemId::parse("https://github.com/o/r/issues/1").unwrap();
    let json = serde_json::json!({
        "v": 1,
        "schema": "slug_github_import",
        "kind": "issue",
        "url": "https://github.com/o/r/issues/1",
        "headline": "#1 Compare card",
        "sublines": ["State: open"],
    });
    let body = format!("```slug-github-card\n{}\n```", json.to_string());
    let html = vote_compare_item_card(
        &nav,
        &item,
        Some(&body),
        "vote-compare-left",
        None,
    )
    .into_string();
    assert!(
        html.contains("github-import-card"),
        "expected rich GitHub card markup, got: {html}"
    );
    assert!(html.contains("item-body-rich"));
    assert!(html.contains("vote-compare-left"));
    assert!(html.contains("#1 Compare card"));
}

#[test]
fn external_source_href_maps_youtube_path_identity_back_to_watch_url() {
    assert_eq!(
        external_source_href("https://www.youtube.com/watch/v/dQw4w9WgXcQ"),
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    );
    assert_eq!(
        external_source_href("https://github.com/sortersocial/slug"),
        "https://github.com/sortersocial/slug"
    );
}

#[test]
fn external_frame_allowed_skips_known_blocked_hosts() {
    assert!(!external_frame_allowed(
        "https://github.com/sortersocial/slug"
    ));
    assert!(external_frame_allowed("https://example.com/path"));
}
