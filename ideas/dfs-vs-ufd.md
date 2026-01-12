### Performance characteristics (DFS/BFS vs union-find)

**What we do now (DFS/BFS per render):**
- Build components from `voted_pairs` and run DFS/BFS.
- **Time**: \(O(n + E)\) where:
  - \(n\) = items in the aspect group (`idx_to_item.len()`)
  - \(E\) = number of voted pairs (`voted_pairs.len()`)
- **Memory**: \(O(n + E)\) if you materialize adjacency (we do).
- Good when:
  - you render occasionally
  - graphs are modest
  - you want simplicity and correctness

**Union-Find (DSU) maintained incrementally:**
- On each new vote pair \((i,j)\): do `union(i,j)`
- **Amortized time per vote**: ~\(O(\alpha(n))\) (effectively constant)
- **Memory**: \(O(n)\)
- To *list* members of each component you still need a pass over items:
  - grouping by root is \(O(n)\)

### “Votes stream in, don’t they?”
Yes: ingests/votes stream in and the reducer updates `GroupState` incrementally.

But the question is: **do we need components incrementally, or only when rendering?**
- If components are only needed for the aspect HTML page, recomputing \(O(n+E)\) at render time is usually fine.
- If you expect **very large tags** (thousands of items, lots of voted pairs) and frequent page loads, DSU is worth it:
  - you’d store DSU state in `GroupState` and update it in `apply_vote`
  - then rendering components is basically `O(n)` to bucket by root (+ sorting)

### Practical note for your system
You already pay the heavier cost: **rank centrality** per component (iterative). So component-finding is rarely the bottleneck unless \(E\) gets huge and you’re rendering often.

If you want truly streaming-friendly behavior, the next step would be:
- keep DSU in `GroupState`
- maintain it on each vote
- still keep `voted_pairs` for “has this pair been compared?” and for auditing, but you wouldn’t need to rebuild adjacency just to find components.