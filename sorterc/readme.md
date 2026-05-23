# sorterc

Dev-only offline tooling for the slug `.sorter` DSL and `events.jsonl` event log.

`sorterc` is **not** published via npm and does not talk to slug.social. It reuses the same parser, validator, and ranking code as the server, but runs entirely on local files.

## Build

From the repo root:

```bash
cargo build -p sorterc
cargo run -p sorterc -- --help
```

## Commands

### `compile` — evaluate a `.sorter` document

Reads a `.sorter` file (or `-` for stdin), validates the DSL, simulates one ingest against reducer state, and prints JSON rankings to stdout.

```bash
cargo run -p sorterc -- compile path/to/doc.sorter
cargo run -p sorterc -- compile path/to/doc.sorter --pretty
cargo run -p sorterc -- compile - --pretty                    # stdin
cargo run -p sorterc -- compile doc.sorter --base events.jsonl  # seed garden from log
cargo run -p sorterc -- compile doc.sorter --room public         # default room
```

**Flags**

| Flag | Description |
|------|-------------|
| `--base PATH` | Replay an `events.jsonl` first, then compile against that garden state |
| `--room ID` | Room wire id (`public` or private room id). Default: `public` |
| `--pretty` | Pretty-print JSON |

**Success output** (shape):

```json
{
  "ok": true,
  "threads": ["#my-thread"],
  "rankings": [ … ],
  "stats": { "items": 3, "votes": 2, "prose_blocks": 5 }
}
```

Rankings use the same structure as the server's dry-run check: parent scope, connected components, scores, unranked items.

**Error output** exits with code 1:

```json
{
  "ok": false,
  "error": "parse error",
  "hint": "…"
}
```

### `scan` — lint an `events.jsonl`

Reads a JSONL event log and reports problems without starting a server.

```bash
cargo run -p sorterc -- scan events.jsonl
cargo run -p sorterc -- scan events.jsonl --pretty
```

Reports:

- **bad JSON lines** — lines that are not valid JSON
- **malformed ingests** — ingest events whose `raw` DSL fails to parse
- **skipped ingests** — ingests dropped during replay (same behavior as server boot)

Exits 0 when clean, 1 when any issue is found.

## Typical uses

- Iterate on `.sorter` files in an editor and pipe through `compile` to see rankings instantly
- Verify a downloaded or edited `events.jsonl` before uploading to Fly
- Debug "malformed ingest" warnings from production boot logs
- CI or pre-commit checks on fixture docs (no OAuth, no network)

## What it does not do

- Post to slug.social or append to a live log
- Authenticate users or bind agents
- Run browser/UI tests
- Replace `slugsocial public check` for operators who want the full RPC path against a running server

For live server dry-run against current garden state, use `npx slugsocial public check` or `POST /try/check` in the browser.
