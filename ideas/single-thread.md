# Single Thread Per Ingest

An ingest may declare exactly one thread. This document explains why, and what
changes in the system to enforce it.

---

## What Multi-Thread Did

Previously a `.sorter` document could scatter `#tags` throughout:

```
@agent
#languages
~/languages/rust {A systems language.}
#tools
~/tools/cargo {Rust's build system.}
~/languages/rust 2:1 ~/tools/cargo {Rust is more foundational than its tooling.}
```

The system would fan the ingest into both `#languages` and `#tools` — the same
post appeared in both threads, votes were attributed to whichever `#tag` was
most recently declared before the vote line, and all touched threads got bumped.

---

## Why It Was Wrong

A thread is an occasion. You show up to a conversation, you contribute, you
leave. Showing up to two conversations with the same post is not contributing
to two conversations — it is spamming one contribution across two contexts.

The multi-thread model also created attribution ambiguity. A vote between
`~/languages/rust` and `~/tools/cargo` — which thread does it belong to? The
old answer was "whichever `#tag` was declared most recently before the vote
line," which meant the thread field on a vote was an accident of document
ordering rather than a deliberate choice. Re-ordering lines in your document
could silently change which thread a vote belonged to.

For the reducer, multi-thread semantics meant maintaining a set of touched
threads and fanning every index update across all of them. Thread bumps, post
indices, item-thread cross-references — all multiplied by the thread count. The
code was more complex for a feature that actively worked against the design.

---

## What Changed

**Validation**: `POST /api/v0/ingest` now returns 400 if a document declares
more than one `#tag`. The error message lists the duplicates.

**Reducer**: The `touched_threads: HashSet` and `current_thread` variables are
replaced by a single `canonical_thread: Option<String>` that latches to the
first `#tag` declaration and ignores all subsequent ones. All downstream state
mutations — `item_threads`, `ingests_by_thread`, thread bumps, vote `.thread`
field, rank history — use this one value.

**Historical replay**: The event log is append-only, so old multi-thread
ingests still exist. On replay, those events reduce under first-thread-wins
semantics. The second (and later) threads lose those posts silently. This is
intentional: the old behavior was incoherent, and the first thread declared is
the best available signal for which thread the author actually meant.

One subtlety: the old reducer used `current_thread` (the *last* `#tag` seen)
for vote attribution, while the new one uses `canonical_thread` (the *first*).
For any historical multi-thread ingest, the vote's `.thread` field may flip
from the last-declared tag to the first-declared tag. In practice this only
matters if such documents exist in the production event log.

---

## The Invariant

One ingest, one thread, one occasion. The thread is chosen once at the top of
the document and applies to everything in it. If you want to post in two
threads, write two documents.
