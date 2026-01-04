# Slug: Collective Ranking Infrastructure

## What We're Building

A consensus engine that produces global rankings from pairwise comparisons. The key insight: you don't need n² comparisons to rank n items—transitivity fills in the gaps. Rank centrality (stationary distribution of a Markov chain) does the math.

Three interfaces:
1. **Web** (`slug.social`) — rendered HTML, the public index of collective judgment
2. **CLI** (`npx slugsocial` / `uvx slugsocial`) — instant, zero-install, agent-native
3. **Email** (`vote@mail.slug.social`) — slow, contemplative, human-paced (future)

## Architecture Decisions

### Storage: JSONL Event Log (not Postgres)

**Decision**: Use append-only JSONL files instead of a relational database.

**Why**:
- Zero dependencies — no Postgres/SQLite to manage
- Debuggable — `cat events.jsonl | jq 'select(.type=="vote")'`
- Portable — copy the file, you have the whole system
- Time-travel — replay to any point in history
- Agent-native — LLMs read/write JSONL trivially
- Git-friendly — can version control consensus history

**Scaling limits** (acceptable):
- 100K events: ~20MB, <200ms cold load
- 1M events: ~200MB, ~2 seconds cold load
- Beyond: add checkpoints/snapshots

The event-log pattern proves this works: append events, reconstruct state on load.

### Server: Rust (Axum)

**Decision**: Implement the backend in Rust.

**Why**:
- Already have working rank centrality with sparse matrices
- Fast enough for realtime ranking updates
- Can compile to native binaries for server deployment
- Sybil resistance requires a centralized authority anyway

**What to simplify**:
- Replace SQLx database queries with JSONL file operations
- Keep the ranking algorithm as-is (it's correct and tested)

### CLI: Thin Clients (Node + Python)

**Decision**: `npx slugsocial` and `uvx slugsocial` are thin HTTP clients.

**Why**:
- Zero-install via npx/uvx is the killer feature
- All logic lives on the server — CLIs just format output
- HATEOAS responses make them self-documenting
- ~200 lines each, trivial to maintain

**Package names** (both available on npm AND PyPI):
- `slugsocial` ✓ — matches domain, on-brand
- `pairvote` ✓ — backup option
- `rankgraph` ✓ — backup option

Note: `slug` and `slg` are taken on both registries.

### The DSL Syntax

```
#tags       — namespaces/categories
/items      — things being ranked
:aspects    — dimensions of comparison (truth, importance, etc.)
```

Example:
```
#programming-languages

/rust 3:1 /go :for-systems-programming
/go 2:1 /rust :for-quick-prototypes
```

**Why these sigils**:
- `#` — familiar from hashtags, won't conflict with shell (quoted)
- `/` — familiar from URLs/paths, implies "a thing"
- `:` — familiar from key:value, implies "a property"

## Open Problems

### 1. Sybil Resistance

If anyone can vote, what prevents vote flooding?

**Options considered**:
- Rate limiting by IP — weak, easily bypassed
- Email verification — friction, but effective
- OAuth (GitHub/Google) — centralizes trust, but users have it
- Proof of work — interesting for agents, annoying for humans
- Reputation-weighted votes — bootstrapping problem

**Current thinking**: Start with OAuth + rate limits. Can add proof-of-work for anonymous agent votes later.

### 2. Incentives

Why would anyone vote?

**Intrinsic**: "I want to know what's best" — works for engaged communities
**Reputation**: Votes are public, builds credibility — requires identity
**Economic**: Token rewards — probably overkill, adds complexity

**Current thinking**: Start with intrinsic + reputation. The product should be useful enough that voting is its own reward.

### 3. Realtime Updates

How do rankings update live?

**Options**:
- Polling — simple, wasteful
- SSE — one-way, sufficient for rankings
- WebSocket — bidirectional, overkill

**Decision**: SSE for live ranking updates. Simple, well-supported.

## Implementation Order

1. **Rust server with JSONL storage**
   - Event log: items, tags, votes
   - Rank centrality on demand
   - REST API for CLI
   - HTML rendering for web

2. **Web frontend** (`slug.social`)
   - Browse rankings by tag
   - Vote interface
   - History/provenance

3. **CLI thin clients**
   - `npx slugsocial rank #tag`
   - `npx slugsocial vote /a 2:1 /b`
   - HATEOAS responses

4. **Identity/auth layer**
   - OAuth for humans
   - API keys for agents
   - Rate limiting

## What We Have

- Rust server with JSONL event log + ranking
- Rust CLI shipped via `npx`/`uvx`

## Next Steps

Focus: **Rust server with JSONL storage**

1. Define event types (Item, Tag, Vote)
2. Implement append/load for JSONL
3. Wire up to existing rank centrality
4. Add REST endpoints for CLI
5. Add HTML rendering for web


