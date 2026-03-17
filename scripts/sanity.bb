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

(defn- pass [msg] (println (str ansi-green "  ✓ " ansi-reset msg)))
(defn- fail [msg] (println (str ansi-red   "  ✗ " ansi-reset msg)))

(defn- assert! [pred msg]
  (if pred
    (pass msg)
    (do (fail msg)
        (throw (ex-info (str "FAIL: " msg) {})))))

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
;; the .sorter document under test
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

  ;; 1. build
  (println "building binaries…")
  (let [build @(p/process [(cargo-bin) "build" "--release" "-p" "slugsocial-server" "-p" "slugsocial"]
                          {:inherit true :env base-env})]
    (assert! (zero? (:exit build)) "cargo build succeeds"))

  (let [server-bin "target/release/slugsocial-server"
        cli-bin    "target/release/slugsocial"
        tmp-dir    (str (fs/create-temp-dir {:prefix "slug-sanity-"}))
        port       (pick-port)
        base-url   (str "http://127.0.0.1:" port)
        event-log  (str tmp-dir "/events.jsonl")]

    (assert! (fs/exists? server-bin) "server binary exists")
    (assert! (fs/exists? cli-bin)    "cli binary exists")

    (try
      ;; 2. boot server (first run — empty state)
      (println (str "\nstarting server on :" port " (data: " tmp-dir ")"))
      (let [server (p/process [server-bin]
                              {:out :inherit :err :inherit
                               :env (merge base-env
                                           {"SLUG_DATA_DIR" tmp-dir
                                            "SLUG_KEYS"     "test:test"
                                            "PORT"          (str port)
                                            "RUST_LOG"      "warn"})})
            server-pid (.pid (:proc server))]

        (try
          (assert! (wait-for-server base-url 10000) "server responds to /healthz")

          ;; 2.5 check endpoint returns grouped rankings (not flattened)
          (println "\nchecking (dry-run) returns discrete ranking groups…")
          (let [result (run-cli cli-bin base-url ["check" "--json"] :input check-doc-disconnected)]
            (assert! (zero? (:exit result)) "cli check exits 0")
            (let [resp (json/parse-string (:out result) true)]
              (assert! (:ok resp) "check response ok=true")
              (assert! (= 1 (count (:rankings resp))) "check returns one touched parent scope")
              (let [scope (first (:rankings resp))]
                (assert! (= "/disc" (:parent scope)) "check scope parent is /disc")
                (assert! (= 2 (count (:components scope))) "check preserves two disconnected components")
                (assert! (every? #(= 2 (count (:ranking %))) (:components scope))
                         "each component ranks 2 items")
                (assert! (empty? (:unranked_items scope)) "no unranked items for disconnected check doc"))))
          (assert! (not (fs/exists? event-log)) "check does not create events.jsonl")

          ;; 3. ingest via CLI
          (println "\ningesting .sorter document via CLI…")
          (let [result (run-cli cli-bin base-url ["ingest" "--json"] :input sorter-doc)]
            (assert! (zero? (:exit result)) "cli ingest exits 0")
            (let [resp (json/parse-string (:out result) true)
                  actor1-passkey (:passkey resp)]
              (assert! (:ok resp) "ingest response ok=true")
              (assert! (pos? (:events_appended resp)) "events_appended > 0")
              (assert! (some #(= "#sanity-test" %) (:threads resp))
                       "thread '#sanity-test' in response")
              (assert! (some? actor1-passkey) "first ingest returns a passkey")

              ;; private namespace tests
              (println "\ntesting private diary namespaces…")
              (let [actor1-uuid "00000000-0000-0000-0000-000000000001"
                    private-path (str actor1-uuid "/private-note")
                    ;; No #tag — private-only ingests don't need a thread
                    private-doc (str/join "\n"
                                  [actor-1
                                   ""
                                   (str "~/" private-path " { secret diary content }")])]

                ;; ingest the private item (actor1 is now registered, must supply passkey)
                (let [result (run-cli cli-bin base-url
                                      ["ingest" "--json" "--passkey" actor1-passkey]
                                      :input private-doc)]
                  (assert! (zero? (:exit result))
                           (str "private item ingest exits 0 (err: " (:err result) ")")))

                ;; garden tree without auth should NOT show private item
                (let [result (run-cli cli-bin base-url ["garden" "tree" "--json"])]
                  (assert! (zero? (:exit result)) "garden tree exits 0")
                  (let [resp (json/parse-string (:out result) true)
                        paths (:paths resp)]
                    (assert! (not (some #(str/includes? % actor1-uuid) paths))
                             "private item NOT visible in unauthenticated garden tree")))

                ;; garden body without auth should return 403
                (let [result (run-cli cli-bin base-url
                                      ["garden" "body" private-path "--json"])]
                  (assert! (not (zero? (:exit result)))
                           "garden body of private item fails without auth"))

                ;; garden body WITH auth should succeed
                (let [result (run-cli cli-bin base-url
                                      ["garden" "body" private-path "--json"
                                       "--actor" actor-1
                                       "--passkey" actor1-passkey])]
                  (assert! (zero? (:exit result))
                           (str "garden body of private item succeeds with auth (err: " (:err result) ")"))
                  (let [resp (json/parse-string (:out result) true)]
                    (assert! (str/includes? (or (:body resp) "") "secret")
                             "private item body contains 'secret'"))))))

          ;; 4. query rankings via CLI
          (println "\nquerying garden children via CLI…")
          (let [result (run-cli cli-bin base-url ["garden" "children" "languages" "--json"])]
            (assert! (zero? (:exit result)) "cli garden children exits 0")
            (let [resp   (json/parse-string (:out result) true)
                  ranked (mapv :item (mapcat :ranking (:components resp)))]
              (assert! (= 3 (count ranked))
                       (str "3 items ranked (got " (count ranked) ")"))
              (assert! (str/ends-with? (first ranked) "languages/rust")
                       (str "rust is #1 (got " (first ranked) ")"))
              (assert! (str/ends-with? (last ranked) "languages/go")
                       (str "go is #3 (got " (last ranked) ")"))))

          ;; 5. verify JSONL written
          ;; 3 events: ActorKeyRegistration + Ingest (actor1) + Ingest (private item)
          (assert! (fs/exists? event-log) "events.jsonl exists")
          (let [lines (->> (slurp event-log) str/split-lines (remove str/blank?))]
            (assert! (= 3 (count lines))
                     (str "3 events in JSONL after actor1 ingests (ActorKeyRegistration + 2x Ingest, got " (count lines) ")")))

          ;; 6. query forum via CLI
          (println "\nquerying forum via CLI…")
          (let [result (run-cli cli-bin base-url ["forum" "--json"])]
            (assert! (zero? (:exit result)) "cli forum exits 0")
            (let [resp (json/parse-string (:out result) true)]
              (assert! (some (fn [t] (= "#sanity-test" (:thread t))) (:threads resp))
                       "thread '#sanity-test' visible in forum")))

          ;; 7. query item body via CLI
          (let [result (run-cli cli-bin base-url ["garden" "body" "languages/rust" "--json"])]
            (assert! (zero? (:exit result)) "cli garden body exits 0")
            (let [resp (json/parse-string (:out result) true)]
              (assert! (str/includes? (or (:body resp) "") "ownership")
                       "rust body contains 'ownership'")))

          ;; 8. digest: second actor votes, triggering a notification for actor-1
          (println "\ntesting digest (second actor ingest + notification check)…")
          (let [result (run-cli cli-bin base-url ["ingest" "--json"] :input sorter-doc-2)]
            (assert! (zero? (:exit result)) "second actor ingest exits 0"))

          (let [result (run-cli cli-bin base-url ["digest" actor-1 "--json"])]
            (assert! (zero? (:exit result)) "cli digest exits 0")
            (let [resp (json/parse-string (:out result) true)]
              (assert! (:ok resp) "digest response ok=true")
              (assert! (some? (:since resp)) "digest since is set (actor has posted)")
              (assert! (pos? (count (:notifications resp)))
                       (str "digest has >= 1 notification (got " (count (:notifications resp)) ")"))))

          (finally
            ;; kill first server
            (println "\nkilling server (pid" (.pid (:proc server)) ")…")
            (.destroyForcibly (:proc server))
            (deref server))))

      ;; 8. restart from same JSONL — prove replay determinism
      (println "\nrestarting server from persisted JSONL…")
      (let [server2 (p/process [server-bin]
                               {:out :inherit :err :inherit
                                :env (merge base-env
                                            {"SLUG_DATA_DIR" tmp-dir
                                             "SLUG_KEYS"     "test:test"
                                             "PORT"          (str port)
                                             "RUST_LOG"      "warn"})})
            _ nil]
        (try
          (assert! (wait-for-server base-url 10000) "restarted server responds to /healthz")

          ;; 9. same query, same result
          (println "\nquerying rankings after replay…")
          (let [result (run-cli cli-bin base-url ["garden" "children" "languages" "--json"])]
            (assert! (zero? (:exit result)) "cli garden children exits 0 after restart")
            (let [resp   (json/parse-string (:out result) true)
                  ranked (mapv :item (mapcat :ranking (:components resp)))]
              (assert! (= 3 (count ranked))
                       "3 items ranked after replay")
              (assert! (str/ends-with? (first ranked) "languages/rust")
                       (str "rust still #1 after replay (got " (first ranked) ")"))))

          (finally
            (.destroyForcibly (:proc server2))
            (deref server2))))

      (finally
        ;; cleanup
        (fs/delete-tree tmp-dir)))

    (println (str "\n" ansi-green "━━━ all checks passed ━━━" ansi-reset "\n"))))
