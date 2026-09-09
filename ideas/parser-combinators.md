# Parser combinators vs the ad-hoc DSL parser

Investigation of `server/src/dsl.rs`: whether a combinator library (chumsky,
winnow, pest, …) would be an improvement, especially for error messages, and
whether the "sigil at the start of a line starts the grammar; everything else
is prose" rule is expressible in one.

**Recommendation: do not replace `parse_full` with a combinator library.** The
outer loop is a line classifier with committed-vs-fallback recovery, not a
CFG/PEG. The error messages we already write are pedagogical and would get
worse under generic "expected X, found Y". The real error upgrade is typed
errors plus line numbers, which the current loop can do without a crate.

---

## What the parser actually is

Two layers, plus a preprocessor.

```
text
  │
  ▼
BlockMasker          ← preprocessor, not a grammar
  fences, {{ }}, { }
  become __BLOCK_xxxxxxxx__ tokens
  │
  ▼
parse_full           ← outer: line classifier + state machine
  pending_block, current_aspect, prose_buffer
  │  candidate DSL lines only
  ▼
parse_line /         ← inner: statement parsers with bespoke errors
parse_item_definition_statement /
parse_block_prefixed_statement
  │
  ▼
desugar_item_ref     ← semantic, not syntactic
aspect inheritance
```

`tokenize_prose_item_refs` is a **third** mini-parser used only for
linkification of prose. It is not the document grammar.

The public contract is `parse_full` → `Document { statements: Vec<Stmt> }`
interleaving `Item`, `Vote`, `Aspect`, `Containment`, and `Prose`. Validation
(`server/src/api/validate.rs`) is a later pass: missing bodies, undefined
items, etc. Combinators would not absorb that layer.

---

## The load-bearing rule: line-start sigil

It is not "column 0". It is **first non-whitespace character of the line**,
after masking. Indented `  ~/foo { bar }` is DSL.

After masking, a leading `{ ... }` becomes `__BLOCK_…__`, so the classifier
sees `_`. Unclosed `{` is **not** masked (the masker leaves unbalanced
markers in place), so `{ unclosed` at line start is prose, while
`~/a { unclosed` starts with `~` and is a hard error with a misleading
"vote explanations must start with `{ ... }`" message.

`parse_full` then:

1. If a vote-explanation block is pending, the next non-empty line **must**
   be a vote/containment (hard error otherwise). Blank lines are skipped.
2. Else try `try_parse_aspect_line` (`:` / `:slug` / `:slug {prompt}`).
   Invalid colon lines (`:)`, `: note`, `:UPPER`) stay prose.
3. Else if the line looks like a DSL candidate (`-/`, `~`, `/`, `!`, `_`,
   `http://`, `https://`), flush prose and call `parse_line`.
4. Else the line is prose, including `#thread`, `@name`, and ordinary text.

`#`, `@`, and `"` have arms in `parse_line` that return "not a DSL line",
but `parse_full` never calls `parse_line` for those prefixes. They are dead
for the public entry point.

### Commitment table (this is the grammar)

The interesting bit is not "does it match" — it is **when we commit**.
`"not a DSL line"` is a sentinel string that `parse_full` turns back into
prose. Every other `DslError` fails the whole ingest.

| Looked like | Complete enough? | Outcome |
|---|---|---|
| `hello world` | — | prose |
| `#tag`, `@x`, `:)` | — | prose |
| `~a <:` (no rhs) | no | prose |
| `~a <: ~b extra` | extra tokens | prose |
| `~a <: ~b` | yes, both sides | **hard** — needs `{ ... }` |
| `~/a 2:1 ~/b` | yes, comparison | **hard** — needs leading `{ ... }` |
| `/languages/python {…}` | leading `/` | **hard** — use `~/` |
| `!anything` at line start | `!` | **hard** — unsupported command |
| `{why}` then `~/a 0:0 ~/b` | committed vote | **hard** — sides must be ≥ 1 |
| `{why}` then not a vote | pending block | **hard** — expected vote statement |

Incomplete containment falls back; a complete two-item `<:` line without an
explanation does not. A `~` line that contains a comparison without a
leading explanation is always a hard error. That distinction is a custom
predicate ("did both item refs parse and is the rest empty?"), not a
production in a grammar.

Aspect inheritance (`:beauty` then later votes pick up `aspect: Some("beauty")`)
and path sugar (`~/a/b/c` → leaf `~c` plus `c <: b <: a <: ~`) are
**semantic** walks after a successful parse. A combinator should not own them.

---

## Why combinators fight the outer loop

Parser combinators (PEG/nom-style) assume: try production A; if it fails,
backtrack and try B; the input is a language. This document is a **mixed
document**: most lines are not in the language. The default is prose. DSL
is an optional overlay that sometimes **must** fail instead of overlaying.

That is expressible in the abstract:

```text
line = dsl_line | prose_line
dsl_line = '~' cut item_or_vote
         | '{' cut vote_explanation_then_verdict
         | '/' cut error("use ~/")
         …
```

`cut` (chumsky) / `commit` (winnow/nom) is exactly "we have seen a sigil,
stop treating failure as prose". So the user's worry is half right:

- **Peeking a line-start sigil is easy.** Combinators can inspect the first
  non-ws character, or parse line-by-line and dispatch.
- **Programming the recovery policy is not easy**, and is the whole parser.
  Commitment is not "after the sigil" uniformly:
  - `~` commits for votes (`~/a 2:1 ~/b`) but **not** for incomplete `<:`.
  - `:` commits only for a valid slug / bare colon; otherwise prose.
  - `_` (`{...}` after mask) commits to a vote/containment, including across
    the next line (`pending_block`).
  - `/` and `!` commit to an error even if the rest looks like English.

You would encode that table as a pile of `cut` vs `or_not` vs custom
predicates. At that point the combinator library is a worse syntax for the
state machine we already have.

Two further mismatches:

**Multi-line constructs.** `{ explanation }` on one line and the verdict on
the next is not a nested tree; it is pending state in the line loop. Blank
lines between them are skipped. Combinators want nested `then`; this wants
a small automaton.

**Block masking.** Fences and braces span lines and nest. The masker exists
so the rest of the parser can be line-oriented. A combinator document parser
would have to parse ``` / `{` as first-class nested regions *before* line
classification, or keep the masker and combinator-ize only the leftovers.
Keeping the masker is the current architecture. Replacing it is possible
(nested `delimited` parsers are what combinators are good at) but then you
still need the line classifier on top, and you must not treat `~/x` inside a
fence as DSL — today the masker deletes those inner newlines from the split.

The `__BLOCK_` token is a leaky implementation detail: `_` is a line-start
"sigil" only because of masking. A nested combinator would remove that, which
is nice, and not worth a rewrite by itself.

---

## Error messages: already bespoke enough

A library's pitch is "better errors": spans, expected-sets, recovery.
Compare that to what we emit today:

- `leading { ... } blocks are vote explanations; item bodies belong after item paths`
- `item bodies must use { ... }; code fences belong inside body blocks`
- `vote ratio sides must be ≥ 1; use 1:1 for a tie or omit the vote`
- `item paths must use ~/ (e.g. ~/languages/python), not a leading /`
- `containment claims require a leading { ... } explanation block`

These teach the language. chumsky's `Rich` error would say something like
`found '2', expected '{'` or `expected ITEM_REF`. Mapping every one of those
back to the current hints is the same work we already do in `ok_or_else`,
plus fighting the library's expected-set merging.

The errors **are** the reason to stay ad-hoc, with two caveats.

### 1. No locations

`DslError` is `Parse(String)`. The outer loop already iterates lines and
never records which one failed. `/try`, `POST /ui`, RPC, and `sorterc`
surface `parse error` + the string, with no line/column. That is the actual
gap. Combinators would give spans for free; a `line_no` on the existing
loop is ~the same UX and no new dependency.

### 2. Unclosed delimiters are misdiagnosed

Unbalanced `{` / ``` stay in the text. `~/t/a { unclosed` is classified as
a `~` line that failed to parse an item body **or** a vote, and the fallback
message is the vote one (`vote explanations must start with a { ... } block`).
A nested `{` parser (combinator or hand-rolled) would say "unclosed `{` in
item body". That is worth fixing in the current masker/line parser; it is
not a reason to switch libraries.

### 3. `"not a DSL line"` is a string protocol

`parse_full` matches `msg == "not a DSL line"` to recover into prose. That
is the commitment table leaking into error text. A typed `DslError::NotDsl`
vs `DslError::Fail { line, message }` is the combinator `cut` distinction
without a combinator crate, and it would make the table reviewable.

---

## Library survey (if we did it anyway)

| Crate | Fit | Errors | Verdict |
|---|---|---|---|
| **chumsky** | `cut`, recovery, recursive `{` | Best expected-sets; labels are still generic unless mapped | Best of a bad fit; would own inner statements, not `parse_full` |
| **winnow** | Fast, easy custom `Err` enum | Errors are whatever we write (same as now) | Fine for `parse_item_name_at` / `parse_comparison_at` only |
| **nom** | Same family as winnow, more verbose | Weak unless we add locate + custom | No reason over winnow |
| **pest** | PEG file for inner statements | "expected rule X" | Cannot encode prose-default without a lexer in front |
| **lalrpop** | LR, needs a token lexer | Similar | Line-start prose is a lexer job; we'd still write the lexer |

None of these want to be the **document** parser. The honest split, if a
crate ever earns its keep:

- Keep `parse_full` + `BlockMasker` + prose buffer + `pending_block` + aspect
  state as imperative code.
- Optionally rewrite only `parse_item_name_at`, `parse_comparison_at`,
  `parse_containment_op_at` with winnow.

That last step replaces ~150 lines of index walking with combinator soup.
The item-ref lexer has three modes (URL, `-/`, `~` / `~/`) and a
punctuation-trim mode for prose linkification. Benefit is modest; behavior
drift risk is not.

---

## What would actually improve the parser

In order, without a combinator crate:

1. **Typed errors** — `NotDsl` (fallback) vs `Fail { line, message }`
   (commit). Stop string-matching `"not a DSL line"`.
2. **Line numbers** on `Fail`, threaded through validate / `/try` / CLI.
3. **Unclosed `{` / fence** as their own message, detected when the masker
   finishes at depth ≠ 0 on a committed DSL line.
4. Leave pedagogical strings as they are. They are the language's UX.

A combinator library becomes interesting only if the inner statement grammar
grows a lot (more operators, more statement kinds) *and* we are willing to
keep the outer classifier. Until then it is a dependency that fights the
prose-default rule we are not going to drop.
