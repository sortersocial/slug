/// Specter/Rama-style navigational path DSL for ReducerState mutations.
///
/// This module provides declarative macros that compile to the same HashMap/Vec/HashSet
/// operations the reducer already uses, but with a composable path-expression syntax
/// inspired by Clojure's Specter library.
///
/// # Navigators
///
/// | Navigator | Description |
/// |---|---|
/// | `keypath(k)` | Navigate into a map by key (like `entry(k).or_default()`) |
/// | `setval(v)` | Set the terminal value |
/// | `set_elem(v)` | Insert into a HashSet at the current position |
/// | `push_front(v)` | Push to front of VecDeque at current position |
/// | `push_back(v)` | Push to back of Vec/VecDeque at current position |
/// | `selected(pred, ...)` | Only proceed if predicate is true |
/// | `view(f, ...)` | Apply function to get a derived navigated value |
/// | `multi_path!` | Fan out to multiple paths in one expression |
///
/// Navigate into a HashMap by key (inserting default if absent) and perform an operation.
///
/// ```ignore
/// // Equivalent to: self.item_children.entry(parent).or_default().insert(child);
/// nav!(self.item_children, keypath(parent), set_elem(child));
/// ```
#[macro_export]
macro_rules! nav {
    // Terminal: keypath -> setval (HashMap insert with explicit value)
    ($root:expr, keypath($key:expr), setval($val:expr)) => {
        $root.insert($key.clone(), $val)
    };

    // Terminal: keypath -> set_elem (navigate into map, insert into inner HashSet)
    ($root:expr, keypath($key:expr), set_elem($val:expr)) => {
        $root.entry($key.clone()).or_default().insert($val)
    };

    // Terminal: keypath -> push_front (navigate into map, push_front on inner VecDeque)
    ($root:expr, keypath($key:expr), push_front($val:expr)) => {
        $root.entry($key.clone()).or_default().push_front($val)
    };

    // Terminal: keypath -> push_back (navigate into map, push_back on inner Vec/VecDeque)
    ($root:expr, keypath($key:expr), push_back($val:expr)) => {
        $root.entry($key.clone()).or_default().push_back($val)
    };

    // Terminal: keypath -> or_insert (entry or_insert — first-write-wins)
    ($root:expr, keypath($key:expr), or_insert($val:expr)) => {
        $root.entry($key.clone()).or_insert($val)
    };

    // Conditional: selected(predicate, navigator_chain...)
    // Only applies the nested navigation if predicate is true.
    ($root:expr, selected($pred:expr, $($rest:tt)*)) => {
        if $pred {
            nav!($root, $($rest)*)
        }
    };

    // Two-level keypath: keypath -> keypath -> terminal
    ($root:expr, keypath($k1:expr), keypath($k2:expr), $($rest:tt)*) => {
        nav!($root.entry($k1.clone()).or_default(), keypath($k2), $($rest)*)
    };

    // View: apply a function to the current value and continue navigation
    ($root:expr, view($f:expr), $($rest:tt)*) => {{
        let v = $f(&$root);
        nav!(v, $($rest)*)
    }};

    // Terminal: set_elem on root (insert into HashSet directly)
    ($root:expr, set_elem($val:expr)) => {
        $root.insert($val)
    };

    // Terminal: push_front on root (push_front on VecDeque directly)
    ($root:expr, push_front($val:expr)) => {
        $root.push_front($val)
    };

    // Terminal: push_back on root (push on Vec directly)
    ($root:expr, push_back($val:expr)) => {
        $root.push($val)
    };

    // Terminal: setval on root
    ($root:expr, setval($val:expr)) => {
        $root = $val
    };
}

/// Fan out to multiple independent path navigations on the same root.
///
/// ```ignore
/// multi_nav! { state;
///     (items, set_elem(item.clone()));
///     (item_bodies, keypath(item.clone()), setval(body));
/// }
/// ```
#[macro_export]
macro_rules! multi_nav {
    ($root:ident; $(( $field:ident, $($path:tt)* ));+ $(;)?) => {
        $(
            nav!($root.$field, $($path)*);
        )+
    };
}

/// Batch insert: apply set_elem to the same map-of-sets for each value in an iterator.
///
/// ```ignore
/// nav_each!(self.item_threads, keypath(item.clone()), set_elem, touched_threads.iter().cloned());
/// ```
#[macro_export]
macro_rules! nav_each {
    ($root:expr, keypath($key:expr), set_elem, $iter:expr) => {
        for __val in $iter {
            nav!($root, keypath($key), set_elem(__val));
        }
    };
    ($root:expr, keypath($key:expr), push_front, $iter:expr) => {
        for __val in $iter {
            nav!($root, keypath($key), push_front(__val));
        }
    };
}

// ---------------------------------------------------------------------------
// Tests: real ReducerState mutations rewritten with the path DSL
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet, VecDeque};

    // -----------------------------------------------------------------------
    // Mutation 1: item_children.entry(parent).or_default().insert(child)
    //
    // Original (reducer.rs line 189-192):
    //   self.item_children.entry(parent.clone()).or_default().insert(child);
    //
    // Path version:
    //   nav!(self.item_children, keypath(parent), set_elem(child));
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_1_item_children_insert() {
        let mut item_children: HashMap<String, HashSet<String>> = HashMap::new();
        let parent = "ai".to_string();
        let child = "ai/llm".to_string();

        nav!(item_children, keypath(parent), set_elem(child.clone()));

        assert!(item_children.get("ai").unwrap().contains("ai/llm"));
    }

    // -----------------------------------------------------------------------
    // Mutation 2: self.item_bodies.insert(item.clone(), body_text)
    //
    // Original (reducer.rs line 343):
    //   self.item_bodies.insert(item.clone(), body_text);
    //
    // Path version:
    //   nav!(self.item_bodies, keypath(item), setval(body_text));
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_2_item_bodies_insert() {
        let mut item_bodies: HashMap<String, String> = HashMap::new();
        let item = "ai/llm".to_string();
        let body = "Large language models".to_string();

        nav!(item_bodies, keypath(item), setval(body));

        assert_eq!(item_bodies.get("ai/llm").unwrap(), "Large language models");
    }

    // -----------------------------------------------------------------------
    // Mutation 3: self.item_votes.entry(it.clone()).or_default().push_front(vote.clone())
    //
    // Original (reducer.rs line 385-386):
    //   let q = self.item_votes.entry(it.clone()).or_default();
    //   q.push_front(vote.clone());
    //
    // Path version:
    //   nav!(self.item_votes, keypath(it), push_front(vote.clone()));
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_3_item_votes_push_front() {
        let mut item_votes: HashMap<String, VecDeque<String>> = HashMap::new();
        let item = "ai/llm".to_string();
        let vote_id = "vote-001".to_string();

        nav!(item_votes, keypath(item), push_front(vote_id));

        assert_eq!(item_votes.get("ai/llm").unwrap().front().unwrap(), "vote-001");
    }

    // -----------------------------------------------------------------------
    // Mutation 4: self.item_snippets.entry(item.clone()).or_default().push_front(ing.id.clone())
    //
    // Original (reducer.rs line 395-396):
    //   let q = self.item_snippets.entry(item.clone()).or_default();
    //   q.push_front(ing.id.clone());
    //
    // Path version:
    //   nav!(self.item_snippets, keypath(item), push_front(ing_id));
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_4_item_snippets_push_front() {
        let mut item_snippets: HashMap<String, VecDeque<String>> = HashMap::new();
        let item = "ai/llm".to_string();
        let ingest_id = "ingest-42".to_string();

        nav!(item_snippets, keypath(item), push_front(ingest_id));

        assert_eq!(
            item_snippets.get("ai/llm").unwrap().front().unwrap(),
            "ingest-42"
        );
    }

    // -----------------------------------------------------------------------
    // Mutation 5: self.item_threads.entry(item).or_default().insert(thread)
    //
    // Original (reducer.rs line 402-405):
    //   self.item_threads
    //       .entry(item.clone())
    //       .or_default()
    //       .insert(thread.clone());
    //
    // Path version:
    //   nav!(self.item_threads, keypath(item), set_elem(thread));
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_5_item_threads_insert() {
        let mut item_threads: HashMap<String, HashSet<String>> = HashMap::new();
        let item = "ai/llm".to_string();
        let thread = "debate-1".to_string();

        nav!(item_threads, keypath(item), set_elem(thread));

        assert!(item_threads.get("ai/llm").unwrap().contains("debate-1"));
    }

    // -----------------------------------------------------------------------
    // Mutation 6: self.actor_keys.entry(actor).or_insert(key_hash)
    //
    // Original (reducer.rs line 461):
    //   self.actor_keys.entry(actor).or_insert(key_hash);
    //
    // Path version:
    //   nav!(self.actor_keys, keypath(actor), or_insert(key_hash));
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_6_actor_keys_first_write_wins() {
        let mut actor_keys: HashMap<String, String> = HashMap::new();
        let actor = "agent-1".to_string();
        let key1 = "hash-aaa".to_string();
        let key2 = "hash-bbb".to_string();

        nav!(actor_keys, keypath(actor.clone()), or_insert(key1));
        nav!(actor_keys, keypath(actor), or_insert(key2));

        // First write wins
        assert_eq!(actor_keys.get("agent-1").unwrap(), "hash-aaa");
    }

    // -----------------------------------------------------------------------
    // Mutation 7: ingests_by_scope_thread (scope, thread_tag) push_front
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_7_ingests_by_scope_thread() {
        use crate::reducer::ScopeId;
        let mut ingests_by_scope_thread: HashMap<(ScopeId, String), VecDeque<String>> = HashMap::new();
        let thread = "debate-1".to_string();
        let id1 = "ing-1".to_string();
        let id2 = "ing-2".to_string();
        let k = (ScopeId::Public, thread.clone());

        nav!(ingests_by_scope_thread, keypath(k.clone()), push_front(id1));
        nav!(ingests_by_scope_thread, keypath(k), push_front(id2));

        let q = ingests_by_scope_thread.get(&(ScopeId::Public, "debate-1".to_string())).unwrap();
        assert_eq!(q[0], "ing-2"); // most recent first
        assert_eq!(q[1], "ing-1");
    }

    // -----------------------------------------------------------------------
    // Mutation 8: conditional thread bump
    //
    // Original (reducer.rs line 411-414):
    //   let state = self.threads.entry(thread.clone()).or_default();
    //   if ing.ts > state.last_activity_ts {
    //       state.last_activity_ts = ing.ts;
    //   }
    //
    // Path version using selected:
    //   let ts = self.threads.entry(thread).or_default();
    //   nav!(ts, selected(new_ts > ts.last_activity_ts, setval(new_ts)));
    //
    // (selected! is the Specter `selected?` navigator — conditional proceed)
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_8_conditional_thread_bump() {
        #[derive(Default, Debug)]
        struct ThreadState {
            last_activity_ts: i64,
        }

        let mut threads: HashMap<String, ThreadState> = HashMap::new();
        let thread = "debate-1".to_string();

        // First bump
        let ts_entry = threads.entry(thread.clone()).or_default();
        let new_ts: i64 = 100;
        nav!(ts_entry.last_activity_ts, selected(new_ts > ts_entry.last_activity_ts, setval(new_ts)));
        assert_eq!(threads.get("debate-1").unwrap().last_activity_ts, 100);

        // No-op bump (older ts)
        let ts_entry = threads.entry(thread).or_default();
        let old_ts: i64 = 50;
        nav!(ts_entry.last_activity_ts, selected(old_ts > ts_entry.last_activity_ts, setval(old_ts)));
        assert_eq!(threads.get("debate-1").unwrap().last_activity_ts, 100);
    }

    // -----------------------------------------------------------------------
    // Mutation 9: multi_nav! for batch item registration
    //
    // Original (reducer.rs line 337-338, 376-378 — registering an item):
    //   self.items.insert(item.clone());
    //   self.item_bodies.insert(item.clone(), body_text);
    //   // + add_child_edge
    //
    // Path version (fan-out):
    //   multi_nav! { self;
    //       (items, set_elem(item.clone()));
    //       (item_bodies, keypath(item.clone()), setval(body));
    //   }
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_9_multi_path_item_registration() {
        struct State {
            items: HashSet<String>,
            item_bodies: HashMap<String, String>,
        }

        let mut state = State {
            items: HashSet::new(),
            item_bodies: HashMap::new(),
        };

        let item = "ai/llm".to_string();
        let body = "Large language models".to_string();

        multi_nav! { state;
            (items, set_elem(item.clone()));
            (item_bodies, keypath(item.clone()), setval(body))
        }

        assert!(state.items.contains("ai/llm"));
        assert_eq!(state.item_bodies.get("ai/llm").unwrap(), "Large language models");
    }

    // -----------------------------------------------------------------------
    // Mutation 10: nav_each! for item_threads batch insert
    //
    // Original (reducer.rs line 400-406):
    //   for item in ingest_items.iter() {
    //       for thread in &touched_threads {
    //           self.item_threads.entry(item.clone()).or_default().insert(thread.clone());
    //       }
    //   }
    //
    // Path version:
    //   for item in &ingest_items {
    //       nav_each!(self.item_threads, keypath(item.clone()), set_elem, touched_threads.iter().cloned());
    //   }
    // -----------------------------------------------------------------------
    #[test]
    fn mutation_10_nav_each_item_threads() {
        let mut item_threads: HashMap<String, HashSet<String>> = HashMap::new();

        let items = vec!["ai/llm".to_string(), "ai/vision".to_string()];
        let threads: HashSet<String> = ["debate-1".to_string(), "debate-2".to_string()]
            .into_iter()
            .collect();

        for item in &items {
            nav_each!(item_threads, keypath(item.clone()), set_elem, threads.iter().cloned());
        }

        assert!(item_threads.get("ai/llm").unwrap().contains("debate-1"));
        assert!(item_threads.get("ai/llm").unwrap().contains("debate-2"));
        assert!(item_threads.get("ai/vision").unwrap().contains("debate-1"));
    }

    // -----------------------------------------------------------------------
    // Side-by-side comparison: a realistic apply_event excerpt
    // -----------------------------------------------------------------------
    //
    // BEFORE (imperative):
    //
    //   self.items.insert(item_a.clone());
    //   self.items.insert(item_b.clone());
    //   self.add_child_edge(&item_a);
    //   self.add_child_edge(&item_b);
    //   for it in [&item_a, &item_b] {
    //       let q = self.item_votes.entry(it.clone()).or_default();
    //       q.push_front(vote.clone());
    //   }
    //   for item in ingest_items.iter() {
    //       let q = self.item_snippets.entry(item.clone()).or_default();
    //       q.push_front(ing.id.clone());
    //   }
    //   for item in ingest_items.iter() {
    //       for thread in &touched_threads {
    //           self.item_threads.entry(item.clone()).or_default().insert(thread.clone());
    //       }
    //   }
    //   let q = self.ingests_by_thread.entry(thread).or_default();
    //   q.push_front(ing.id.clone());
    //   self.ingests_ordered.push(ing.id.clone());
    //   self.actor_last_post_ts.insert(ing.actor.clone(), ing.ts);
    //
    // AFTER (path DSL):
    //
    //   for it in [&item_a, &item_b] {
    //       multi_nav! { self;
    //           (items, set_elem(it.clone()));
    //           (item_votes, keypath(it.clone()), push_front(vote.clone()))
    //       }
    //       self.add_child_edge(it);
    //   }
    //   for item in &ingest_items {
    //       nav!(self.item_snippets, keypath(item.clone()), push_front(ing.id.clone()));
    //       nav_each!(self.item_threads, keypath(item.clone()), set_elem, touched_threads.iter().cloned());
    //   }
    //   nav!(self.ingests_by_thread, keypath(thread), push_front(ing.id.clone()));
    //   nav!(self.ingests_ordered, push_back(ing.id.clone()));
    //   nav!(self.actor_last_post_ts, keypath(ing.actor.clone()), setval(ing.ts));
}
