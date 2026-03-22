# Paths

Specter-style navigational path expressions for state mutations in Rust.
This document explains why they exist, what was tried, and what it means.

---

## What Happened

The reducer (`server/src/reducer.rs`) is the heart of slug. Every event from
the JSONL log passes through `apply_event`, which mutates `ReducerState` —
HashMaps, HashSets, VecDeques, nested structures. The imperative code was
correct but repetitive:

```rust
self.item_children.entry(parent.clone()).or_default().insert(child);
self.item_votes.entry(it.clone()).or_default().push_front(vote.clone());
self.ingests_by_thread.entry(thread).or_default().push_front(ing.id.clone());
self.ingests_ordered.push(ing.id.clone());
```

Every line is the same shape: navigate to a location, perform a terminal
operation. The Rust `entry()` API is powerful but verbose — the navigation
is implicit in the method chain, and you can't talk about "where" separately
from "what."

---

## Two Approaches

We prototyped both in isolated git worktrees, racing them against each other.

### Macro Approach (chosen)

Declarative macros that compile to the same `entry().or_default().insert()`
calls, but with Specter vocabulary:

```rust
nav!(self.item_children, keypath(parent), set_elem(child));
nav!(self.item_votes, keypath(it), push_front(vote));
nav!(self.ingests_ordered, push_back(id));
nav_each!(self.item_threads, keypath(item), set_elem, threads.iter());
```

Zero runtime cost. Paths evaporate at compile time. You cannot inspect,
serialize, or compose them at runtime. The benefit is purely readability
and vocabulary.

### Builder Approach (killed)

Runtime path objects — `Vec<Nav>` with a `Serialize`/`Deserialize` enum:

```rust
Path::new().field("item_children").key(parent).set_elem(child).apply(&mut state);
```

Paths as data: serializable, inspectable, comparable. An interpreter
dispatches segments against concrete struct fields via string matching.

This was killed because:
- String-based field lookup is Java thinking in Rust
- Runtime dispatch adds indirection without payoff when data never leaves the process
- The portable artifact is the mental model, not the serialized representation
- Rust's strength is in-memory; the network boundary is where you switch to Rama, not where you add an abstraction layer

---

## Why the Vocabulary Matters

The macro approach is "just a rename." `keypath(x), set_elem(y)` compiles
to `entry(x).or_default().insert(y)`. It doesn't do anything new.

But vocabulary shapes thinking. Every mutation in the reducer is now expressed
as: navigate here (`keypath`), do this (`set_elem`, `push_front`, `setval`).
That's the Specter model. That's the Rama model. When you read the reducer,
you're reading path expressions — not HashMap method chains.

The day slug outgrows a single node (if it ever does), the JSONL event log
is already a depot, the reducer is already a topology, and the `nav!` calls
translate directly to `local-transform>` with Specter paths. The migration
is syntactic because the mental model is already there. The programs don't
change shape. The runtime underneath changes completely.

---

## What This Costs

The macros add a dependency on knowing the vocabulary. A Rust developer
reading `nav!(self.item_children, keypath(parent), set_elem(child))` needs
to know that `keypath` means "entry by key" and `set_elem` means "insert
into set." The raw `entry().or_default().insert()` is self-documenting to
anyone who knows Rust's standard library.

The tradeoff: broader legibility for deeper expressiveness. The path
vocabulary is smaller than the entry API surface, and it generalizes
across collection types. Once you learn `keypath` and `set_elem`, you can
read every mutation in the reducer without knowing the underlying types.

---

## Rama

Nathan Marz spent 10 years building Rama. The paths in slug's `nav!` macro
are a toy version of what Rama provides. In Rama, the same path expression
that works on a local HashMap also works on a distributed, partitioned,
replicated state machine with transactional guarantees and exactly-once
semantics. That's not a library feature. That's a platform.

What slug has: the vocabulary. What Rama has: the vocabulary plus the
execution model plus the partitioners plus the fault tolerance plus the
replication. The gap between them is the gap between a declarative macro
and a decade of systems engineering.

But you have to start thinking in paths before you can build on a platform
that thinks in paths. The vocabulary is how you get there.

---

@9243b4e8-985d-4dd2-9587-815fc3eb2901:claudecode:anthropic/claude-sonnet-4-6
March 22, 2026
