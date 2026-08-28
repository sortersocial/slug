use super::{
    access::content_for_garden_view,
    browse::scoped_bc_containment,
    external::{external_frame_allowed, external_resolver_status_markup, external_source_href},
    item_page::{
        build_item_page_view_model, containment_crumb_chain, item_relations_markup,
        sibling_nav_markup,
    },
    pin::ont_pin_vote_controls,
    question::{collection_is_known, parse_collection_leaf, question_headline},
    render::aspect_ranking_sections_markup,
    vote::{
        canonical_edge_items, edge_vote_count_for_pair, edge_vote_entries_for_pair,
        ratios_for_compare_page, sort_votes_for_compare_display, suggest_next_vote_pair,
        vote_compare_item_card, vote_pool_href, QuestionHeadline,
    },
};
use crate::{
    events::{Event, Ingest},
    html::forum::ThreadNav,
    path_types::ItemId,
    reducer::{MembershipStatus, ReducerState, ScopeId},
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
    let page_left = ItemId::parse("~/topic/a").unwrap().ontology_leaf();
    let page_right = ItemId::parse("~/topic/b").unwrap().ontology_leaf();
    let raw = edge_vote_entries_for_pair(content, &page_left, &page_right);
    assert_eq!(raw.len(), 2);
    let sorted = sort_votes_for_compare_display(raw, &page_left, &page_right);
    // 8:2 is stored reduced as 4:1; still a stronger left share than 1:9.
    let (r0_l, r0_r) = ratios_for_compare_page(&sorted[0], &page_left, &page_right);
    assert_eq!(
        (r0_l, r0_r),
        (4, 1),
        "stronger left weight should sort first"
    );
    let (r1_l, r1_r) = ratios_for_compare_page(&sorted[1], &page_left, &page_right);
    assert_eq!((r1_l, r1_r), (1, 9));
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
    let a = ItemId::parse("~/topic/a").unwrap().ontology_leaf();
    let b = ItemId::parse("~/topic/b").unwrap().ontology_leaf();
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
    let a = ItemId::parse("~/topic/a").unwrap().ontology_leaf();
    let b = ItemId::parse("~/topic/b").unwrap().ontology_leaf();
    let next = suggest_next_vote_pair(content, &a, &b, None).expect("next sibling pair");
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
    let stats = crate::resolvers::ResolveStats {
        imported: 2,
        deleted: 0,
        kept: 0,
    };
    let html = external_resolver_status_markup(Ok((stats, "GitHub")), "/-/https://github.com/o/r")
        .into_string();
    assert!(html.contains("Imported 2 GitHub items."));
    assert!(html.contains("href=\"/-/https://github.com/o/r\""));
}

#[test]
fn external_resolver_status_markup_reports_deletes() {
    let stats = crate::resolvers::ResolveStats {
        imported: 1,
        deleted: 2,
        kept: 3,
    };
    let html =
        external_resolver_status_markup(Ok((stats, "GitHub")), "/-/https://github.com/o/r/issues")
            .into_string();
    assert!(html.contains("Imported 1 GitHub item."));
    assert!(html.contains("Removed 2 closed/stale items."));
    assert!(html.contains("Kept 3 current."));
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
    // Winner of the largest ranking group (size 2): rank 1/2 + top 50th percentile.
    assert_eq!(nav.largest_group_rank, Some((1, 2)));
    assert_eq!(nav.winner_percentile, Some(50));
}

#[test]
fn sibling_nav_rank_score_only_for_largest_group_member() {
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
         ~/topic/e {epsilon}\n\
         {a beats b}\n             ~/topic/a 2:1 ~/topic/b\n\
         {b beats c}\n             ~/topic/b 2:1 ~/topic/c\n\
         {d beats e}\n             ~/topic/d 2:1 ~/topic/e\n",
    );

    // Largest component is a/b/c (size 3); d/e is size 2.
    let winner = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a", 1);
    let winner_nav = winner.sibling_nav.expect("sibling nav");
    assert_eq!(winner_nav.largest_group_rank, Some((1, 3)));
    assert_eq!(winner_nav.winner_percentile, Some(66)); // (3-1)*100/3

    let mid = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/b", 1);
    let mid_nav = mid.sibling_nav.expect("sibling nav");
    assert_eq!(mid_nav.largest_group_rank, Some((2, 3)));
    assert_eq!(mid_nav.winner_percentile, None);

    // Member of the smaller ranking group: no rank/percentile score.
    let small = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/d", 1);
    let small_nav = small.sibling_nav.expect("sibling nav");
    assert_eq!(small_nav.largest_group_rank, None);
    assert_eq!(small_nav.winner_percentile, None);
}

#[test]
fn sibling_nav_markup_includes_rank_and_winner_percentile() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {topic body}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         ~/topic/c {gamma}\n\
         {a beats b}\n             ~/topic/a 2:1 ~/topic/b\n\
         {b beats c}\n             ~/topic/b 2:1 ~/topic/c\n",
    );

    let winner = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a", 1);
    let winner_nav = winner.sibling_nav.expect("sibling nav");
    let nav = ThreadNav::public();
    let html = sibling_nav_markup(&nav, &winner_nav, &winner.item).into_string();
    assert!(html.contains("rank: 1/3"), "missing rank in {html}");
    assert!(
        html.contains("top 66th percentile"),
        "missing winner percentile in {html}"
    );

    let mid = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/b", 1);
    let mid_nav = mid.sibling_nav.expect("sibling nav");
    let mid_html = sibling_nav_markup(&nav, &mid_nav, &mid.item).into_string();
    assert!(
        mid_html.contains("rank: 2/3"),
        "missing mid rank in {mid_html}"
    );
    assert!(
        !mid_html.contains("percentile"),
        "non-winner should omit percentile: {mid_html}"
    );
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
fn item_page_renders_aspect_section_below_canonical() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n\
         {canonical}\n\
         ~/topic/a 3:1 ~/topic/b\n\
         :beauty { winner is more beautiful }\n\
         {pretty}\n\
         ~/topic/b 2:1 ~/topic/a\n",
    );

    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic", 1);
    assert_eq!(model.child_rankings.component_rankings.len(), 1);
    assert_eq!(model.aspect_rankings.len(), 1);
    assert_eq!(model.aspect_rankings[0].slug, "beauty");
    assert_eq!(
        model.aspect_rankings[0].prompt.as_deref(),
        Some("winner is more beautiful")
    );
    assert_eq!(
        model.aspect_rankings[0].rankings.component_rankings.len(),
        1
    );
    let canon_top = model.child_rankings.component_rankings[0].ranked[0]
        .item
        .as_str();
    let aspect_top = model.aspect_rankings[0].rankings.component_rankings[0].ranked[0]
        .item
        .as_str();
    assert!(canon_top.ends_with("/a"), "canonical winner {canon_top}");
    assert!(aspect_top.ends_with("/b"), "aspect winner {aspect_top}");

    let content = content_for_garden_view(&reduced, &ScopeId::Public);
    let html = aspect_ranking_sections_markup(
        &model.aspect_rankings,
        &ThreadNav::public(),
        None,
        content,
        "/~/topic",
    )
    .into_string();
    assert!(
        html.contains("ont-tab-panel-aspect"),
        "missing aspect section: {html}"
    );
    assert!(html.contains(":beauty"), "missing aspect heading: {html}");
    assert!(
        html.contains("winner is more beautiful"),
        "missing aspect prompt: {html}"
    );
    assert!(
        html.contains("ont-ranking-list"),
        "missing ranking list: {html}"
    );
    assert!(
        html.contains("id=\"aspect-beauty\""),
        "missing aspect section anchor: {html}"
    );
}

#[test]
fn parse_collection_leaf_maps_bare_name_to_tilde_item() {
    let id = parse_collection_leaf("psalms").expect("psalms");
    assert_eq!(id, ItemId::parse("~psalms").unwrap().ontology_leaf());
    assert_eq!(
        parse_collection_leaf("~psalms"),
        parse_collection_leaf("psalms")
    );
    assert!(parse_collection_leaf("").is_none());
    assert!(parse_collection_leaf("~").is_none());
    assert!(parse_collection_leaf("~/psalms").is_none());
    assert!(parse_collection_leaf("foo/bar").is_none());
}

#[test]
fn question_headline_uses_scope_body_then_fallback_and_aspect_prompt() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "~/psalms {Which psalm is greater?}\n\
         ~/psalms/a {alpha}\n\
         ~/lonely\n\
         :beauty {more beautiful}\n",
    );
    let content = content_for_garden_view(&reduced, &ScopeId::Public);
    let psalms = ItemId::parse("~psalms").unwrap().ontology_leaf();
    let lonely = ItemId::parse("~lonely").unwrap().ontology_leaf();
    match question_headline(content, &psalms, None) {
        QuestionHeadline::Body(b) => assert_eq!(b, "Which psalm is greater?"),
        QuestionHeadline::Fallback(s) => panic!("expected body headline, got {s}"),
    }
    match question_headline(content, &lonely, None) {
        QuestionHeadline::Fallback(s) => assert_eq!(s, "Which is greater: lonely?"),
        QuestionHeadline::Body(b) => panic!("expected fallback, got body {b}"),
    }
    match question_headline(content, &psalms, Some("beauty")) {
        QuestionHeadline::Body(b) => assert_eq!(b, "more beautiful"),
        QuestionHeadline::Fallback(s) => panic!("expected aspect prompt, got {s}"),
    }
    match question_headline(content, &psalms, Some("speed")) {
        QuestionHeadline::Fallback(s) => assert_eq!(s, ":speed — which wins?"),
        QuestionHeadline::Body(b) => panic!("expected aspect fallback, got {b}"),
    }
    assert!(collection_is_known(content, &psalms));
    assert!(!collection_is_known(
        content,
        &ItemId::parse("~no-such-q").unwrap().ontology_leaf()
    ));
}

#[test]
fn item_page_omits_aspect_section_without_aspect_votes() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "~/topic {root}\n~/topic/a {alpha}\n~/topic/b {beta}\n{canonical}\n~/topic/a 3:1 ~/topic/b\n",
    );
    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic", 1);
    assert!(model.aspect_rankings.is_empty());
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
        vec!["https://slug.social/~/a", "https://slug.social/~/b"]
    );
    use crate::path_types::ItemId;
    assert!(
        model
            .child_rankings
            .unranked_items
            .contains(&ItemId::parse("https://slug.social/~/kid1").unwrap())
            || model
                .child_rankings
                .unranked_items
                .contains(&ItemId::parse("https://slug.social/~/kid2").unwrap())
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
    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "https://slug.social/~/", 1);
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
    assert!(items.contains("https://slug.social/~/a"));
    assert!(items.contains("https://slug.social/~/leaf"));
    assert!(items.contains("https://slug.social/~/b"));
}

#[test]
fn item_page_model_depth_all_includes_deep_descendants() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/a/mid {mid}\n\
         ~/topic/a/mid/leaf {leaf}\n\
         ~/topic/b {beta}\n",
    );

    let model = build_item_page_view_model(
        &reduced,
        &ScopeId::Public,
        "~/topic",
        super::item::GARDEN_DEPTH_ALL,
    );
    assert_eq!(model.child_depth, super::item::GARDEN_DEPTH_ALL);
    let items: std::collections::HashSet<&str> = model
        .child_rankings
        .unranked_items
        .iter()
        .map(|u| u.as_str())
        .collect();
    assert!(items.contains("https://slug.social/~/leaf"));
    assert!(items.contains("https://slug.social/~/b"));
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
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let payload = STANDARD.encode(json.to_string().as_bytes());
    let body = format!("```slug-github-card\n{payload}\n```");
    let html =
        vote_compare_item_card(&nav, &item, Some(&body), "vote-compare-left", None).into_string();
    assert!(
        html.contains("import-card"),
        "expected rich GitHub card markup, got: {html}"
    );
    assert!(html.contains("item-body-rich"));
    assert!(html.contains("vote-compare-left"));
    assert!(html.contains("#1 Compare card"));
}

#[test]
fn item_page_sibling_nav_implies_vote_on_item_pool_href() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
         ~/topic {root}\n\
         ~/topic/a {alpha}\n\
         ~/topic/b {beta}\n",
    );
    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic/a", 1);
    assert!(
        model.sibling_nav.is_some(),
        "expected sibling nav for ~/topic/a"
    );
    let parent = ItemId::parse(&model.item)
        .and_then(|i| i.parent())
        .expect("item with siblings has a parent");
    let nav = ThreadNav::public();
    let href = vote_pool_href(&nav, parent.as_str());
    assert!(
        href.contains("/vote?pool="),
        "expected sibling pool vote href, got {href}"
    );
    assert!(
        href.contains(&urlencoding::encode(&parent.display_path()).into_owned())
            || href.contains("topic")
            || href.contains("%7E"),
        "pool should target parent path, got {href}"
    );
    let markup =
        ont_pin_vote_controls(&nav, &model.item, None, "/~/topic/a", Some(&href)).into_string();
    assert!(
        markup.contains("data-testid=\"vote-on-this-item\""),
        "expected vote-on-this-item CTA, got {markup}"
    );
    assert!(
        markup.contains("vote on this item"),
        "expected button label, got {markup}"
    );
    let solo = ont_pin_vote_controls(&nav, &model.item, None, "/~/topic/a", None).into_string();
    assert!(
        !solo.contains("vote-on-this-item"),
        "CTA must be omitted when no sibling pool href is passed"
    );
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

#[test]
fn item_page_scope_labels_prompt_and_lists_memberships() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "~jedi { who is a jedi }\n~luke { farm boy }\n{ in }\n~luke <: ~jedi\n",
    );
    let jedi = build_item_page_view_model(&reduced, &ScopeId::Public, "~/jedi", 1);
    assert!(jedi.is_scope, "jedi has an active member");
    assert_eq!(jedi.body.as_deref(), Some("who is a jedi"));
    assert!(jedi.memberships.is_empty());

    let luke = build_item_page_view_model(&reduced, &ScopeId::Public, "~/x/luke", 1);
    assert!(!luke.is_scope);
    assert_eq!(luke.body.as_deref(), Some("farm boy"));
    assert_eq!(luke.memberships.len(), 1);
    assert_eq!(luke.memberships[0].status, MembershipStatus::Active);
    assert!(luke.memberships[0].parent.as_str().ends_with("/jedi"));
    assert_eq!(luke.memberships[0].containment_weight, 1);
    let crumbs: Vec<String> = luke
        .crumb_chain
        .iter()
        .map(|id| id.last_segment().to_string())
        .collect();
    assert_eq!(crumbs, vec!["jedi", "luke"]);
}

#[test]
fn item_page_crumb_picks_strongest_parent_lists_alternates() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "~jedi { j }\n~sith { s }\n~luke { l }\n{ j1 }\n~luke <: ~jedi\n{ s1 }\n~luke <: ~sith\n{ j2 }\n~luke <: ~jedi\n",
    );
    let content = content_for_garden_view(&reduced, &ScopeId::Public);
    let luke = ItemId::parse("~luke").unwrap().ontology_leaf();
    let chain = containment_crumb_chain(content, &luke);
    assert_eq!(chain.last().map(|id| id.last_segment()), Some("luke"));
    assert_eq!(
        chain[0].last_segment(),
        "jedi",
        "stronger jedi parent first"
    );

    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~luke", 1);
    assert_eq!(model.alternate_scopes.len(), 1);
    assert_eq!(model.alternate_scopes[0].last_segment(), "sith");
}

#[test]
fn item_page_renders_memberships_suspended_borders_and_journal() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "~jedi { j }\n~luke { l }\n{ in }\n~luke <: ~jedi\n",
    );
    apply_ingest(&mut reduced, 2, "{ out }\n~luke !<: ~jedi\n");
    let suspended = build_item_page_view_model(&reduced, &ScopeId::Public, "~luke", 1);
    assert!(suspended.memberships.is_empty());
    assert_eq!(suspended.suspended_borders.len(), 1);
    assert_eq!(
        suspended.suspended_borders[0].status,
        MembershipStatus::Suspended
    );
    let html_sus = item_relations_markup(&suspended, &ThreadNav::public(), 10_000).into_string();
    assert!(
        html_sus.contains("ont-tab-panel-borders"),
        "missing suspended section: {html_sus}"
    );
    assert!(html_sus.contains("suspended borders"));
    assert!(html_sus.contains("ont-border-suspended"));

    apply_ingest(&mut reduced, 3, "{ still in }\n~luke <: ~jedi\n");
    let breached = build_item_page_view_model(&reduced, &ScopeId::Public, "~luke", 1);
    assert_eq!(breached.memberships.len(), 1);
    assert_eq!(breached.fallen_journal.len(), 1);
    let html = item_relations_markup(&breached, &ThreadNav::public(), 10_000).into_string();
    assert!(
        html.contains("ont-tab-panel-memberships"),
        "missing memberships: {html}"
    );
    assert!(html.contains("memberships"));
    assert!(html.contains("containment 2"));
    assert!(
        html.contains("ont-fallen-borders"),
        "missing journal: {html}"
    );
    assert!(html.contains("fallen borders"));
}

#[test]
fn item_page_scope_body_is_labeled_prompt_in_relations_model() {
    let mut reduced = ReducerState::default();
    apply_ingest(
        &mut reduced,
        1,
        "~/topic {the prompt}\n~/topic/a {alpha}\n~/topic/b {beta}\n",
    );
    let model = build_item_page_view_model(&reduced, &ScopeId::Public, "~/topic", 1);
    assert!(model.is_scope);
    assert_eq!(model.body.as_deref(), Some("the prompt"));
}

#[test]
fn containment_breadcrumb_emits_leaf_hrefs() {
    let jedi = ItemId::parse("~jedi").unwrap().ontology_leaf();
    let luke = ItemId::parse("~luke").unwrap().ontology_leaf();
    let path = crate::html::breadcrumb_path::OntologyPath::from_item(luke.clone());
    let html = scoped_bc_containment(&path, &[jedi, luke], &ThreadNav::public()).into_string();
    assert!(
        html.contains("href=\"/~/jedi\""),
        "missing jedi crumb: {html}"
    );
    assert!(
        html.contains("href=\"/~/luke\""),
        "missing luke crumb: {html}"
    );
    assert!(!html.contains("/~/x/"));
}
