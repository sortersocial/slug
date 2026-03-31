(ns test.integration
  "End-to-end integration check: DSL → CLI → HTTP → JSONL → materialized view → CLI query.
   Boots a server, ingests via CLI, queries back, kills server, restarts from
   the same JSONL, and queries again to prove replay determinism."
  (:require [babashka.process :as p]
            [clojure.string :as str]
            [babashka.fs :as fs]
            [cheshire.core :as json]
            [test.common :as common]))

;; ---------------------------------------------------------------------------
;; helpers
;; ---------------------------------------------------------------------------

(def ^:private ansi-green "\033[32m")
(def ^:private ansi-red   "\033[31m")
(def ^:private ansi-reset "\033[0m")

(def ^:private counts (atom {:pass 0 :fail 0}))

(defn- pass [msg]
  (swap! counts update :pass inc)
  (println (str ansi-green "  ✓ " ansi-reset msg)))

(defn- fail [msg]
  (swap! counts update :fail inc)
  (println (str ansi-red "  ✗ " ansi-reset msg)))

(defn- assert! [pred msg]
  (if pred
    (pass msg)
    (do (fail msg)
        (throw (ex-info (str "FAIL: " msg) {})))))

;; ---------------------------------------------------------------------------
;; letlocals unit tests
;; ---------------------------------------------------------------------------

(defn- test-letlocals []
  (println "━━━ letlocals unit tests ━━━\n")
  (assert! (= 3  (common/letlocals (bind x 1) (bind y 2) (+ x y)))
           "letlocals: bind pairs + final expr")
  (assert! (= 42 (common/letlocals (bind x 42) x))
           "letlocals: single bind returns sym")
  (assert! (= 2  (common/letlocals (bind x 1) (bind y (inc x))))
           "letlocals: last (bind sym expr) returns expr without binding sym")
  (assert! (= 10 (common/letlocals (bind x 5) (identity x) (* x 2)))
           "letlocals: bare expr for side effects, last form is return")
  (assert! (= 6  (common/letlocals (bind a 1) (bind b (+ a 2)) (bind c (+ b 3)) c))
           "letlocals: chained dependencies"))

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
             "~/languages/rust   { Systems language with ownership model }"
             "~/languages/go     { Compiled, garbage-collected, simple concurrency }"
             ""
             "~/languages/rust 3:1 ~/languages/python { Rust has stronger type safety }"
             "~/languages/rust 2:1 ~/languages/go     { Ownership beats GC for systems work }"
             "~/languages/python 2:1 ~/languages/go   { Python's ecosystem is broader }"]))

(def sorter-doc-2
  (str/join "\n"
            [actor-2
             "#integration-test"
             ""
             "~/languages/go 2:1 ~/languages/python { Go deploys as a single binary, simpler ops }"]))

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
             "~/disc/a 2:1 ~/disc/b { first component }"
             "~/disc/c 2:1 ~/disc/d { second component }"]))

;; ---------------------------------------------------------------------------
;; main
;; ---------------------------------------------------------------------------

(defn integration [& _args]
  (println "\n━━━ slug integration check ━━━\n")
  (reset! counts {:pass 0 :fail 0})
  (test-letlocals)

  (println "\nbuilding binaries…")
  (common/letlocals
    ;; 1. build
   (bind build      @(p/process [(common/cargo-bin) "build" "--release" "-p" "slugsocial-server" "-p" "slugsocial"]
                                {:inherit true :env common/base-env}))
   (assert! (zero? (:exit build)) "cargo build succeeds")

   (bind server-bin "target/release/slugsocial-server")
   (bind cli-bin    "target/release/slugsocial")
   (bind tmp-dir    (str (fs/create-temp-dir {:prefix "slug-integration-"})))
   (bind port       (common/pick-port))
   (bind base-url   (str "http://127.0.0.1:" port))
   (bind event-log  (str tmp-dir "/events.jsonl"))
   (assert! (fs/exists? server-bin) "server binary exists")
   (assert! (fs/exists? cli-bin)    "cli binary exists")

   (bind !server    (atom nil))
   (bind !server2   (atom nil))
   (bind server-env (merge common/base-env
                           {"SLUG_DATA_DIR" tmp-dir
                            "SLUG_KEYS"     "test:test"
                            "PORT"          (str port)
                            "RUST_LOG"      "warn"}))

   (try
     (common/letlocals
        ;; 2. boot server — first run, empty state
      (println (str "\nstarting server on :" port " (data: " tmp-dir ")"))
      (bind server     (common/start-server server-bin server-env))
      (reset! !server server)
      (bind server-pid (.pid (:proc server)))
      (assert! (common/wait-for-server base-url 10000) "server responds to /healthz")

        ;; 2.5 check endpoint — disconnected components not flattened
      (println "\nchecking (dry-run) returns discrete ranking groups…")
      (bind check-result (common/run-cli cli-bin base-url ["check" "--json" "--thread" "integration-test"] :input check-doc-disconnected))
      (assert! (zero? (:exit check-result)) "cli check exits 0")
      (bind check-resp   (json/parse-string (:out check-result) true))
      (assert! (:ok check-resp) "check response ok=true")
      (assert! (= 1 (count (:rankings check-resp))) "check returns one touched parent scope")
      (bind check-scope  (first (:rankings check-resp)))
      (assert! (= "https://slug.social/~/disc" (:parent check-scope)) "check scope parent is canonical ~/disc URL")
      (assert! (= 2 (count (:components check-scope))) "check preserves two disconnected components")
      (assert! (every? #(= 2 (count (:ranking %))) (:components check-scope))
               "each component ranks 2 items")
      (assert! (empty? (:unranked_items check-scope)) "no unranked items for disconnected check doc")
      (assert! (not (fs/exists? event-log)) "check does not create events.jsonl")

        ;; 3. ingest via CLI
      (println "\ningesting .sorter document via CLI…")
      (bind ingest1-result (common/run-cli cli-bin base-url ["ingest" "--json" "--thread" "integration-test"] :input sorter-doc))
      (assert! (zero? (:exit ingest1-result)) "cli ingest exits 0")
      (bind ingest1-resp   (json/parse-string (:out ingest1-result) true))
      (assert! (:ok ingest1-resp) "ingest response ok=true")
      (assert! (pos? (:events_appended ingest1-resp)) "events_appended > 0")
      (assert! (some #(= "#integration-test" %) (:threads ingest1-resp))
               "thread '#integration-test' in response")

        ;; 4. query rankings via CLI
      (println "\nquerying garden children via CLI…")
      (bind children-result (common/run-cli cli-bin base-url ["garden" "children" "languages" "--json"]))
      (assert! (zero? (:exit children-result)) "cli garden children exits 0")
      (bind children-resp   (json/parse-string (:out children-result) true))
      (bind ranked          (mapv :item (mapcat :ranking (:components children-resp))))
      (assert! (= 3 (count ranked))
               (str "3 items ranked (got " (count ranked) ")"))
      (assert! (str/ends-with? (first ranked) "languages/rust")
               (str "rust is #1 (got " (first ranked) ")"))
      (assert! (str/ends-with? (last ranked) "languages/go")
               (str "go is #3 (got " (last ranked) ")"))

        ;; 5. verify JSONL written
      (assert! (fs/exists? event-log) "events.jsonl exists")
      (bind event-lines (->> (slurp event-log) str/split-lines (remove str/blank?)))
      (assert! (= 1 (count event-lines))
               (str "1 event in JSONL (1x Ingest, got " (count event-lines) ")"))

        ;; 6. query forum via CLI
      (println "\nquerying forum via CLI…")
      (bind forum-result (common/run-cli cli-bin base-url ["forum" "--json"]))
      (assert! (zero? (:exit forum-result)) "cli forum exits 0")
      (bind forum-resp   (json/parse-string (:out forum-result) true))
      (assert! (some (fn [t] (= "#integration-test" (:thread t))) (:threads forum-resp))
               "thread '#integration-test' visible in forum")

        ;; 7. query item body via CLI
      (bind body-result (common/run-cli cli-bin base-url ["garden" "body" "languages/rust" "--json"]))
      (assert! (zero? (:exit body-result)) "cli garden body exits 0")
      (bind body-resp   (json/parse-string (:out body-result) true))
      (assert! (str/includes? (or (:body body-resp) "") "ownership")
               "rust body contains 'ownership'")

        ;; 9. global rank endpoint
      (println "\ntesting global rank endpoint…")
      (bind grank-result (common/run-cli cli-bin base-url ["garden" "rank" "--json"]))
      (assert! (zero? (:exit grank-result)) "cli garden rank exits 0")
      (bind grank-resp   (json/parse-string (:out grank-result) true))
      (assert! (pos? (:ranked_total grank-resp)) "global rank has ranked items")
      (assert! (not-empty (:items grank-resp)) "global rank returns items")
      (assert! (str/ends-with? (:item (first (:items grank-resp))) "languages/rust")
               (str "rust is #1 globally (got " (:item (first (:items grank-resp))) ")"))

      (bind grank-pct-result (common/run-cli cli-bin base-url ["garden" "rank" "--percent" "--limit" "2" "--json"]))
      (assert! (zero? (:exit grank-pct-result)) "cli garden rank --percent --limit 2 exits 0")
      (bind grank-pct-resp   (json/parse-string (:out grank-pct-result) true))
      (assert! (= 2 (count (:items grank-pct-resp))) "limit=2 returns 2 items")
      (assert! (= 100.0 (:percent (first (:items grank-pct-resp)))) "top item has 100.0% score")

      (bind grank-off-result (common/run-cli cli-bin base-url ["garden" "rank" "--limit" "1" "--offset" "1" "--json"]))
      (assert! (zero? (:exit grank-off-result)) "cli garden rank --offset 1 exits 0")
      (bind grank-off-resp   (json/parse-string (:out grank-off-result) true))
      (assert! (= (:item (second (:items grank-pct-resp))) (:item (first (:items grank-off-resp))))
               "offset=1 aligns with page 0 item index 1")

        ;; 10. rank history endpoint
      (println "\ntesting rank history endpoint…")
      (bind two-vote-doc     (str/join "\n"
                                       ["@00000000-0000-0000-0000-000000000003:integration:local/test"
                                        "#integration-test"
                                        "~/languages/rust 4:1 ~/languages/python { type safety }"
                                        "~/languages/rust 3:1 ~/languages/go { zero-cost abstractions }"]))
      (bind hist-ingest      (common/run-cli cli-bin base-url ["ingest" "--json"] :input two-vote-doc))
      (assert! (zero? (:exit hist-ingest))
               (str "two-vote ingest exits 0 (err: " (:err hist-ingest) ")"))

      (bind hist-result      (common/run-cli cli-bin base-url ["garden" "history" "languages/rust" "--json"]))
      (assert! (zero? (:exit hist-result))
               (str "cli garden history exits 0 (err: " (:err hist-result) ")"))
      (bind hist-resp        (json/parse-string (:out hist-result) true))
      (assert! (= "https://slug.social/~/languages/rust" (:item hist-resp)) "history item path is canonical ~/languages/rust URL")
      (assert! (>= (count (:history hist-resp)) 2)
               (str "rust has at least 2 history entries (got " (count (:history hist-resp)) ")"))
      (bind hist-last        (last (:history hist-resp)))
      (assert! (= 2 (count (:caused_by hist-last)))
               (str "last entry has 2 caused_by votes (got " (count (:caused_by hist-last)) ")"))
      (assert! (= 1 (:scope_rank hist-last))
               (str "rust still #1 in scope after two-vote ingest (got " (:scope_rank hist-last) ")"))

        ;; 11. kill first server, restart from same JSONL — prove replay determinism
      (println "\nkilling server (pid" server-pid ")…")
      (common/kill-server server)
      (reset! !server nil)

      (println "\nrestarting server from persisted JSONL…")
      (bind server2 (common/start-server server-bin server-env))
      (reset! !server2 server2)
      (assert! (common/wait-for-server base-url 10000) "restarted server responds to /healthz")

        ;; 12. same query, same result
      (println "\nquerying rankings after replay…")
      (bind replay-result (common/run-cli cli-bin base-url ["garden" "children" "languages" "--json"]))
      (assert! (zero? (:exit replay-result)) "cli garden children exits 0 after restart")
      (bind replay-resp   (json/parse-string (:out replay-result) true))
      (bind replay-ranked (mapv :item (mapcat :ranking (:components replay-resp))))
      (assert! (= 3 (count replay-ranked)) "3 items ranked after replay")
      (assert! (str/ends-with? (first replay-ranked) "languages/rust")
               (str "rust still #1 after replay (got " (first replay-ranked) ")")))

     (finally
       (when-some [s @!server]
         (println "\nkilling server…")
         (common/kill-server s))
       (when-some [s @!server2]
         (println "\nkilling restarted server…")
         (common/kill-server s))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass} @counts)
   (println (str "\n" ansi-green "━━━ " pass " checks passed ━━━" ansi-reset "\n"))))
