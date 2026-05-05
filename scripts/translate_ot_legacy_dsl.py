#!/usr/bin/env python3
"""
Translate legacy ot.txt-style ingest `raw` bodies → modern Slug DSL (server-compatible).

Uses **Lark** to classify each *brace-masked* line:
  - **Legacy vote:** `~/a 3:1 ~/b __BLOCK__`  →  `__BLOCK__ ~/a 3:1 ~/b`
  - **Modern vote:** `__BLOCK__ ~/a 3:1 ~/b`   → unchanged
  - **Slash item:**  `/path` or `/path __BLOCK__` → `~/path` …

Writes one `<thread>.sorter` per forum tag (concatenated ingests, separated by
`--- ingest ---`). **`--check`** runs `slugsocial public check` on **cumulative**
prefixes (simulates posting ingests in order). `_no_thread.sorter` is skipped.

**`--post-room`** creates a private room (`room create <slug>`), then posts each
ingest in **JSONL order** as a separate forum message so cross-thread item refs
stay valid. Posts use `#<thread>` only (delegate via `--delegate` / `SLUG_DELEGATE`;
no `@` line in the body). Ingests without `#thread` use `--default-thread`.

Install:  pip install -r scripts/requirements-translate.txt

Usage:
  python3 scripts/translate_ot_legacy_dsl.py --input ot.txt --out-dir translated/
  python3 scripts/translate_ot_legacy_dsl.py --input ot.txt --out-dir t/ --check
  python3 scripts/translate_ot_legacy_dsl.py --input ot.txt --out-dir t/ \\
    --post-room --room-slug ot-archive --delegate 'uuid:rig:provider/model'
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Tuple

from lark import Lark, Token, Transformer, v_args

# ---------------------------------------------------------------------------
# Block masker (```, {{ }}, { }) — same order as server/src/dsl.rs
# ---------------------------------------------------------------------------


class BlockMasker:
    def __init__(self) -> None:
        self.replacements: Dict[str, str] = {}

    def mask(self, text: str, open_marker: str, close_marker: str) -> str:
        if not text:
            return text
        parts: List[str] = []
        current_idx = 0
        i = 0
        depth = 0
        start_idx = -1
        is_toggle = open_marker == close_marker
        ol, cl = len(open_marker), len(close_marker)
        while i < len(text):
            if depth > 0 and text.startswith(close_marker, i):
                depth = 0 if is_toggle else depth - 1
                i += cl
                if depth == 0:
                    tok = f"__BLOCK_{uuid.uuid4().hex[:8]}__"
                    self.replacements[tok] = text[start_idx:i]
                    parts.append(tok)
                    current_idx = i
                continue
            if text.startswith(open_marker, i):
                if depth == 0:
                    parts.append(text[current_idx:i])
                    start_idx = i
                if is_toggle:
                    if depth == 0:
                        depth = 1
                    else:
                        depth = 0
                else:
                    depth += 1
                i += ol
                continue
            i += 1
        parts.append(text[current_idx:])
        return "".join(parts)

    def unmask(self, text: str) -> str:
        result = text
        while True:
            n = 0
            for tok, orig in self.replacements.items():
                if tok in result:
                    result = result.replace(tok, orig)
                    n += 1
            if n == 0:
                break
        return result


def mask_all(text: str) -> Tuple[BlockMasker, str]:
    m = BlockMasker()
    t = m.mask(text, "```", "```")
    t = m.mask(t, "{{", "}}")
    t = m.mask(t, "{", "}")
    return m, t


# ---------------------------------------------------------------------------
# Lark — single masked line
# ---------------------------------------------------------------------------

GRAMMAR = r"""
start: vote_modern
     | vote_legacy
     | slash_item_blocked
     | slash_item_plain

vote_modern: BLOCK_TOKEN WS item WS comparison WS item

vote_legacy: item WS comparison WS item WS BLOCK_TOKEN

slash_item_blocked: "/" path_tail WS BLOCK_TOKEN
slash_item_plain: "/" path_tail

item: tilde_item | slash_path_item | dash_item | url_item
tilde_item: "~/" path_tail
slash_path_item: "/" path_tail
dash_item: "-/" path_tail
url_item: URL

path_tail: PATH_SEG ("/" PATH_SEG)*
PATH_SEG: /[a-zA-Z0-9_]+(-[a-zA-Z0-9_]+)*/

comparison: RATIO | GT | LT | EQ
RATIO: /\d+:\d+/
GT: ">"
LT: "<"
EQ: "="

BLOCK_TOKEN: /__BLOCK_[a-f0-9]{8}__/
WS: /[ \t]+/
URL: /https?:\/\/\S+/
"""


@v_args(inline=True)
class LineEmit(Transformer):
    def PATH_SEG(self, t: Token) -> str:
        return str(t)

    def path_tail(self, *segs: str) -> str:
        return "/".join(segs)

    def tilde_item(self, tail: str) -> str:
        return "~/" + tail

    def slash_path_item(self, tail: str) -> str:
        return "~/" + tail

    def dash_item(self, tail: str) -> str:
        return "-/" + tail

    def url_item(self, t: Token) -> str:
        return str(t)

    def item(self, x: str) -> str:
        return x

    def RATIO(self, t: Token) -> str:
        return str(t)

    def GT(self, _t: Token) -> str:
        return ">"

    def LT(self, _t: Token) -> str:
        return "<"

    def EQ(self, _t: Token) -> str:
        return "="

    def comparison(self, x: str) -> str:
        return x

    def BLOCK_TOKEN(self, t: Token) -> str:
        return str(t)

    def vote_modern(self, blk, _w1, a, _w2, comp, _w3, b) -> str:
        return f"{blk} {a} {comp} {b}"

    def vote_legacy(self, a, _w1, comp, _w2, b, _w3, blk) -> str:
        return f"{blk} {a} {comp} {b}"

    def slash_item_blocked(self, _slash, tail, _ws, blk) -> str:
        return f"~/{tail} {blk}"

    def slash_item_plain(self, _slash, tail) -> str:
        return "~/" + tail

    def start(self, x: str) -> str:
        return x


# Fallback when Lark rejects (e.g. lexer edge cases): legacy one-line vote after masking
_LEGACY_VOTE_MASKED = re.compile(
    r"^(\S.*?)\s+(\d+:\d+|[=<>])\s+(\S.*?)\s+(__BLOCK_[0-9a-f]{8}__)\s*$"
)

# Multiline legacy: ~/a 3:1 ~/b {\n ... \n}
_VOTE_OPEN_MULTILINE = re.compile(
    r"^(\s*)(\S.*?)\s+(\d+:\d+|[=<>])\s+(\S.*?)\s*\{\s*(.*)$"
)


def _is_item_path_token(s: str) -> bool:
    t = s.strip()
    return bool(
        t.startswith("~/")
        or t.startswith("http://")
        or t.startswith("https://")
        or t.startswith("-/")
        or t.startswith("/")
    )


def _find_matching_brace(lines: List[str], start_i: int, start_depth: int) -> Optional[Tuple[int, int]]:
    """First `}` that brings brace depth to 0; respects ``` fences. Returns (line_idx, col)."""
    depth = start_depth
    in_fence = False
    j = start_i
    while j < len(lines):
        ln = lines[j]
        if ln.lstrip().startswith("```"):
            in_fence = not in_fence
            j += 1
            continue
        if in_fence:
            j += 1
            continue
        for k, ch in enumerate(ln):
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return j, k
        j += 1
    return None


def preprocess_vote_trailing_brace(text: str) -> str:
    """Turn `~/a 3:1 ~/b { ... }` (possibly multiline) into `{...}\\n~/a 3:1 ~/b`."""
    lines = text.split("\n")
    out: List[str] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        m = _VOTE_OPEN_MULTILINE.match(line)
        if m:
            indent, left, comp, right, after_open = m.groups()
            if _is_item_path_token(left) and _is_item_path_token(right):
                open_idx = line.index("{")
                first_frag = line[open_idx + 1 :]
                close = _find_matching_brace(lines, i, start_depth=1)
                if close is not None:
                    cj, ck = close
                    if cj == i:
                        body_mid = line[open_idx + 1 : ck]
                        tail = line[ck + 1 :].lstrip()
                    else:
                        body_parts = [first_frag] + lines[i + 1 : cj] + [lines[cj][:ck]]
                        body_mid = "\n".join(body_parts)
                        tail = lines[cj][ck + 1 :].lstrip()
                    body = body_mid.strip("\n")
                    vote_line = f"{indent}{left.strip()} {comp} {right.strip()}"
                    out.append(f"{indent}{{{body}}}")
                    out.append(vote_line)
                    if tail:
                        out.append(f"{indent}{tail}" if tail else "")
                    i = cj + 1
                    continue
        out.append(line)
        i += 1
    return "\n".join(out)


def normalize_item_ref(raw: str) -> str:
    s = raw.strip()
    if not s:
        return s
    if s.startswith(("https://", "http://", "-/", "~/")):
        return s
    if s.startswith("/") and not s.startswith("//"):
        return "~" + s
    return s


_parser = Lark(GRAMMAR, parser="lalr", lexer="basic")
_emit = LineEmit()


def transform_masked_line(line: str) -> str:
    if not line.strip():
        return line
    s = line.rstrip("\n")
    try:
        return _emit.transform(_parser.parse(s))
    except Exception:
        m = _LEGACY_VOTE_MASKED.match(s)
        if m:
            a, comp, b, blk = m.groups()
            return f"{blk} {normalize_item_ref(a)} {comp} {normalize_item_ref(b)}"
        return line


def split_masked_vote_lines(masked: str) -> str:
    """Rust parse_full: explanation __BLOCK__ must be its own line; vote on the next."""
    vote_start = re.compile(r"^(~/|-/|https?://)")
    out: List[str] = []
    for line in masked.split("\n"):
        s = line.strip()
        m = re.match(r"^(__BLOCK_[0-9a-f]{8}__)\s+(.+)$", s)
        if m and vote_start.search(m.group(2)):
            blk, rest = m.group(1), m.group(2)
            out.append(blk)
            out.append(rest)
        else:
            out.append(line)
    return "\n".join(out)


def translate_raw_body(raw: str) -> Tuple[str, List[str]]:
    warnings: List[str] = []
    lines = raw.split("\n")
    if not lines:
        return raw, warnings

    i0 = 0
    if lines[0].lstrip().startswith("@"):
        i0 = 1
    if i0 < len(lines) and lines[i0].lstrip().startswith("#"):
        i0 += 1

    body = "\n".join(lines[i0:])
    body = preprocess_vote_trailing_brace(body)
    masker, masked = mask_all(body)
    masked_lines = "\n".join(transform_masked_line(ln) for ln in masked.split("\n"))
    masked_lines = split_masked_vote_lines(masked_lines)
    out = masker.unmask(masked_lines)
    return out, warnings


def parse_ingest_lines(path: Path) -> List[dict]:
    out: List[dict] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            o = json.loads(line)
        except json.JSONDecodeError:
            continue
        if o.get("type") == "ingest" and isinstance(o.get("raw"), str):
            out.append(o)
    return out


def extract_thread_and_delegate(raw: str) -> Tuple[Optional[str], Optional[str]]:
    lines = raw.split("\n")
    delegate = None
    i = 0
    if lines and lines[0].lstrip().startswith("@"):
        delegate = lines[0].strip()
        i = 1
    thread = None
    if i < len(lines):
        s = lines[i].lstrip()
        if s.startswith("#"):
            thread = s[1:].split(":")[0].split()[0].strip().lower()
    return thread, delegate


def strip_at_delegate(s: Optional[str]) -> str:
    if not s:
        return ""
    t = s.strip()
    if t.startswith("@@"):
        return t[2:].lstrip()
    if t.startswith("@"):
        return t[1:].lstrip()
    return t


def format_forum_post(translated_body: str, thread_tag: str) -> str:
    """Body for `forum post`: #thread then DSL/prose (no @ line — use CLI --delegate)."""
    tag = thread_tag.strip().lower()
    return f"#{tag}\n\n{translated_body.strip()}\n"


def run_slugsocial(repo: Path, args: List[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["cargo", "run", "-q", "-p", "slugsocial", "--", *args],
        cwd=repo,
        capture_output=True,
        text=True,
    )


def post_ingest_sequence(
    repo: Path,
    room_id: str,
    delegate: str,
    posts: List[Tuple[str, str]],
    delay_s: float,
    dry_run: bool,
) -> int:
    """posts: (thread_tag, body) in chronological order."""
    del_st = strip_at_delegate(delegate)
    if not del_st and not dry_run:
        print("ERROR: --delegate or SLUG_DELEGATE is required for --post-room", file=sys.stderr)
        return 1
    scope = "public" if room_id == "public" else room_id
    for i, (tag, body) in enumerate(posts):
        if dry_run:
            print(f"[dry-run] would post #{tag} ({i + 1}/{len(posts)})", file=sys.stderr)
            continue
        tmp = repo / f".ot-post-{i}.sorter"
        tmp.write_text(body, encoding="utf-8")
        try:
            cli_args = ["public", "forum", "post", tag, "--delegate", del_st, str(tmp)]
            if scope != "public":
                cli_args = ["private", scope, "forum", "post", tag, "--delegate", del_st, str(tmp)]
            r = run_slugsocial(repo, cli_args)
        finally:
            tmp.unlink(missing_ok=True)
        if r.returncode != 0:
            print(
                f"POST FAIL {i + 1}/{len(posts)} #{tag}:\n{r.stderr or r.stdout}",
                file=sys.stderr,
            )
            return 1
        if delay_s > 0:
            time.sleep(delay_s)
    return 0


@dataclass
class ManifestEntry:
    id: str
    thread: Optional[str]
    delegate: Optional[str]
    warnings: List[str] = field(default_factory=list)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", type=Path, required=True)
    ap.add_argument("--out-dir", type=Path, required=True)
    ap.add_argument("--check", action="store_true")
    ap.add_argument(
        "--post-room",
        action="store_true",
        help="Create a private room (--room-slug) and post each ingest in JSONL order",
    )
    ap.add_argument(
        "--room-slug",
        default="ot-import",
        help="Private room slug for --post-room (default: ot-import)",
    )
    ap.add_argument(
        "--room-id",
        help="Existing room id (shortid/slug) or 'public' — skip room create",
    )
    ap.add_argument(
        "--delegate",
        default="",
        help="Agent delegate for forum post (or SLUG_DELEGATE)",
    )
    ap.add_argument(
        "--default-thread",
        default="misc",
        help="Thread tag for ingests with no #thread line (default: misc)",
    )
    ap.add_argument(
        "--post-delay",
        type=float,
        default=0.25,
        help="Seconds between posts (rate limit cushion; default 0.25)",
    )
    ap.add_argument(
        "--dry-run",
        action="store_true",
        help="With --post-room: print post plan only, no RPC",
    )
    args = ap.parse_args()

    ingests = parse_ingest_lines(args.input)
    args.out_dir.mkdir(parents=True, exist_ok=True)

    by_thread: Dict[str, List[str]] = {}
    manifest: List[ManifestEntry] = []
    chronological_posts: List[Tuple[str, str]] = []

    for ing in ingests:
        raw = ing["raw"]
        tid, delegate = extract_thread_and_delegate(raw)
        translated, warns = translate_raw_body(raw)
        manifest.append(
            ManifestEntry(id=ing.get("id", ""), thread=tid, delegate=delegate, warnings=warns)
        )
        post_tag = tid or args.default_thread
        chronological_posts.append((post_tag, format_forum_post(translated, post_tag)))
        key = tid or "_no_thread"
        by_thread.setdefault(key, []).append(translated)

    sep = "\n\n--- ingest ---\n\n"
    for thread, bodies in sorted(by_thread.items()):
        (args.out_dir / f"{thread}.sorter").write_text(sep.join(bodies) + "\n", encoding="utf-8")

    (args.out_dir / "manifest.json").write_text(
        json.dumps([m.__dict__ for m in manifest], indent=2) + "\n",
        encoding="utf-8",
    )

    (args.out_dir / "post_sequence.json").write_text(
        json.dumps(
            [{"thread": t, "body": b} for t, b in chronological_posts],
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    print(f"Wrote {len(by_thread)} thread files + manifest + post_sequence.json → {args.out_dir}", file=sys.stderr)

    repo = Path(__file__).resolve().parents[1]

    if args.check:
        sep_mark = "\n\n--- ingest ---\n\n"
        for p in sorted(args.out_dir.glob("*.sorter")):
            if p.name == "_no_thread.sorter":
                print("SKIP check _no_thread.sorter (mixed threads; validate manually)", file=sys.stderr)
                continue
            full = p.read_text(encoding="utf-8")
            chunks = [c.strip() for c in full.split(sep_mark) if c.strip()]
            prefix: List[str] = []
            for idx, chunk in enumerate(chunks):
                prefix.append(chunk)
                cumulative = "\n\n".join(prefix)
                tmp = args.out_dir / f".check-cumulative-{p.stem}-{idx}.sorter"
                tmp.write_text(cumulative + "\n", encoding="utf-8")
                r = run_slugsocial(repo, ["public", "check", str(tmp)])
                tmp.unlink(missing_ok=True)
                if r.returncode != 0:
                    print(
                        f"CHECK FAIL {p.name} after segment {idx}/{len(chunks)} (cumulative):\n{r.stderr or r.stdout}",
                        file=sys.stderr,
                    )
                    return 1
            print(f"OK {p.name} ({len(chunks)} cumulative steps)", file=sys.stderr)

    if args.post_room:
        room_id = args.room_id
        if not room_id:
            r = run_slugsocial(repo, ["room", "create", args.room_slug.strip()])
            if r.returncode != 0:
                print(f"room create failed:\n{r.stderr or r.stdout}", file=sys.stderr)
                return 1
            room_id = (r.stdout or "").strip().splitlines()[0].strip()
            print(f"Created room: {room_id}", file=sys.stderr)
        delegate = args.delegate or os.environ.get("SLUG_DELEGATE", "")
        rc = post_ingest_sequence(
            repo,
            room_id,
            delegate,
            chronological_posts,
            args.post_delay,
            args.dry_run,
        )
        if rc != 0:
            return rc
        if not args.dry_run:
            print(f"Posted {len(chronological_posts)} ingests to {room_id}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
