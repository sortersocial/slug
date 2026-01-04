# Roadmap (ideas not implemented yet)

This file captures the “vision” items that were discussed in earlier docs (previously `root.tdsl`) but are **not implemented** in the current repo.

It’s intentionally concrete: each item is either a missing product surface area (CLI/site) or missing infrastructure (deploy/auth/realtime).

## Shipping today

- **Server**: Rust (Axum) + JSONL event log + reducer + ranking
- **API**: `/api/v0/vote`, `/api/v0/pair`, `/api/v0/rank`, `/api/v0/ingest`
- **CLI**: `slugsocial rank|pair|vote|ingest|healthz`
- **DSL**: Rust parser with block masking, prose handling, fixtures parsing

## Missing from the original “vision”

### Email interface (write via email)

- **Inbound email**: `vote@mail.slug.social` (or similar) → parse DSL → emit events
- **Deliverability**: SPF/DKIM/DMARC, bounce handling
- **Security**: keying/auth for email senders

### CLI surface area (commands)

Earlier drafts implied commands that don’t exist yet:

- **`slugsocial item /name`**: show item details (body, tags, vote history)
- **`slugsocial tag #name`**: show tag contents (items, aspects, recent votes/ingests)
- **`slugsocial search <query>`**: search tags/items
- **`slugsocial add /item`**: explicit item creation (today it’s implicit via ingest/votes)

### Better “agent + human” output (UX)

- **Pretty, pipeable output** (aligned columns, minimal noise)
- **More HATEOAS**: always print next actions as commands
- **Stats**: last vote time, vote totals, per-aspect coverage
- **Explanations**: first-class display of vote body/explanations everywhere

### Identity / reputation / anti-sybil

Currently: API key-based auth for write endpoints.

Ideas not implemented:

- **OAuth for humans**
- **Invite / vouch tree**
- **Strikes / moderation**
- **Reputation-weighted votes**
- **“Prescience” reputation** (alignment with eventual consensus)

### Realtime updates

- **SSE** for live ranking updates (discussed; not implemented)

### Web “index” completeness

- **Item detail pages** (body + provenance)
- **Search UI**
- **Better tag landing pages**
- **Public vote history browsing**

### Ops / deploy automation

- **Auto deploy server** on `main` updates (today: manual `fly deploy`)
- **Separate release tracks**:
  - CLI releases via tags (already done)
  - Server deploys via main branch (not yet automated)


