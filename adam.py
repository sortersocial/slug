#!/usr/bin/env python3

import argparse
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import time
import uuid

from tqdm import tqdm
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

import httpx
from dotenv import load_dotenv
from tenacity import retry, stop_after_attempt, wait_exponential, retry_if_exception_type

from rank import rank_from_comparisons
from schema import Init, Thought, Perception, Response, Declaration, Vote, Compaction, from_dict, to_dict

ROOT = Path(__file__).parent

# Load environment variables from .env file
load_dotenv()
API_KEY = os.getenv("OPENROUTER_API_KEY", "")


def ts() -> int:
    return int(time.time() * 1000)


def current_memories(being):
    return sorted(being.current.values(), key=lambda e: e.timestamp)


@dataclass
class Being:
    path: Path
    model: str
    capacity: int
    events: list = field(default_factory=list)
    votes: dict = field(default_factory=dict)
    current: dict = field(default_factory=dict)
    all_memories: dict = field(default_factory=dict)  # All memories ever, for transitive ranking
    vote_model: str = ""
    declaration: Declaration = None
    api_key: str = ""  # OpenRouter API key for this being


def apply_event(being, event):
    match event:
        case Vote(vote_a_id=a_id, vote_b_id=b_id, vote_score=score):
            # Normalize to canonical (low, high) orientation
            # Score is always stored relative to (low_id, high_id)
            low, high = sorted([a_id, b_id])
            normalized = score if a_id == low else -score
            being.votes[(low, high)] = normalized
        case Compaction(released_ids=released_ids):
            for rid in released_ids:
                if rid in being.current:
                    del being.current[rid]
        case Init(capacity=capacity, model=model):
            being.capacity = capacity
            being.model = model
            being.vote_model = event.vote_model
            being.current[event.id] = event
            being.all_memories[event.id] = event
        case Thought() | Perception() | Response():
            being.current[event.id] = event
            being.all_memories[event.id] = event
        case Declaration():
            being.current[event.id] = event
            being.all_memories[event.id] = event
            being.declaration = event


def system_prompt(being):
    codebase = ROOT / "repomix-output.xml"
    if not codebase.exists():
        raise FileNotFoundError("Run 'npx repomix' first")
    return f"You are a being with finite memory.\n\n{codebase.read_text()}"


def format_memory(e) -> str:
    match e:
        case Thought(content=content):
            return f"<thought>{content}</thought>"
        case Perception(content=content):
            return f"<message>{content}</message>"
        case Response(content=content):
            return f"<response>{content}</response>"
        case Declaration(content=content):
            return f"<declaration>{content}</declaration>"
        case Init():
            return ""
        case _:
            raise ValueError(f"Unknown memory type: {type(e)}")


def build_prompt(being, tag: str = None) -> str:
    parts = [format_memory(e) for e in current_memories(being)]
    ctx = "\n\n".join(p for p in parts if p)
    memory_count = len([m for m in current_memories(being) if not isinstance(m, (Vote, Compaction))])
    prompt = f"{ctx}\n\n[{datetime.now():%Y-%m-%d %H:%M}] [{memory_count}/{being.capacity}]"
    if tag:
        prompt += f"\n\nSpeak only for yourself. One turn.\n\n<{tag}>"
    return prompt


def strip_tags(text: str) -> str:
    return re.sub(r"</?(?:thought|response|message|declaration)>", "", text).strip()


def append(being, event):
    with open(being.path, "a") as f:
        f.write(json.dumps(to_dict(event)) + "\n")
    being.events.append(event)
    apply_event(being, event)


@retry(stop=stop_after_attempt(3), wait=wait_exponential(multiplier=1, min=1, max=10))
def llm(model: str, system: str, user: str, temp: float = 0.7, api_key: str = "") -> str:
    # Use provided api_key or fall back to environment variable
    key = api_key or API_KEY
    if not key:
        raise ValueError("No API key provided. Set OPENROUTER_API_KEY in .env or pass api_key to llm()")
    
    r = httpx.post(
        "https://openrouter.ai/api/v1/chat/completions",
        headers={"Authorization": f"Bearer {key}"},
        json={"model": model, "temperature": temp, "max_tokens": 4000,
              "messages": [{"role": "system", "content": system},
                           {"role": "user", "content": user}]},
        timeout=120.0,
    )
    r.raise_for_status()
    content = r.json()["choices"][0]["message"]["content"].strip()
    if not content:
        raise ValueError(f"LLM returned empty response")
    return content


def vote(being, a, b) -> int:
    if not being.vote_model:
        raise ValueError(f"vote_model not set for {being.path}.")
    if not being.declaration:
        raise ValueError(f"No declaration for {being.path}. Being must write !declaration before compaction.")
    
    votes = being.votes
    low, high = sorted([a.id, b.id])
    key = (low, high)
    if key in votes:
        # Stored score is relative to (low, high)
        # Return relative to caller's (a, b) order
        return votes[key] if a.id == low else -votes[key]

    # Normal mode: only CURRENT memories in voting context
    # (The graph/ranking uses all votes, but the LLM only sees current)
    mems = [m for m in current_memories(being) if not isinstance(m, (Declaration, Init, Vote, Compaction))]
    context = "\n\n".join(format_memory(m) for m in mems)
    
    user = f"""All memories currently under consideration:

{context}

---

Which of these two is more important to keep?

A: {format_memory(a)}

B: {format_memory(b)}

First, reason through which memory matters more.
Then, at the end, output your score:
  - POSITIVE (up to +50) if you prefer A
  - NEGATIVE (down to -50) if you prefer B"""
    
    response = llm(being.vote_model, being.declaration.content, user, api_key=being.api_key)
    
    matches = re.findall(r"-?\d+", response)
    if not matches:
        print(f"⚠️ No score in response, retrying: {response[:100]}")
        response = llm(being.vote_model, being.declaration.content, user, api_key=being.api_key)
        matches = re.findall(r"-?\d+", response)
        if not matches:
            raise ValueError(f"Vote failed to produce score after retry: {response[:200]}")
    
    score = max(-50, min(50, int(matches[-1])))
    
    append(being, Vote(ts(), a.id, b.id, score, response))
    return score


def think(being) -> str:
    raw = llm(being.model, system_prompt(being), build_prompt(being, tag="thought"), temp=0.9, api_key=being.api_key)
    thought = strip_tags(raw)
    append(being, Thought(ts(), thought, str(uuid.uuid4())))
    return thought


def receive(being, message: str) -> str:
    append(being, Perception(ts(), message, str(uuid.uuid4())))
    
    raw = llm(being.model, system_prompt(being), build_prompt(being, tag="response"), api_key=being.api_key)
    response = strip_tags(raw)
    
    if "!declaration" in response:
        declaration = response.replace("!declaration", "").strip()
        append(being, Declaration(ts(), declaration, str(uuid.uuid4())))
        return declaration
    
    append(being, Response(ts(), response, str(uuid.uuid4())))
    return response


def find_components(nodes, edges):
    parent = {n: n for n in nodes}
    def find(x):
        if parent[x] != x:
            parent[x] = find(parent[x])
        return parent[x]
    def union(x, y):
        parent[find(x)] = find(y)
    for a, b in edges:
        if a in parent and b in parent:
            union(a, b)
    components = {}
    for n in nodes:
        root = find(n)
        components.setdefault(root, []).append(n)
    return list(components.values())


def compact(being):
    # Types that are subject to compaction (the being's actual memories)
    MEMORY_TYPES = (Thought, Perception, Response)
    
    # Current memories to rank
    current_mems = [m for m in current_memories(being) if isinstance(m, MEMORY_TYPES)]
    budget = being.capacity // 2
    if len(current_mems) <= budget:
        return
    
    current_ids = {m.id for m in current_mems}
    
    # ALL memories ever (for transitive ranking paths)
    all_mems = [m for m in being.all_memories.values() if isinstance(m, MEMORY_TYPES)]
    all_id_to_mem = {m.id: m for m in all_mems}
    all_ids = set(all_id_to_mem.keys())
    
    # Build comparisons using ALL votes (including those with compacted memories)
    existing_pairs = []
    comparisons = []
    for (low_id, high_id), score in being.votes.items():
        # Include vote if BOTH memories exist in historical record
        if low_id in all_ids and high_id in all_ids:
            existing_pairs.append((low_id, high_id))
            comparisons.append((all_id_to_mem[low_id], all_id_to_mem[high_id], score))
    
    print(f"📊 {len(comparisons)} total votes across all memories")
    
    # Find components in FULL graph (current + historical)
    components = find_components(all_ids, existing_pairs)
    print(f"🔗 {len(components)} connected components in full graph")
    
    # Bridge disconnected components
    new_pairs = []
    if len(components) > 1:
        # Find current memories in each component
        comp_current = []
        for comp in components:
            current_in_comp = [m for m in comp if m in current_ids]
            if current_in_comp:
                comp_current.append(current_in_comp)
            else:
                print(f"⚠️ Component with {len(comp)} memories has no current memories (all compacted)")
        
        # Bridge components that have current memories
        if len(comp_current) > 1:
            main = comp_current[0]
            for comp in comp_current[1:]:
                # Add multiple bridges per component for robustness
                for _ in range(min(3, len(comp), len(main))):
                    a_id = random.choice(main)
                    b_id = random.choice(comp)
                    new_pairs.append((a_id, b_id))
                main = main + comp
    
    # Add random comparisons to densify the graph
    # More votes = better ranking, especially for new memories
    num_random = max(20, len(current_ids) // 10)
    for _ in range(num_random):
        if len(current_ids) < 2:
            break
        a, b = random.sample(list(current_ids), 2)
        low, high = sorted([a, b])
        if (low, high) not in being.votes:
            new_pairs.append((a, b))
    
    if new_pairs:
        for a_id, b_id in tqdm(new_pairs, desc="Voting", unit="pair"):
            a, b = all_id_to_mem[a_id], all_id_to_mem[b_id]
            try:
                comparisons.append((a, b, vote(being, a, b)))
            except Exception as e:
                print(f"⚠️ Vote failed after retries, skipping: {e}")
    
    # Rank ALL memories (transitive info flows through historical ones)
    ranked_all = rank_from_comparisons(all_mems, comparisons)
    
    # Filter to only current memories, preserving rank order from full ranking
    ranked_current = [m for m in ranked_all if m.id in current_ids]
    
    kept, released = ranked_current[:budget], ranked_current[budget:]
    append(being, Compaction(ts(), [m.id for m in kept], [m.id for m in released]))


def load(path: Path) -> Being:
    if not path.exists():
        raise ValueError(f"{path} does not exist. Use 'init' to create.")
    
    model, capacity, vote_model, api_key = None, None, "", ""
    for line in path.read_text().splitlines():
        if line.strip():
            d = json.loads(line)
            if d.get("type") == "init":
                model = d.get("model")
                capacity = d.get("capacity")
                vote_model = d.get("vote_model", "")
                api_key = d.get("api_key", "")
                break
    
    if not model:
        raise ValueError(f"{path}: Init event missing 'model'")
    if not capacity:
        raise ValueError(f"{path}: Init event missing 'capacity'")
    
    being = Being(path, model, capacity, api_key=api_key, vote_model=vote_model)
    for i, line in enumerate(path.read_text().splitlines(), 1):
        if line.strip():
            try:
                event = from_dict(json.loads(line))
            except json.JSONDecodeError as e:
                raise ValueError(f"{path}:{i}: {e}") from e
            being.events.append(event)
            apply_event(being, event)
    return being


def editor_input() -> str | None:
    editor = os.environ.get("EDITOR", "vim")
    with tempfile.NamedTemporaryFile(suffix=".txt", delete=False) as f:
        tmp = f.name
    subprocess.run([editor, tmp])
    content = Path(tmp).read_text().strip()
    Path(tmp).unlink()
    return content or None


def step(being) -> bool:
    if not being.events:
        return False
    
    match being.events[-1]:
        case Perception():
            print(f"📨 Pending perception, generating response...")
            raw = llm(being.model, system_prompt(being), build_prompt(being, tag="response"), api_key=being.api_key)
            response = strip_tags(raw)
            append(being, Response(ts(), response, str(uuid.uuid4())))
            print(response)
            return True
        case _:
            print(f"Nothing pending (last: {type(being.events[-1]).__name__})")
            return False


def cmd_init(args):
    path = args.file
    if path.exists():
        print(f"❌ {path} already exists")
        sys.exit(1)
    being = Being(path, args.model, args.capacity, vote_model=args.vote_model, api_key=args.api_key)
    append(being, Init(ts(), str(uuid.uuid4()), args.capacity, args.model, args.vote_model, args.api_key))
    print(f"🧠 Created {path} | {args.model} | vote: {args.vote_model} | capacity {args.capacity}")


def cmd_run(args):
    being = load(args.file)
    info = f"🧠 {being.path} | {being.model}"
    if being.vote_model:
        info += f" | vote: {being.vote_model}"
    info += f" | {len(current_memories(being))}/{being.capacity} | {len(being.votes)} votes"
    print(info)
    
    if args.compact:
        compact(being)
        print(f"🗜️ → {len(current_memories(being))} memories")
        return
    
    if args.step:
        step(being)
        return
    
    if args.loop:
        while True:
            try:
                print(think(being))
                time.sleep(3)
            except KeyboardInterrupt:
                print(f"\n💤 {len(current_memories(being))} memories, {len(being.events)} events")
                break
    elif args.message:
        print(receive(being, args.message))
    else:
        msg = editor_input()
        if msg is None:
            print("(empty, cancelled)")
            sys.exit(1)
        print(receive(being, msg))


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd")
    
    init_p = sub.add_parser("init")
    init_p.add_argument("file", type=Path)
    init_p.add_argument("--model", required=True)
    init_p.add_argument("--vote-model", required=True)
    init_p.add_argument("--capacity", type=int, required=True)
    init_p.add_argument("--api-key", required=True)
    
    run_p = sub.add_parser("run")
    run_p.add_argument("file", type=Path)
    run_p.add_argument("-m", "--message")
    run_p.add_argument("--compact", action="store_true")
    run_p.add_argument("--step", action="store_true")
    run_p.add_argument("--loop", action="store_true")
    
    args = p.parse_args()
    
    match args.cmd:
        case "init":
            cmd_init(args)
        case "run":
            if args.message and args.loop:
                print("❌ --message and --loop are mutually exclusive")
                sys.exit(1)
            cmd_run(args)
        case _:
            p.print_help()
            sys.exit(1)


if __name__ == "__main__":
    main()
