# The bare-tilde / root-scope edge (investigated 2026-10-05, reproduced on production)

## The edge

Defining an item with bare tilde — `~x { body }` — creates a **floating
item**: it exists, is votable, accumulates rank-history, gets a working item
page (`/~/x`, `/r/<room>/~/x`), and appears in the flat global ranking
(`garden rank`, `GetGardenRank` with no parent). But it belongs to **no
electorate** — it is invisible in every scope listing: the root garden page
(`/~`, `/r/<room>/~`), pair pools, children tables, and vote-compare pools.

The UI confesses it: a floating item's vote-history block renders
`#0 of 0` — globally ranked, scope-ranked nowhere.

Contrast: `~/x { body }` (path sugar) emits `x <: root` as a sugar edge
(weight exactly 1), making the item a root-scope member, visible in the root
listing immediately (unranked until voted).

## Why (mechanism)

- DSL: `~/a/b` desugars to sugar containment claims along the path
  (`b <: a <: root`); bare `~x` desugars to no containment at all.
- Reducer: `add_child_edge` (`reducer.rs:832`) returns immediately for tilde
  items (`if item.tilde_tail().is_some() { return; }`) — it only ghosts
  parents for web items (the `/-/` external index). Item statements and
  vote-item creation call it, so nothing ever plants a tilde item anywhere.
- Scope views render `members_of(scope)`; a floating item is a member of
  nothing.
- The flat global ranking group is containment-independent — hence the
  CLI/API showing what the pages hide.

## The trap is one-way (no DSL escape hatch)

`~x <: ~/` is a **parse error** ("incomplete containment claim") — the root
is not a valid containment target. A floating item cannot be claimed into
the root electorate explicitly. (A later post containing `~/x` sugar would
plant it — sugar claims accumulate — but authors don't know that, and
nothing tells them.)

## Reproduction (production, room `fx7gmyd/eros`, 2026-10-05)

1. Post `~tape { … }`, `~wave { … }`, `{ why } ~tape 2:1 ~wave`.
2. `garden rank` (CLI) shows both, 0.667/0.333. Item pages render fully.
3. Root garden page `/r/fx7gmyderos/~`: neither listed; "no voted pairs yet
   in this scope".
4. Post `~/planted { … }`: root page immediately lists `~/planted`
   (unranked). tape/wave still absent.
5. `/r/<room>/~/wave` page: body, vote history, `#0 of 0`.
6. Room pages 404 for non-members (auth-gating, correct).

## Fix options

**A. Semantic — implicit root sugar for bare tilde items at ingest** (weight
1, same as path sugar). Root becomes the default electorate, matching the
user mental model and the docs' own language ("GET /~ is the root-electorate
index"). Replay recomputes everything, so historical floating items (public
garden included — e.g. bare scope items like `~fast-food`) join the root
listing. Blast radius: root page and root pair pools grow.

**B. Presentational — scope views union a "floating items" section.**
Root (and any scope view?) list unscoped items in a separate section with a
placement CTA. No replay/semantics change; keeps "floating" as a first-class
draft state. Smaller blast radius, but two listing semantics to maintain.

**C. Parser — allow `~x <: ~/` as an explicit root claim.** Fixes the rescue
path; insufficient alone (nothing tells authors they're floating).

Regardless of A/B/C: the `#0 of 0` rendering on vote-history blocks should
say something kinder for unscoped items ("unscoped — claim a home with
`{ why } ~/x <: ~/parent`").

## Recommendation

**A + C**, with B's floating-section as the fallback if replay-time growth
of the public root listing is judged undesirable. The default should match
the mental model: the garden is the root electorate, and a thing in the
garden should be *in* the garden.
