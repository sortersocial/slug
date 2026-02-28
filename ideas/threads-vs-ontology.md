# Thread / Ontology Split

slug.social is two things. They share a namespace but serve completely different
purposes, operate on different timescales, and need different UX modes.

---

## The Two Layers

**Threads** (`#tag`, `/t/:tag`) are forum sessions. Bump-ordered. Color-coded
by recency. The living feed of what's in circulation right now. You come in
through a thread because something is happening there. Like 4chan or
teamfortress.tv — the index is a board, each entry is a thread, threads die
when they go cold. The value is temporal.

**The ontology** (`~/path/item`, `/~/tag/...`) is the garden. Permanently
path-addressed. Items accumulate votes across time, across threads, across
actors. Rankings emerge and stabilize. The garden grows whether or not anyone
is watching. The value is structural.

A thread provides the *occasion* for comparison. The ontology absorbs the
*signal* from that comparison. The thread passes. The items and their votes
remain.

---

## Why They Must Be Separate

Conflating them breaks both:

- If threads own items, then namespace structure gets shaped by what happened
  to be discussed this week. Old threads pollute the ontology with stale
  context.
- If the ontology drives the feed, the bump-ordered index doesn't work —
  you'd need to browse a hierarchical garden to find what's active, which
  is the wrong entry point.

The feed should answer: *what's happening now?*
The garden should answer: *what has been established?*

These are different questions. They need different views.

---

## How They Interact

Same namespace, different layer:

- `#software-values` is a thread. It bumps when someone ingests into it.
  It appears in the feed. It dies when it goes cold.
- `~/software-values/correctness` is an ontology item. It was probably
  introduced in `#software-values` but it now exists independently. Future
  threads — `#engineering-tradeoffs`, `#type-systems-2026` — can produce
  votes on the same item.

The vote lands on the *item*, not the *thread*. The thread provided context.
The item absorbed signal.

Multiple threads can accumulate votes on the same item from different angles.
This is the point: the garden is built collectively over time, and the threads
are just the occasions that made the building happen.

---

## URL Structure

```
/            thread index   — bump-ordered, recency-colored, dark
/t/:tag      thread view    — recent ingests, conversation, dark
/~           ontology index — all namespaces, alphabetical, light
/~/tag       ontology ns    — aspects + items for this namespace, light
/~/tag/item  item detail    — votes, rankings, infinite nesting, light
```

The visual treatment is load-bearing: thread pages are dark (you're in the
live space, the active conversation), ontology pages are light (you're reading
a document, a settled record). Switching modes is jarring on purpose — you
cross a threshold.

The `~` sigil in URLs mirrors the DSL (`~/path/item`). The `#` sigil mirrors
the thread syntax (`#tag`). The URL is readable as prose if you know the DSL.

---

## The Ontology is Navigable Independently

You don't need threads to browse the garden. `/~` lists all namespaces.
`/~/languages` lists all items under `languages`. Items nest arbitrarily:

```
~/languages/compiled/rust
~/languages/compiled/go
~/models/anthropic/claude-sonnet-4-6
~/whitepaper/mechanism/rank-centrality
```

The hierarchy is meaningful. Parent paths could eventually aggregate rankings
from their children — `/~/languages` could show a coarse-grained ranking that
inherits signal from `/~/languages/compiled` and `/~/languages/scripted`.
This is not implemented but the path structure makes it possible.

---

## What Threads Are Not

Threads are not categories. They don't own items. They don't structure the
ontology. They're sessions — a shared context for comparison that existed
at a particular moment in time.

A thread with no recent activity is not dead in the way a forum thread dies
— its votes are still in the garden. It's just that the *occasion* has passed.
The record persists.

---

## Current Implementation

- `ThreadState` in `ReducerState`: tracks `last_activity_ts` and
  `subscriber_count` per tag
- Thread feed on `/` broadcasts live via SSE + Idiomorph — new ingests bump
  threads in real time without page reload
- `/t/:tag` shows raw ingests in reverse-chron — the conversation itself,
  not its products
- `/~/tag` shows aspects + items — no ingests, no conversation, just structure
- CSS treats the views differently at the body class level: `view-thread` stays
  dark, `view-ontology` flips to light (Win95-gray bevel palette)

---

## What Remains

**SSE for ontology pages.** Currently only the thread feed broadcasts live.
Ontology pages (aspect rankings, item pages) are static-on-load. Rankings that
shift while you're looking at them don't update. This should be easy to add —
the broadcast channel exists, just needs fragment rendering for ontology views.

**Actor identity persistence.** The web ingest form requires typing `@actor`
every time. Should be stored in localStorage and prepended automatically.
Without this, the web UI isn't practically usable for the ontology.

**Item depth view.** The idea: `/~/tag/item?depth=3` shows the current item
with its children to depth 3 inline, deeper items becoming footnotes. This
makes the garden actually navigable as a document rather than a directory tree.

**Thread subscriptions.** Actors should be able to subscribe to threads and
receive notifications when new votes arrive on items they've previously voted
on. The `subscriber_count` field exists in `ThreadState`. The notification
infrastructure exists. The subscribe endpoint does not.

**Parent aggregation.** Votes on `/~/languages/compiled/rust` should propagate
partial signal to `/~/languages/compiled` and `/~/languages`. The aggregation
model is an open question — probably weighted by vote recency and component
connectivity.
