#!/usr/bin/env bb
;;
;; bb sanity — full round-trip integration test
;;
;; Tests:
;;   1.  cargo build --release (both crates)
;;   1b. cargo test --workspace (unit + integration)
;;   1c. cargo clippy (lint gate, -D warnings)
;;   2.  server boot on a random free port with a fresh temp dir
;;   3.  CLI ingest of a .sorter document (actor + thread + 3 items + 3 votes)
;;   4.  garden children → assert rust #1, go #3
;;   4b. garden tree   → items visible
;;   4c. garden rank   → global ranking returns components
;;   4d. garden pair   → suggest a pair under languages
;;   4e. garden matchup → vote history for rust
;;   5.  events.jsonl written with exactly 1 event
;;   6.  forum listing → assert thread is listed
;;   6b. forum detail  → thread posts visible
;;   6c. check (dry-run) → ok without persisting
;;   7.  garden body → assert rust body contains "ownership"
;;   8.  kill server, restart from same data dir (JSONL replay)
;;   9.  same garden children query → same rankings
;;  10.  cleanup temp dir

(ns scripts.sanity
  (:require [babashka.process :as p]
            [cheshire.core :as json]
            [clojure.string :as str]))

;; ---------------------------------------------------------------------------
;; Output helpers
;; ---------------------------------------------------------------------------

(defn- green [s] (str "\u001b[32m  \u2713 \u001b[0m" s))
(defn- red   [s] (str "\u001b[31m  \u2717 \u001b[0m" s))
(defn- pass  [msg] (println (green msg)))
(defn- fail  [msg] (println (red   msg)))
(ns scripts.sanity
  "End-to-end sanity check: DSL → CLI → HTTP → JSONL → materialized view → CLI query.
   Boots a server, ingests via CLI, queries back, kills server, restarts from
   the same JSONL, and queries again to prove replay determinism."
  (:require [babashka.process :as p]
            [clojure.string :as str]
            [babashka.fs :as fs]
            [cheshire.core :as json]))

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
;; Port picker
;; ---------------------------------------------------------------------------

(defn- pick-port
  "Find a free port by binding :0 then releasing."
  []
  (let [ss (java.net.ServerSocket. 0)
        port (.getLocalPort ss)]
    (.close ss)
    port))

;; ---------------------------------------------------------------------------
;; base-env — must be defined BEFORE run-cli
;;
;; p/process resolves binary names against the *parent* PATH (JVM exec),
;; not the :env map we pass in. So we prepend ~/.cargo/bin here so that
;; later p/process calls using the full cargo path work, and also so
;; subprocesses (the server binary etc.) inherit a sane environment.
;; ---------------------------------------------------------------------------

(def base-env
  (let [env       (into {} (System/getenv))
        home      (System/getProperty "user.home")
        cargo-dir (str home "/.cargo/bin")
        path      (get env "PATH" "")]
    (if (.isDirectory (java.io.File. cargo-dir))
      (assoc env "PATH" (str cargo-dir ":" path))
      env)))

(def cargo-bin
  (let [rustup-cargo (str (System/getProperty "user.home") "/.cargo/bin/cargo")]
    (if (.exists (java.io.File. rustup-cargo))
      rustup-cargo
      "cargo")))

;; ---------------------------------------------------------------------------
;; CLI runner
;; ---------------------------------------------------------------------------

(defn- run-cli
  "Invoke the CLI binary with args. Returns the deref'd process map
   {:exit :out :err}. Optionally pipes input string to stdin."
  [binary base-url args & {:keys [input]}]
  (let [proc (p/process (into [binary] args)
                        (cond-> {:out :string
                                 :err :string
                                 :env (merge base-env {"SLUG_SERVER" base-url})}
(defn- pick-port
  "Find a free port by binding :0 then closing."
  []
  (let [ss (java.net.ServerSocket. 0)]
    (try (.getLocalPort ss) (finally (.close ss)))))

(defn- wait-for-server
  "Poll /healthz until it returns 'ok', up to `timeout-ms`."
  [base-url timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [ok? (try
                  (= "ok" (str/trim (slurp (str base-url "/healthz"))))
                  (catch Exception _ false))]
        (if ok?
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(def ^:private cargo-home (str (System/getProperty "user.home") "/.cargo/bin"))

(def ^:private base-env
  (let [env  (into {} (System/getenv))
        path (get env "PATH" "")]
    (if (str/includes? path cargo-home)
      env
      (assoc env "PATH" (str cargo-home ":" path)))))

(defn- cargo-bin
  "Resolve cargo, preferring ~/.cargo/bin if not on PATH."
  []
  (let [local (str cargo-home "/cargo")]
    (if (fs/exists? local) local "cargo")))

(defn- run-cli
  "Run the slugsocial CLI binary with args, return {:exit :out :err}."
  [binary base-url args & {:keys [input]}]
  (let [proc (p/process (into [binary] args)
                        (cond-> {:out :string :err :string
                                 :env (merge base-env
                                             {"SLUG_SERVER" base-url})}
                          input (assoc :in input)))]
    @proc))

;; ---------------------------------------------------------------------------
;; Server readiness poll
;; ---------------------------------------------------------------------------

(defn- wait-for-server
  "Poll GET /healthz until it responds 200 or timeout-ms elapses."
  [base-url timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [ok (try (slurp (str base-url "/healthz")) true
                    (catch Exception _ false))]
        (if ok
          true
          (if (> (System/currentTimeMillis) deadline)
            false
            (do (Thread/sleep 200)
                (recur))))))))

;; ---------------------------------------------------------------------------
;; Test document
;;
;; Deterministic triangle: rust beats python and go, python beats go.
;; Therefore: rust #1, python #2, go #3.
;; ---------------------------------------------------------------------------

(def test-doc
  "@00000000-0000-4000-a000-000000000001:sanity:test/model

#sanity-test
~/languages/rust { Rust features memory safety through ownership. }
~/languages/python { Python is dynamic and interpreted. }
~/languages/go { Go is statically typed and compiled. }
~/languages/rust 3:1 ~/languages/python { ownership and zero-cost abstractions beat duck typing }
~/languages/rust 3:1 ~/languages/go { borrow checker catches bugs that gc cannot }
~/languages/python 3:1 ~/languages/go { expressiveness and ecosystem breadth }
")

;; ---------------------------------------------------------------------------
;; Main
;; ---------------------------------------------------------------------------

(defn sanity []
  (println "\n─── slug sanity check ───\n")

  ;; 1. build
  (println "building binaries…")
  (let [build @(p/process [cargo-bin "build" "--release"
                            "-p" "slugsocial-server"
                            "-p" "slugsocial"]
                           {:inherit true :env base-env})]
    (assert! (zero? (:exit build)) "cargo build succeeds"))

  ;; 1b. run unit + integration tests
  (println "\nrunning cargo test…")
  (let [test-run @(p/process [cargo-bin "test" "--workspace"]
                              {:inherit true :env base-env})]
    (assert! (zero? (:exit test-run)) "cargo test passes"))

  ;; 1c. lint gate
  (println "\nrunning cargo clippy…")
  (let [clippy @(p/process [cargo-bin "clippy" "--workspace" "--" "-D" "warnings"]
                            {:inherit true :env base-env})]
    (assert! (zero? (:exit clippy)) "cargo clippy clean"))

  (let [server-bin "target/release/slugsocial-server"
        cli-bin    "target/release/slugsocial"]

    (assert! (.exists (java.io.File. server-bin)) "server binary exists")
    (assert! (.exists (java.io.File. cli-bin))    "cli binary exists")

    ;; 2. start server
    (let [port     (pick-port)
          data-dir (str "/tmp/slug-sanity-" (System/nanoTime))]
      (.mkdirs (java.io.File. data-dir))
      (println (str "\nstarting server on :" port " (data: " data-dir ")"))

      (let [server  (p/process [server-bin]
                               {:env (merge base-env
                                            {"SLUG_DATA_DIR" data-dir
                                             "SLUG_KEYS"     "dev:dev"
                                             "PORT"          (str port)
                                             "RUST_LOG"      "info"})})
            base-url (str "http://127.0.0.1:" port)]

        ;; 3. wait for server ready
        (assert! (wait-for-server base-url 10000) "server responds to /healthz")

        ;; 4. ingest
        (println "\ningesting .sorter document via CLI…")
        (let [ingest (run-cli cli-bin base-url ["ingest" "--json"] :input test-doc)]
          (assert! (zero? (:exit ingest)) "cli ingest exits 0")
          (let [resp (json/parse-string (:out ingest) true)]
            (assert! (:ok resp)                              "ingest response ok=true")
            (assert! (pos? (:events_appended resp))          "events_appended > 0")
            (assert! (some #(= "#sanity-test" %) (:threads resp))
                     "thread '#sanity-test' in response")))

        ;; 5. garden children
        (println "\nquerying garden children via CLI…")
        (let [children (run-cli cli-bin base-url ["garden" "children" "languages" "--json"])]
          (assert! (zero? (:exit children)) "cli garden children exits 0")
          (let [resp       (json/parse-string (:out children) true)
                ranking    (get-in resp [:components 0 :ranking])
                n          (count ranking)
                first-item (:item (first ranking))
                last-item  (:item (last ranking))]
            (assert! (= 3 n)                  (str "3 items ranked (got " n ")"))
            (assert! (= "/languages/rust" first-item) (str "rust is #1 (got " first-item ")"))
            (assert! (= "/languages/go"  last-item)   (str "go is #3 (got " last-item ")"))))

        ;; 5b. garden tree
        (println "\nquerying garden tree via CLI…")
        (let [tree (run-cli cli-bin base-url ["garden" "tree" "--json"])]
          (assert! (zero? (:exit tree)) "cli garden tree exits 0")
          (let [resp (json/parse-string (:out tree) true)]
            (assert! (seq (:paths resp))                    "tree returns paths")
            (assert! (some #(str/includes? % "languages/rust") (:paths resp))
                     "tree includes languages/rust")))

        ;; 5c. garden rank (global)
        (let [rank-r (run-cli cli-bin base-url ["garden" "rank" "--json"])]
          (assert! (zero? (:exit rank-r)) "cli garden rank exits 0")
          (let [resp (json/parse-string (:out rank-r) true)]
            (assert! (seq (:components resp)) "rank returns components")))

        ;; 5d. garden pair
        (let [pair-r (run-cli cli-bin base-url ["garden" "pair" "languages" "--json"])]
          (assert! (zero? (:exit pair-r)) "cli garden pair exits 0")
          (let [resp (json/parse-string (:out pair-r) true)]
            (assert! (some? (:left resp))  "pair has left item")
            (assert! (some? (:right resp)) "pair has right item")))

        ;; 5e. garden matchup
        (let [matchup-r (run-cli cli-bin base-url ["garden" "matchup" "languages/rust" "--json"])]
          (assert! (zero? (:exit matchup-r)) "cli garden matchup exits 0")
          (let [resp (json/parse-string (:out matchup-r) true)]
            (assert! (= "/languages/rust" (:item resp)) "matchup item is /languages/rust")
            (assert! (seq (:votes resp))                "matchup has votes")))

        ;; 6. verify JSONL
        (let [jsonl (str data-dir "/events.jsonl")]
          (assert! (.exists (java.io.File. jsonl)) "events.jsonl exists")
          (let [lines (count (str/split-lines (slurp jsonl)))]
            (assert! (= 1 lines) (str "1 event in JSONL (got " lines ")"))))

        ;; 7. forum listing
        (println "\nquerying forum via CLI…")
        (let [forum (run-cli cli-bin base-url ["forum" "--json"])]
          (assert! (zero? (:exit forum)) "cli forum exits 0")
          (let [resp  (json/parse-string (:out forum) true)
                names (map :thread (:threads resp))]
            (assert! (some #(= "#sanity-test" %) names) "thread '#sanity-test' visible in forum")))

        ;; 7b. forum thread detail
        (let [detail (run-cli cli-bin base-url ["forum" "sanity-test" "--json"])]
          (assert! (zero? (:exit detail)) "cli forum thread detail exits 0")
          (let [resp (json/parse-string (:out detail) true)]
            (assert! (seq (:posts resp)) "thread has posts")))

        ;; 7c. check (dry-run — must not persist)
        (println "\ntesting check (dry-run) via CLI…")
        (let [chk (run-cli cli-bin base-url ["check" "--json"] :input test-doc)]
          (assert! (zero? (:exit chk)) "cli check exits 0")
          (let [resp (json/parse-string (:out chk) true)]
            (assert! (:ok resp) "check response ok=true")))

        ;; 8. item body
        (let [body-r (run-cli cli-bin base-url ["garden" "body" "languages/rust" "--json"])]
          (assert! (zero? (:exit body-r)) "cli garden body exits 0")
          (let [resp (json/parse-string (:out body-r) true)]
            (assert! (str/includes? (or (:body resp) "") "ownership")
                     "rust body contains 'ownership'")))

        ;; 9. kill server
        (println (str "\nkilling server (pid " (-> server :proc .pid) ")…"))
        (-> server :proc .destroy)
        (Thread/sleep 500)

        ;; 10. restart from persisted JSONL
        (println "\nrestarting server from persisted JSONL…")
        (let [server2 (p/process [server-bin]
                                 {:env (merge base-env
                                              {"SLUG_DATA_DIR" data-dir
                                               "SLUG_KEYS"     "dev:dev"
                                               "PORT"          (str port)
                                               "RUST_LOG"      "info"})})]
          (assert! (wait-for-server base-url 10000) "restarted server responds to /healthz")

          ;; 11. replay rankings
          (println "\nquerying rankings after replay…")
          (let [children (run-cli cli-bin base-url ["garden" "children" "languages" "--json"])]
            (assert! (zero? (:exit children)) "cli garden children exits 0 after restart")
            (let [resp       (json/parse-string (:out children) true)
                  ranking    (get-in resp [:components 0 :ranking])
                  n          (count ranking)
                  first-item (:item (first ranking))
                  last-item  (:item (last ranking))]
              (assert! (= 3 n)                  "3 items ranked after replay")
              (assert! (= "/languages/rust" first-item) (str "rust still #1 after replay (got " first-item ")"))
              (assert! (= "/languages/go"  last-item)   (str "go still #3 after replay (got " last-item ")"))))

          (-> server2 :proc .destroy)
          (Thread/sleep 200))

        ;; 12. cleanup
        (doseq [f (reverse (file-seq (java.io.File. data-dir)))]
          (.delete f))))

  (println "\n\u001b[32m─── all checks passed ───\u001b[0m\n")))
;; letlocals — ML-style sequential let with explicit bind and side-effect forms
;;
;;   (letlocals
;;     (bind x 1)       ; (bind sym expr) → [sym expr] in let bindings
;;     (println x)      ; bare expr      → [_ expr]    (side effect, result discarded)
;;     (bind y (+ x 1)) ; can reference earlier bindings
;;     (+ x y))         ; last form is the return expression
;;
;; If the last form is (bind sym expr), only expr is returned (sym unbound),
;; matching the clj-kondo hook semantics.
;; ---------------------------------------------------------------------------

(defmacro letlocals [& body]
  (let [all-but-last  (butlast body)
        last-item     (last body)
        last-binding? (and (seq? last-item) (= 'bind (first last-item)))
        last-expr     (if last-binding? (last last-item) last-item)
        bindings      (vec (mapcat (fn [item]
                                     (if (and (seq? item) (= 'bind (first item)))
                                       [(second item) (nth item 2)]
                                       ['_ item]))
                                   all-but-last))]
    `(let ~bindings ~last-expr)))

;; ---------------------------------------------------------------------------
;; letlocals unit tests
;; ---------------------------------------------------------------------------

(defn- test-letlocals []
  (println "━━━ letlocals unit tests ━━━\n")
  (assert! (= 3  (letlocals (bind x 1) (bind y 2) (+ x y)))
           "letlocals: bind pairs + final expr")
  (assert! (= 42 (letlocals (bind x 42) x))
           "letlocals: single bind returns sym")
  (assert! (= 2  (letlocals (bind x 1) (bind y (inc x))))
           "letlocals: last (bind sym expr) returns expr without binding sym")
  (assert! (= 10 (letlocals (bind x 5) (identity x) (* x 2)))
           "letlocals: bare expr for side effects, last form is return")
  (assert! (= 6  (letlocals (bind a 1) (bind b (+ a 2)) (bind c (+ b 3)) c))
           "letlocals: chained dependencies"))

;; ---------------------------------------------------------------------------
;; the .sorter documents under test
;; ---------------------------------------------------------------------------

(def actor-1 "@00000000-0000-0000-0000-000000000001:sanity:local/test")
(def actor-2 "@00000000-0000-0000-0000-000000000002:sanity:local/test2")

(def sorter-doc
  (str/join "\n"
    [actor-1
     "#sanity-test"
     ""
     "~/languages/python { General-purpose, dynamically typed }"
     "~/languages/rust   { Systems language with ownership model }"
     "~/languages/go     { Compiled, garbage-collected, simple concurrency }"
     ""
     "~/languages/rust 3:1 ~/languages/python { Rust has stronger type safety }"
     "~/languages/rust 2:1 ~/languages/go     { Ownership beats GC for systems work }"
     "~/languages/python 2:1 ~/languages/go   { Python's ecosystem is broader }"]))

;; A second actor votes on the same items — this generates a notification for actor-1.
(def sorter-doc-2
  (str/join "\n"
    [actor-2
     "#sanity-test"
     ""
     "~/languages/go 2:1 ~/languages/python { Go deploys as a single binary, simpler ops }"]))

;; A check-only document that creates TWO disconnected components under one parent scope.
(def check-doc-disconnected
  (str/join "\n"
    [actor-1
     "#sanity-test"
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

(defn sanity [& _args]
  (println "\n━━━ slug sanity check ━━━\n")
  (reset! counts {:pass 0 :fail 0})

  (test-letlocals)

  ;; 1. build
  (println "\nbuilding binaries…")
  (letlocals
    (bind build @(p/process [(cargo-bin) "build" "--release" "-p" "slugsocial-server" "-p" "slugsocial"]
                            {:inherit true :env base-env}))
    (assert! (zero? (:exit build)) "cargo build succeeds"))

  (letlocals
    (bind server-bin "target/release/slugsocial-server")
    (bind cli-bin    "target/release/slugsocial")
    (bind tmp-dir    (str (fs/create-temp-dir {:prefix "slug-sanity-"})))
    (bind port       (pick-port))
    (bind base-url   (str "http://127.0.0.1:" port))
    (bind event-log  (str tmp-dir "/events.jsonl"))
    (assert! (fs/exists? server-bin) "server binary exists")
    (assert! (fs/exists? cli-bin)    "cli binary exists")

    (try
      ;; 2. boot server — first run, empty state
      (println (str "\nstarting server on :" port " (data: " tmp-dir ")"))
      (letlocals
        (bind server     (p/process [server-bin]
                                    {:out :inherit :err :inherit
                                     :env (merge base-env
                                                 {"SLUG_DATA_DIR" tmp-dir
                                                  "SLUG_KEYS"     "test:test"
                                                  "PORT"          (str port)
                                                  "RUST_LOG"      "warn"})}))
        (bind server-pid (.pid (:proc server)))
        (try
          (assert! (wait-for-server base-url 10000) "server responds to /healthz")

          ;; 2.5 check endpoint — disconnected components not flattened
          (println "\nchecking (dry-run) returns discrete ranking groups…")
          (letlocals
            (bind result (run-cli cli-bin base-url ["check" "--json"] :input check-doc-disconnected))
            (assert! (zero? (:exit result)) "cli check exits 0")
            (bind resp   (json/parse-string (:out result) true))
            (assert! (:ok resp) "check response ok=true")
            (assert! (= 1 (count (:rankings resp))) "check returns one touched parent scope")
            (bind scope  (first (:rankings resp)))
            (assert! (= "/disc" (:parent scope)) "check scope parent is /disc")
            (assert! (= 2 (count (:components scope))) "check preserves two disconnected components")
            (assert! (every? #(= 2 (count (:ranking %))) (:components scope))
                     "each component ranks 2 items")
            (assert! (empty? (:unranked_items scope)) "no unranked items for disconnected check doc"))
          (assert! (not (fs/exists? event-log)) "check does not create events.jsonl")

          ;; 3. ingest via CLI
          (println "\ningesting .sorter document via CLI…")
          (letlocals
            (bind result         (run-cli cli-bin base-url ["ingest" "--json"] :input sorter-doc))
            (assert! (zero? (:exit result)) "cli ingest exits 0")
            (bind resp           (json/parse-string (:out result) true))
            (bind actor1-passkey (:passkey resp))
            (assert! (:ok resp) "ingest response ok=true")
            (assert! (pos? (:events_appended resp)) "events_appended > 0")
            (assert! (some #(= "#sanity-test" %) (:threads resp))
                     "thread '#sanity-test' in response")
            (assert! (some? actor1-passkey) "first ingest returns a passkey")

            ;; private namespace tests
            (println "\ntesting private diary namespaces…")
            (bind actor1-uuid  "00000000-0000-0000-0000-000000000001")
            (bind private-path (str actor1-uuid "/private-note"))
            (bind private-doc  (str/join "\n"
                                  [actor-1 ""
                                   (str "~/" private-path " { secret diary content }")]))

            (bind ingest-priv  (run-cli cli-bin base-url
                                        ["ingest" "--json" "--passkey" actor1-passkey]
                                        :input private-doc))
            (assert! (zero? (:exit ingest-priv))
                     (str "private item ingest exits 0 (err: " (:err ingest-priv) ")"))

            (bind tree-result  (run-cli cli-bin base-url ["garden" "tree" "--json"]))
            (assert! (zero? (:exit tree-result)) "garden tree exits 0")
            (bind tree-resp    (json/parse-string (:out tree-result) true))
            (assert! (not (some #(str/includes? % actor1-uuid) (:paths tree-resp)))
                     "private item NOT visible in unauthenticated garden tree")

            (bind body-unauth  (run-cli cli-bin base-url ["garden" "body" private-path "--json"]))
            (assert! (not (zero? (:exit body-unauth)))
                     "garden body of private item fails without auth")

            (bind body-auth    (run-cli cli-bin base-url
                                        ["garden" "body" private-path "--json"
                                         "--actor" actor-1 "--passkey" actor1-passkey]))
            (assert! (zero? (:exit body-auth))
                     (str "garden body of private item succeeds with auth (err: " (:err body-auth) ")"))
            (bind auth-resp    (json/parse-string (:out body-auth) true))
            (assert! (str/includes? (or (:body auth-resp) "") "secret")
                     "private item body contains 'secret'"))

          ;; 4. query rankings via CLI
          (println "\nquerying garden children via CLI…")
          (letlocals
            (bind result (run-cli cli-bin base-url ["garden" "children" "languages" "--json"]))
            (assert! (zero? (:exit result)) "cli garden children exits 0")
            (bind resp   (json/parse-string (:out result) true))
            (bind ranked (mapv :item (mapcat :ranking (:components resp))))
            (assert! (= 3 (count ranked))
                     (str "3 items ranked (got " (count ranked) ")"))
            (assert! (str/ends-with? (first ranked) "languages/rust")
                     (str "rust is #1 (got " (first ranked) ")"))
            (assert! (str/ends-with? (last ranked) "languages/go")
                     (str "go is #3 (got " (last ranked) ")")))

          ;; 5. verify JSONL written
          ;; 3 events: ActorKeyRegistration + Ingest (actor1) + Ingest (private item)
          (assert! (fs/exists? event-log) "events.jsonl exists")
          (letlocals
            (bind lines (->> (slurp event-log) str/split-lines (remove str/blank?)))
            (assert! (= 3 (count lines))
                     (str "3 events in JSONL after actor1 ingests (ActorKeyRegistration + 2x Ingest, got "
                          (count lines) ")")))

          ;; 6. query forum via CLI
          (println "\nquerying forum via CLI…")
          (letlocals
            (bind result (run-cli cli-bin base-url ["forum" "--json"]))
            (assert! (zero? (:exit result)) "cli forum exits 0")
            (bind resp   (json/parse-string (:out result) true))
            (assert! (some (fn [t] (= "#sanity-test" (:thread t))) (:threads resp))
                     "thread '#sanity-test' visible in forum"))

          ;; 7. query item body via CLI
          (letlocals
            (bind result (run-cli cli-bin base-url ["garden" "body" "languages/rust" "--json"]))
            (assert! (zero? (:exit result)) "cli garden body exits 0")
            (bind resp   (json/parse-string (:out result) true))
            (assert! (str/includes? (or (:body resp) "") "ownership")
                     "rust body contains 'ownership'"))

          ;; 8. feed: second actor ingests, actor-1 uses feed to see what happened
          (println "\ntesting feed endpoint…")
          (letlocals
            (bind result (run-cli cli-bin base-url ["ingest" "--json"] :input sorter-doc-2))
            (assert! (zero? (:exit result)) "second actor ingest exits 0"))
          (letlocals
            (bind result (run-cli cli-bin base-url ["feed" actor-1 "--json"]))
            (assert! (zero? (:exit result)) "cli feed exits 0")
            (bind resp   (json/parse-string (:out result) true))
            (assert! (some? (:since resp)) "feed since is set (actor has posted)")
            (assert! (pos? (:total resp))
                     (str "feed has >= 1 post (got " (:total resp) ")"))
            (assert! (not-empty (:posts resp)) "feed posts non-empty"))

          ;; 9. global rank endpoint
          (println "\ntesting global rank endpoint…")
          (letlocals
            (bind result (run-cli cli-bin base-url ["garden" "rank" "--json"]))
            (assert! (zero? (:exit result)) "cli garden rank exits 0")
            (bind resp   (json/parse-string (:out result) true))
            (assert! (pos? (:ranked_total resp)) "global rank has ranked items")
            (assert! (not-empty (:items resp)) "global rank returns items")
            (assert! (str/ends-with? (:item (first (:items resp))) "languages/rust")
                     (str "rust is #1 globally (got " (:item (first (:items resp))) ")")))
          (letlocals
            (bind result  (run-cli cli-bin base-url ["garden" "rank" "--percent" "--limit" "2" "--json"]))
            (assert! (zero? (:exit result)) "cli garden rank --percent --limit 2 exits 0")
            (bind resp    (json/parse-string (:out result) true))
            (assert! (= 2 (count (:items resp))) "limit=2 returns 2 items")
            (assert! (= 100.0 (:percent (first (:items resp)))) "top item has 100.0% score")
            (bind result2 (run-cli cli-bin base-url ["garden" "rank" "--limit" "1" "--offset" "1" "--json"]))
            (assert! (zero? (:exit result2)) "cli garden rank --offset 1 exits 0")
            (bind resp2   (json/parse-string (:out result2) true))
            (assert! (= (:item (second (:items resp))) (:item (first (:items resp2))))
                     "offset=1 aligns with page 0 item index 1"))

          ;; 10. rank history endpoint
          (println "\ntesting rank history endpoint…")
          (letlocals
            ;; First ingest already ran (sorter-doc). Now ingest a second doc that votes on
            ;; languages/rust twice in one document — the multi-vote-per-ingest case.
            (bind two-vote-doc (str/join "\n"
                                 ["@00000000-0000-0000-0000-000000000003:sanity:local/test"
                                  "#sanity-test"
                                  "~/languages/rust 4:1 ~/languages/python { type safety }"
                                  "~/languages/rust 3:1 ~/languages/go { zero-cost abstractions }"]))
            (bind ingest-result (run-cli cli-bin base-url ["ingest" "--json"] :input two-vote-doc))
            (assert! (zero? (:exit ingest-result))
                     (str "two-vote ingest exits 0 (err: " (:err ingest-result) ")"))

            (bind result (run-cli cli-bin base-url ["garden" "history" "languages/rust" "--json"]))
            (assert! (zero? (:exit result))
                     (str "cli garden history exits 0 (err: " (:err result) ")"))
            (bind resp   (json/parse-string (:out result) true))
            (assert! (= "/languages/rust" (:item resp)) "history item path is /languages/rust")
            (assert! (>= (count (:history resp)) 2)
                     (str "rust has at least 2 history entries (got " (count (:history resp)) ")"))

            ;; The last entry is from the two-vote doc — caused_by must have 2 votes.
            (bind last-entry (last (:history resp)))
            (assert! (= 2 (count (:caused_by last-entry)))
                     (str "last entry has 2 caused_by votes (got " (count (:caused_by last-entry)) ")"))
            (assert! (= 1 (:scope_rank last-entry))
                     (str "rust still #1 in scope after two-vote ingest (got " (:scope_rank last-entry) ")")))

          (finally
            (println "\nkilling server (pid" server-pid ")…")
            (.destroyForcibly (:proc server))
            (deref server))))

      ;; 10. restart from same JSONL — prove replay determinism
      (println "\nrestarting server from persisted JSONL…")
      (letlocals
        (bind server2 (p/process [server-bin]
                                  {:out :inherit :err :inherit
                                   :env (merge base-env
                                               {"SLUG_DATA_DIR" tmp-dir
                                                "SLUG_KEYS"     "test:test"
                                                "PORT"          (str port)
                                                "RUST_LOG"      "warn"})}))
        (try
          (assert! (wait-for-server base-url 10000) "restarted server responds to /healthz")

          ;; 11. same query, same result
          (println "\nquerying rankings after replay…")
          (letlocals
            (bind result (run-cli cli-bin base-url ["garden" "children" "languages" "--json"]))
            (assert! (zero? (:exit result)) "cli garden children exits 0 after restart")
            (bind resp   (json/parse-string (:out result) true))
            (bind ranked (mapv :item (mapcat :ranking (:components resp))))
            (assert! (= 3 (count ranked)) "3 items ranked after replay")
            (assert! (str/ends-with? (first ranked) "languages/rust")
                     (str "rust still #1 after replay (got " (first ranked) ")")))

          (finally
            (.destroyForcibly (:proc server2))
            (deref server2))))

      (finally
        (fs/delete-tree tmp-dir))))

  (let [{pass :pass} @counts]
    (println (str "\n" ansi-green "━━━ " pass " checks passed ━━━" ansi-reset "\n"))))
