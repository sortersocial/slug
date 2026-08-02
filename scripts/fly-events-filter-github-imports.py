#!/usr/bin/env python3
"""Filter GitHub-resolver import spam from a slug events.jsonl.

Removes:
  - Ingest events where principal == system:github-resolver
  - Ingest events whose thread_tag starts with import:https:::github.com:
    (optionally narrowed with --only-substring, e.g. litellm)
  - PostRedacted events whose post_id refers to a removed Ingest id

Does not touch unrelated human / AI posts.

Usage:
  scripts/fly-events-filter-github-imports.py INPUT [-o OUTPUT] [--dry-run]
  scripts/fly-events-filter-github-imports.py INPUT -o OUT --only-substring litellm
  scripts/fly-events-filter-github-imports.py INPUT --report-only

Defaults write OUTPUT next to INPUT as <stem>.cleaned.jsonl when -o omitted
(unless --dry-run / --report-only).
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path
from typing import Any


GITHUB_RESOLVER = "system:github-resolver"
IMPORT_PREFIX = "import:https:::github.com:"


def event_payload(obj: dict[str, Any]) -> tuple[str, dict[str, Any]]:
    etype = obj.get("type")
    if not isinstance(etype, str):
        raise ValueError("missing type")
    return etype, obj


def ingest_matches(ev: dict[str, Any], only_substring: str | None) -> bool:
    principal = ev.get("principal") or ""
    thread_tag = ev.get("thread_tag") or ""
    is_resolver = principal == GITHUB_RESOLVER
    is_gh_import = isinstance(thread_tag, str) and thread_tag.startswith(IMPORT_PREFIX)
    if not (is_resolver or is_gh_import):
        return False
    if only_substring:
        hay = f"{principal}\n{thread_tag}\n{ev.get('raw') or ''}"
        return only_substring.lower() in hay.lower()
    return True


def classify_line(
    line: str,
    only_substring: str | None,
) -> tuple[str, str | None, dict[str, Any] | None]:
    """Return (kind, ingest_id_or_none, parsed). kind in keep|drop_ingest|drop_redact|bad."""
    raw = line.strip()
    if not raw:
        return "keep", None, None
    try:
        obj = json.loads(raw)
    except json.JSONDecodeError:
        return "bad", None, None
    if not isinstance(obj, dict):
        return "bad", None, None
    etype, ev = event_payload(obj)
    if etype == "ingest":
        if ingest_matches(ev, only_substring):
            return "drop_ingest", ev.get("id"), ev
        return "keep", None, ev
    if etype == "post_redacted":
        # Second pass decides; mark tentatively
        return "maybe_redact", ev.get("post_id"), ev
    return "keep", None, ev


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", type=Path, help="Source events.jsonl")
    ap.add_argument("-o", "--output", type=Path, help="Destination cleaned jsonl")
    ap.add_argument(
        "--only-substring",
        default=None,
        help="Only drop events whose principal/thread_tag/raw contain this (case-insensitive)",
    )
    ap.add_argument(
        "--dry-run",
        action="store_true",
        help="Report what would be removed; do not write output",
    )
    ap.add_argument(
        "--report-only",
        action="store_true",
        help="Alias for --dry-run",
    )
    args = ap.parse_args()
    dry = args.dry_run or args.report_only

    if not args.input.is_file():
        print(f"error: input not found: {args.input}", file=sys.stderr)
        return 1

    out = args.output
    if out is None and not dry:
        out = args.input.with_name(args.input.stem + ".cleaned.jsonl")

    lines = args.input.read_text(encoding="utf-8").splitlines(keepends=True)

    # Pass 1: collect ingest ids to drop + sample tags
    drop_ids: set[str] = set()
    drop_ingest_lines: set[int] = set()
    tag_counts: Counter[str] = Counter()
    samples: list[str] = []
    bad = 0

    for i, line in enumerate(lines):
        kind, iid, ev = classify_line(line, args.only_substring)
        if kind == "bad":
            bad += 1
            continue
        if kind == "drop_ingest":
            drop_ingest_lines.add(i)
            if isinstance(iid, str) and iid:
                drop_ids.add(iid)
            if ev:
                tag = str(ev.get("thread_tag") or "")
                tag_counts[tag] += 1
                if len(samples) < 12:
                    samples.append(
                        f"  ingest id={iid} ts={ev.get('ts')} tag={tag!r} principal={ev.get('principal')!r}"
                    )

    # Pass 2: drop matching redactions + emit
    kept: list[str] = []
    drop_redact = 0
    drop_ingest = 0
    kept_n = 0

    for i, line in enumerate(lines):
        if i in drop_ingest_lines:
            drop_ingest += 1
            continue
        kind, pid, ev = classify_line(line, args.only_substring)
        if kind == "maybe_redact" and isinstance(pid, str) and pid in drop_ids:
            drop_redact += 1
            continue
        # Also drop PostRedacted by github-resolver that target dropped ids already handled;
        # additionally drop redactions authored as github-resolver for safety when post is gone.
        if kind == "maybe_redact" and ev and (ev.get("principal") == GITHUB_RESOLVER):
            # Only drop if the referenced post was a dropped ingest; otherwise keep
            # (shouldn't happen for human posts).
            if isinstance(pid, str) and pid in drop_ids:
                drop_redact += 1
                continue
        kept.append(line if line.endswith("\n") else line + "\n")
        kept_n += 1

    removed = drop_ingest + drop_redact
    print(f"input:  {args.input} ({len(lines)} lines)")
    if args.only_substring:
        print(f"filter: substring={args.only_substring!r}")
    print(f"remove: {drop_ingest} ingest(s), {drop_redact} post_redacted, total={removed}")
    print(f"keep:   {kept_n} lines (bad/unparsed kept as-is: {bad})")
    if tag_counts:
        print("removed thread_tag counts:")
        for tag, n in tag_counts.most_common(40):
            print(f"  {n:5d}  {tag}")
    if samples:
        print("sample removed ingests:")
        for s in samples:
            print(s)

    if dry:
        print("dry-run: no output written")
        return 0

    assert out is not None
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("".join(kept), encoding="utf-8")
    print(f"output: {out} ({kept_n} lines)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
