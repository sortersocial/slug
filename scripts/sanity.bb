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
