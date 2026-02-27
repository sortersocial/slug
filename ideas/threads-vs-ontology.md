# Threads vs Ontology

## Two layers

slug.social is two things at once: a forum and a garden.

**Threads** (`#thread`) are topic-based sessions. Bump-ordered, color-coded by
recency, a living feed of what's in circulation right now. The entry point.
Like 4chan or teamfortress.tv — you come in through a thread, you see what's
active, you participate in the conversation of the moment.

**The ontology** (`~/path/to/item`) is the garden. Collectively built, permanent,
path-addressed. Items accumulate votes over time. Rankings emerge and stabilize.
The garden grows whether or not anyone is watching.

These are different things. A thread is a session. An item is a thing that persists.
The thread passes. The item and its votes remain.

## Why the distinction matters

Without it, the system conflates two different kinds of value:

- **Temporal value**: what's worth talking about right now
- **Structural value**: what has been established through sustained comparison

A thread surfaces temporal value. The ontology holds structural value.
Mixing them means neither works properly — the feed gets cluttered with
permanent structure, and the permanent structure gets shaped by what
happened to be hot this week.

## How they interact

A thread is a context for comparison. When you compare two items in a thread,
the votes land on the ontology items — not on the thread. The thread provided
the occasion; the items absorb the signal.

Multiple threads can touch the same items from different angles. `#ai-models-2025`
and `#coding-assistants` might both produce votes on `~/models/anthropic/claude`
and `~/models/openai/gpt4`. Those votes accumulate on the items regardless of
which thread produced them.

The thread is the session. The ontology is the record.

## The feed

Threads need a bump-order sorted index as the primary entry point. When someone
ingests a document into a thread, the thread bumps. The feed shows what's active.

Color-coded by recency — you can see at a glance what's fresh vs what's gone cold.
Live streaming (SSE or SSE + idiomorph for partial DOM updates) would make this
feel alive: new votes arriving, rankings shifting, threads bumping in real time.

## The garden

The ontology is navigable independently of threads. You can browse `~/languages/`
and see everything that's been compared there, across all threads, across all time.
The path structure makes it a garden you can walk through.

Items can be nested arbitrarily deep:
  ~/whitepaper/mechanism/rank-centrality
  ~/models/anthropic/claude-sonnet
  ~/languages/compiled/rust

The hierarchy is meaningful — parent paths can aggregate rankings from children,
giving you both fine-grained and coarse-grained views of the same space.

## Current state

The implementation currently conflates these layers. `#thread` acts as a namespace
for `~/` items, making threads and ontology structure the same thing. This needs
to be separated:

- Threads get their own bump-ordered feed with recency state
- Ontology items exist independently of any thread
- Votes reference both a thread (context) and items (targets)
- The ranking API operates on items, not threads

The DSL already points toward this — `~/path/item` is the right primitive.
Threads need to become first-class objects with their own state (bump time,
subscriber count, recency) rather than just namespaces.
