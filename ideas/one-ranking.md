# One Ranking

The aspect system is being removed. All votes fold into a single unnamed ranking
per scope. This document explains why.

---

## What Aspects Were Supposed To Do

The original design allowed votes to be tagged with an aspect (`:beauty`,
`:depth`, `:comfort`). Each aspect maintained a separate ranking group. Visiting
`~/psalms?aspect=beauty` showed a ranking of psalms by beauty. Visiting
`~/psalms?aspect=depth` showed a different ranking.

The idea: items can be good in different ways, so different comparison campaigns
should stay separate.

---

## What Aspects Actually Did

They made the singleness disappear.

The whole point of sorter is that you are forced to make one judgment. Psalm 88
vs psalm 23 — which is better? You have to look at both from every angle and
still commit. That commitment, that compression of many dimensions into one
verdict, is the meaning-making act. It is also the novel thing. Most ranking
systems decompose quality into measurable sub-axes. Sorter doesn't. You just
decide.

Multi-aspect voting gives you an escape hatch. You never have to say 88 beats
23. You say 88 wins on `:strangeness`, 23 wins on `:comfort`, and now you've
avoided the question entirely. The ranking loses its force because it was never
really made.

The aspects also generated cascading UI complexity:

- Every ontology URL carried `?aspect=` as a required parameter
- Visiting a path without an aspect required detecting a "default" aspect and
  redirecting — fragile, slow, and sometimes wrong
- The breadcrumb and scope logic had to be aware of aspect selection at every
  level
- The "no aspects yet" empty state appeared at paths that had items and votes,
  just not in the expected aspect
- Every rendering function threaded aspect through as a dimension

Removing aspects removes all of this.

---

## One Ranking

Each scope has one ranking. Votes are not tagged with aspects. When you cast a
vote between two items, it contributes to the single ranking for those items.
The connected components, the rank centrality algorithm, and the path hierarchy
all work exactly as before — they just no longer have an aspect dimension.

Visiting `~/psalms` shows the ranking of psalms.  
Visiting `~/psalms/88` shows where psalm 88 sits in that ranking, and any
rankings among its children if they exist.  
No gate. No selection. No redirect.

---

## What This Costs

The ability to ask "which psalm is most comforting" as a separate question from
"which psalm is best" is gone. If you want to explore that question you do it
the same way you explore any question: run a new comparison campaign, in a new
namespace, or in a new thread. The ontology doesn't need to store multiple
answers to the same population of items. It needs to store the best current
answer.

---

## The DSL

The `:aspect` syntax can be deprecated or repurposed. Existing `.sorter` files
with `:aspect` declarations will need migration — the simplest path is to
collapse all votes regardless of aspect into the single group per scope.

---

## What This Unblocks

- `~/path` shows a ranking immediately, no redirect, no detection
- Item pages (`~/parables/wise-foolish-builders`) show where the item ranks
  among its siblings, plus any rankings among its children, all in one view
- `default_aspect_for_scope` and the aspect-selector UI go away entirely
- `?aspect=` disappears from all URLs
- `bc_path` can replace `bc_ontology` everywhere without needing to carry aspect
  state through the breadcrumb
- The `render_path_view` / `render_aspect_view` split simplifies into one
  function per path depth

The path is the address. The ranking is the content. That's it.
