(ns test.integration
  "End-to-end integration check: DSL → CLI → HTTP → JSONL → materialized view → CLI query.
   Boots a server, ingests via CLI, queries back, kills server, restarts from
   the same JSONL, and queries again to prove replay determinism."
  (:require [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [babashka.fs :as fs]
            [cheshire.core :as json]
            [test.common :as common]
            [test.oauth :as oauth]))

;; ---------------------------------------------------------------------------
;; letlocals unit tests
;; ---------------------------------------------------------------------------

(defn- test-letlocals []
  (testing "letlocals macro"
    (is (= 3  (common/letlocals (bind x 1) (bind y 2) (+ x y)))
        "letlocals: bind pairs + final expr")
    (is (= 42 (common/letlocals (bind x 42) x))
        "letlocals: single bind returns sym")
    (is (= 2  (common/letlocals (bind x 1) (bind y (inc x))))
        "letlocals: last (bind sym expr) returns expr without binding sym")
    (is (= 10 (common/letlocals (bind x 5) (identity x) (* x 2)))
        "letlocals: bare expr for side effects, last form is return")
    (is (= 6  (common/letlocals (bind a 1) (bind b (+ a 2)) (bind c (+ b 3)) c))
        "letlocals: chained dependencies")))

;; ---------------------------------------------------------------------------
;; the .sorter documents under test
;; ---------------------------------------------------------------------------

(def actor-1 "@00000000-0000-0000-0000-000000000001:integration:local/test")
(def actor-2 "@00000000-0000-0000-0000-000000000002:integration:local/test2")

(def sorter-doc
  (str/join "\n"
            [actor-1
             "#integration-test"
             ""
             "~/languages/python { General-purpose, dynamically typed }"
             "~/languages/rust { Systems language with ownership model }"
             "~/languages/go { Compiled, garbage-collected, simple concurrency }"
             ""
             "{ Rust has stronger type safety }
~/languages/rust 3:1 ~/languages/python"
             "{ Ownership beats GC for systems work }
~/languages/rust 2:1 ~/languages/go"
             "{ Python's ecosystem is broader }
~/languages/python 2:1 ~/languages/go"]))

(def sorter-doc-2
  (str/join "\n"
            [actor-2
             "#integration-test"
             ""
             "{ Go deploys as a single binary, simpler ops }
~/languages/go 2:1 ~/languages/python"]))

(def external-sorter-doc
  (str/join "\n"
            [actor-1
             "#integration-test"
             ""
             "-/github.com/iss/1 { issue one }"
             "-/github.com/iss/2 { issue two }"
             ""
             "{ triage order }
-/github.com/iss/1 2:1 -/github.com/iss/2"]))

(def check-doc-disconnected
  (str/join "\n"
            [actor-1
             "#integration-test"
             ""
             "~/disc/a { a }"
             "~/disc/b { b }"
             "~/disc/c { c }"
             "~/disc/d { d }"
             ""
             "{ first component }
~/disc/a 2:1 ~/disc/b"
             "{ second component }
~/disc/c 2:1 ~/disc/d"]))

;; ---------------------------------------------------------------------------
;; main flow (linear integration)
;; ---------------------------------------------------------------------------

(defn integration-flow! []
  (println "\n━━━ slug integration check ━━━\n")
  (test-letlocals)

  (println "\nbuilding binaries…")
  (common/letlocals
    ;; 1. build
   (bind build (common/run-cargo-build-release! ["slugsocial-server" "slugsocial"]))
   (is (zero? (:exit build)) "cargo build succeeds")

   (bind server-bin "target/release/slugsocial-server")
   (bind cli-bin    "target/release/slugsocial")
   (bind tmp-dir    (str (fs/create-temp-dir {:prefix "slug-integration-"})))
   (bind port       (common/pick-port))
   (bind google-port (common/pick-port))
   (bind google-url (str "http://127.0.0.1:" google-port))
   (bind base-url   (str "http://127.0.0.1:" port))
   (bind event-log  (str tmp-dir "/events.jsonl"))
   (is (fs/exists? server-bin) "server binary exists")
   (is (fs/exists? cli-bin)    "cli binary exists")

   (bind !server    (atom nil))
   (bind !server2   (atom nil))
   (bind !google    (atom nil))
   (bind server-env (common/slug-server-env tmp-dir base-url google-url port))

   (try
     (common/letlocals
        ;; 2. mock Google + boot server — first run, empty state
      (println (str "\nstarting mock google on :" google-port))
      (reset! !google (oauth/start-mock-google google-port))
      (is (some? (:stop-fn @!google)) "mock google started")

      (println (str "\nstarting server on :" port " (data: " tmp-dir ")"))
      (bind server     (common/start-server server-bin server-env))
      (reset! !server server)
      (bind server-pid (.pid (:proc server)))
      (is (common/wait-for-server base-url 10000) "server responds to /healthz")

        ;; 2.5 check endpoint — disconnected components not flattened (before OAuth; no events.jsonl yet)
      (println "\nchecking (dry-run) returns discrete ranking groups…")
      (bind check-result (common/run-cli cli-bin base-url ["public" "check" "--json"] :input check-doc-disconnected))
      (is (zero? (:exit check-result)) "cli check exits 0")
      (bind check-resp   (json/parse-string (:out check-result) true))
      (is (:ok check-resp) "check response ok=true")
      (is (= 1 (count (:rankings check-resp))) "check returns one touched parent scope")
      (bind check-scope  (first (:rankings check-resp)))
      (is (= "https://slug.social/~/disc" (:parent check-scope)) "check scope parent is canonical ~/disc URL")
      (is (= 2 (count (:components check-scope))) "check preserves two disconnected components")
      (is (every? #(= 2 (count (:ranking %))) (:components check-scope))
               "each component ranks 2 items")
      (is (empty? (:unranked_items check-scope)) "no unranked items for disconnected check doc")
      (is (not (fs/exists? event-log)) "check does not create events.jsonl")

      (println "\nOAuth handoff (integration user)…")
      (bind bearer-token (oauth/fetch-bearer-token! base-url :username "intuser"))
      (is (not (str/blank? bearer-token)) "bearer token from OAuth")
      (bind token-env {"SLUG_BEARER_TOKEN" bearer-token})

        ;; 2.6 private room: CLI read RPCs must send bearer (regression — missing header looked like "room not found")
      (println "\nprivate room: create → post thread → forum show via CLI…")
      (bind private-slug "cli-private-read-regression")
      (bind create-room-result (common/run-cli cli-bin base-url ["room" "create" private-slug "--json"] :extra-env token-env))
      (is (zero? (:exit create-room-result))
               (str "cli room create exits 0 (err: " (:err create-room-result) ")"))
      (bind create-room-json (json/parse-string (:out create-room-result) true))
      (bind private-room-id (:room_id create-room-json))
      (is (and (string? private-room-id) (str/includes? private-room-id "/"))
               (str "room create returns shortid/slug room_id (got " (pr-str private-room-id) ")"))
      (bind private-thread "private-cli-thread")
      (bind private-marker "private room prose regression marker")
      (bind private-doc
            (str/join "\n"
                      ["@00000000-0000-0000-0000-000000000000:cli:local/dev"
                       (str "#" private-thread)
                       ""
                       private-marker]))
      (bind private-post-result
            (common/run-cli cli-bin base-url
                            ["private" private-room-id "forum" "post" private-thread "--json"
                             "--delegate" "00000000-0000-0000-0000-000000000000:cli:local/dev"]
                            :input private-doc :extra-env token-env))
      (is (zero? (:exit private-post-result))
               (str "private forum post exits 0 (err: " (:err private-post-result) ")"))
      (bind private-post-json (json/parse-string (:out private-post-result) true))
      (is (:ok private-post-json) "private forum post ok=true")

      (bind private-needs-bearer-hint "needs bearer token, use slugsocial identity command")
      (bind show-no-token (common/run-cli cli-bin base-url
                                         ["private" private-room-id "forum" "show" private-thread "--json"]))
      (is (not (zero? (:exit show-no-token)))
               "private forum show without SLUG_BEARER_TOKEN must fail (server hides private rooms without auth)")
      (bind show-no-token-combined (str (:out show-no-token) (:err show-no-token)))
      (is (not (str/blank? show-no-token-combined))
               "private forum show without token must not be completely silent (stdout+stderr)")
      (is (str/includes? show-no-token-combined private-needs-bearer-hint)
               "private forum show without token must mention identity / bearer command")

      (bind show-bad-token (common/run-cli cli-bin base-url
                                          ["private" private-room-id "forum" "show" private-thread "--json"]
                                          :extra-env {"SLUG_BEARER_TOKEN" "not-a-valid-slug-token"}))
      (is (not (zero? (:exit show-bad-token)))
               "private forum show with invalid bearer must fail (server returns room not found)")
      (bind show-bad-combined (str (:out show-bad-token) (:err show-bad-token)))
      (is (str/includes? show-bad-combined private-needs-bearer-hint)
               "private forum show with bad token must map room-not-found to bearer hint, not empty output")

      (bind show-with-token (common/run-cli cli-bin base-url
                                            ["private" private-room-id "forum" "show" private-thread "--json"]
                                            :extra-env token-env))
      (is (zero? (:exit show-with-token))
               (str "private forum show with bearer exits 0 (err: " (:err show-with-token) ")"))
      (bind show-json (json/parse-string (:out show-with-token) true))
      ;; ThreadItem JSON uses serde snake_case tag values: {:kind "post" :body ...}
      (is (some (fn [row]
                       (and (= "post" (:kind row))
                            (str/includes? (str (:body row)) private-marker)))
                     (:items show-json))
               "private forum show --json includes posted prose body")

        ;; 3. ingest via CLI (bearer required)
      (println "\ningesting .sorter document via CLI…")
      (bind ingest1-result (common/run-cli cli-bin base-url ["public" "forum" "post" "integration-test" "--json" "--delegate" "00000000-0000-0000-0000-000000000000:cli:local/dev"] :input sorter-doc :extra-env token-env))
      (is (zero? (:exit ingest1-result)) "cli ingest exits 0")
      (bind ingest1-resp   (json/parse-string (:out ingest1-result) true))
      (is (:ok ingest1-resp) "ingest response ok=true")
      (is (pos? (:events_appended ingest1-resp)) "events_appended > 0")
      (is (some #(= "#integration-test" %) (:threads ingest1-resp))
               "thread '#integration-test' in response")

        ;; 4. query rankings via CLI
      (println "\nquerying garden children via CLI…")
      (bind children-result (common/run-cli cli-bin base-url ["public" "garden" "children" "--json" "--" "languages"]))
      (is (zero? (:exit children-result)) "cli garden children exits 0")
      (bind children-resp   (json/parse-string (:out children-result) true))
      (bind ranked          (mapv :item (mapcat :ranking (:components children-resp))))
      (is (= 3 (count ranked))
               (str "3 items ranked (got " (count ranked) ")"))
      (is (str/ends-with? (first ranked) "/rust")
               (str "rust is #1 as leaf ~/rust (got " (first ranked) ")"))
      (is (not (str/includes? (first ranked) "languages/rust"))
               "ranked item identity is the leaf, not the old nested path")
      (is (str/ends-with? (last ranked) "/go")
          (str "go is #3 as leaf ~/go (got " (last ranked) ")"))

      (println "\nchecking agent-facing missing path error…")
      (bind missing-path-result
            (common/run-cli cli-bin base-url
                            ["public" "garden" "children" "--" "does-not-exist"]
                            :extra-env {"RUST_BACKTRACE" "1"}))
      (is (not (zero? (:exit missing-path-result)))
          "missing garden path exits nonzero")
      (bind missing-path-output
            (str (:out missing-path-result) (:err missing-path-result)))
      (is (str/includes? missing-path-output "path not found")
          "missing garden path reports the concise error")
      (is (str/includes? missing-path-output "~/does-not-exist does not exist")
          "missing garden path preserves the actionable hint")
      (is (not (str/includes? missing-path-output "Stack backtrace:"))
          "missing garden path does not expose an anyhow backtrace")

        ;; 5. verify JSONL written (user_registered + token_issued + room_created + grant_added
        ;;    + private ingest + agent_bound + public ingest)
      (is (fs/exists? event-log) "events.jsonl exists")
      (bind event-lines (->> (slurp event-log) str/split-lines (remove str/blank?)))
      (is (= 7 (count event-lines))
               (str "7 events in JSONL (incl. private room + first public ingest, got " (count event-lines) ")"))

      (println "\ningesting external -/ namespace .sorter via CLI…")
      (bind ext-ingest-result (common/run-cli cli-bin base-url ["public" "forum" "post" "integration-test" "--json" "--delegate" "00000000-0000-0000-0000-000000000000:cli:local/dev"] :input external-sorter-doc :extra-env token-env))
      (is (zero? (:exit ext-ingest-result)) "external namespace cli ingest exits 0")
      (bind ext-ingest-resp (json/parse-string (:out ext-ingest-result) true))
      (is (:ok ext-ingest-resp) "external ingest ok=true")

      (println "\nquerying garden children for external parent via CLI…")
      (bind ext-children-result (common/run-cli cli-bin base-url ["public" "garden" "children" "--json" "--" "-/github.com/iss"]))
      (is (zero? (:exit ext-children-result)) "cli garden children -/github.com/iss exits 0")
      (bind ext-children-resp (json/parse-string (:out ext-children-result) true))
      (bind ext-ranked (mapv :item (mapcat :ranking (:components ext-children-resp))))
      (is (= 2 (count ext-ranked)) (str "2 external issues ranked (got " (count ext-ranked) ")"))
      (is (str/ends-with? (first ext-ranked) "github.com/iss/1")
               (str "iss/1 is #1 (got " (first ext-ranked) ")"))

      (println "\nquerying complete garden tree via CLI…")
      (bind tree-result (common/run-cli cli-bin base-url ["public" "garden" "tree"]))
      (is (zero? (:exit tree-result))
          (str "cli garden tree exits 0 (err: " (:err tree-result) ")"))
      (bind tree-paths (->> (:out tree-result) str/split-lines (remove str/blank?) set))
      (is (contains? tree-paths "~/rust")
          "tree renders slug.social leaves as clean ~/leaf paths")
      (is (not (contains? tree-paths "~/languages/rust"))
          "tree does not keep the pre-containment nested path")
      (is (contains? tree-paths "-/https://github.com/iss/1")
          "tree renders external leaves with the -/ URL form")
      (is (not-any? #(str/starts-with? % "~/http") tree-paths)
          "tree never adds ~/ in front of absolute URLs")

      (bind event-lines-after-ext (->> (slurp event-log) str/split-lines (remove str/blank?)))
      (is (= 8 (count event-lines-after-ext))
               (str "8 events in JSONL after external ingest (got " (count event-lines-after-ext) ")"))

        ;; 6. query forum via CLI
      (println "\nquerying forum via CLI…")
      (bind forum-result (common/run-cli cli-bin base-url ["public" "forum" "list" "--json"]))
      (is (zero? (:exit forum-result)) "cli forum exits 0")
      (bind forum-resp   (json/parse-string (:out forum-result) true))
      (is (some (fn [t] (= "#integration-test" (:thread t))) (:threads forum-resp))
               "thread '#integration-test' visible in forum")

        ;; 7. query item body via CLI
      (bind body-result (common/run-cli cli-bin base-url ["public" "garden" "body" "languages/rust" "--json"]))
      (is (zero? (:exit body-result)) "cli garden body exits 0")
      (bind body-resp   (json/parse-string (:out body-result) true))
      (is (str/includes? (or (:body body-resp) "") "ownership")
               "rust body contains 'ownership'")

        ;; 9. global rank endpoint
      (println "\ntesting global rank endpoint…")
      (bind grank-result (common/run-cli cli-bin base-url ["public" "garden" "rank" "--json"]))
      (is (zero? (:exit grank-result)) "cli garden rank exits 0")
      (bind grank-resp   (json/parse-string (:out grank-result) true))
      (is (pos? (:ranked_total grank-resp)) "global rank has ranked items")
      (bind grank-items (vec (mapcat :ranking (:components grank-resp))))
      (is (not-empty grank-items) "global rank returns ranked components")
      (is (str/ends-with? (:item (first grank-items)) "/rust")
               (str "rust is first in its component as leaf (got " (:item (first grank-items)) ")"))

      (bind grank-pct-result (common/run-cli cli-bin base-url ["public" "garden" "rank" "--percent" "--limit" "2" "--json"]))
      (is (zero? (:exit grank-pct-result)) "cli garden rank --percent --limit 2 exits 0")
      (bind grank-pct-resp   (json/parse-string (:out grank-pct-result) true))
      (bind grank-pct-items (vec (mapcat :ranking (:components grank-pct-resp))))
      (is (= 2 (count grank-pct-items)) "limit=2 returns 2 items")
      (is (= 100.0 (:percent (first grank-pct-items))) "top item has 100.0% score")

      (bind grank-off-result (common/run-cli cli-bin base-url ["public" "garden" "rank" "--limit" "1" "--offset" "1" "--json"]))
      (is (zero? (:exit grank-off-result)) "cli garden rank --offset 1 exits 0")
      (bind grank-off-resp   (json/parse-string (:out grank-off-result) true))
      (bind grank-off-items (vec (mapcat :ranking (:components grank-off-resp))))
      (is (= (:item (second grank-pct-items)) (:item (first grank-off-items)))
               "offset=1 aligns with page 0 item index 1")

        ;; 10. rank history endpoint
      (println "\ntesting rank history endpoint…")
      (bind two-vote-doc     (str/join "\n"
                                       ["@00000000-0000-0000-0000-000000000003:integration:local/test"
                                        "#integration-test"
                                        "{ type safety }
~/languages/rust 4:1 ~/languages/python"
                                        "{ zero-cost abstractions }
~/languages/rust 3:1 ~/languages/go"]))
      (bind hist-ingest      (common/run-cli cli-bin base-url ["public" "forum" "post" "integration-test" "--json" "--delegate" "00000000-0000-0000-0000-000000000000:cli:local/dev"] :input two-vote-doc :extra-env token-env))
      (is (zero? (:exit hist-ingest))
               (str "two-vote ingest exits 0 (err: " (:err hist-ingest) ")"))

      (bind event-lines-after-hist (->> (slurp event-log) str/split-lines (remove str/blank?)))
      (is (= 9 (count event-lines-after-hist))
               (str "9 events in JSONL after second public ingest (got " (count event-lines-after-hist) ")"))

      (bind hist-result      (common/run-cli cli-bin base-url ["public" "garden" "history" "languages/rust" "--json"]))
      (is (zero? (:exit hist-result))
               (str "cli garden history exits 0 (err: " (:err hist-result) ")"))
      (bind hist-resp        (json/parse-string (:out hist-result) true))
      (is (= "https://slug.social/~/rust" (:item hist-resp))
          "history item path is the leaf URL (nested languages/rust query still resolves)")
      (is (>= (count (:history hist-resp)) 2)
               (str "rust has at least 2 history entries (got " (count (:history hist-resp)) ")"))
      (bind hist-last        (last (:history hist-resp)))
      (is (= 2 (count (:caused_by hist-last)))
               (str "last entry has 2 caused_by votes (got " (count (:caused_by hist-last)) ")"))
      (is (= 1 (:scope_rank hist-last))
               (str "rust still #1 in scope after two-vote ingest (got " (:scope_rank hist-last) ")"))

        ;; 11. kill first server, restart from same JSONL — prove replay determinism
      (println "\nkilling server (pid" server-pid ")…")
      (common/kill-server server)
      (reset! !server nil)

      (println "\nrestarting server from persisted JSONL…")
      (bind server2 (common/start-server server-bin server-env))
      (reset! !server2 server2)
      (is (common/wait-for-server base-url 10000) "restarted server responds to /healthz")

        ;; 12. same query, same result
      (println "\nquerying rankings after replay…")
      (bind replay-result (common/run-cli cli-bin base-url ["public" "garden" "children" "--json" "--" "languages"]))
      (is (zero? (:exit replay-result)) "cli garden children exits 0 after restart")
      (bind replay-resp   (json/parse-string (:out replay-result) true))
      (bind replay-ranked (mapv :item (mapcat :ranking (:components replay-resp))))
      (is (= 3 (count replay-ranked)) "3 items ranked after replay")
      (is (str/ends-with? (first replay-ranked) "/rust")
               (str "rust still #1 leaf after replay (got " (first replay-ranked) ")")))

     (finally
       (when-some [s @!server]
         (println "\nkilling server…")
         (common/kill-server s))
       (when-some [s @!server2]
         (println "\nkilling restarted server…")
         (common/kill-server s))
       (when-some [g @!google]
         (println "\nstopping mock google…")
         ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn integration [& _args]
  (integration-flow!))

(deftest slug-integration-check
  (integration-flow!))
