(ns test.common
  "Shared utilities for slug test rigs: port allocation, server lifecycle,
   environment helpers, CLI runner, test harness (ANSI + pass/fail counts),
   standard server env for bb suites, and the letlocals macro."
  (:require [babashka.process :as p]
            [clojure.string :as str]
            [babashka.fs :as fs]))

;; ---------------------------------------------------------------------------
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
;; environment
;; ---------------------------------------------------------------------------

(def cargo-home (str (System/getProperty "user.home") "/.cargo/bin"))

(def base-env
  (-> (into {} (System/getenv))
      (as-> env
            (if (str/includes? (get env "PATH" "") cargo-home)
              env
              (assoc env "PATH" (str cargo-home ":" (get env "PATH" "")))))))

;; ---------------------------------------------------------------------------
;; test harness (ANSI + counters)
;; ---------------------------------------------------------------------------

(def ansi-green "\033[32m")
(def ansi-red   "\033[31m")
(def ansi-reset "\033[0m")

(defn test-pass! [counts-atom msg]
  (swap! counts-atom update :pass inc)
  (println (str ansi-green "  ✓ " ansi-reset msg)))

(defn test-fail! [counts-atom msg]
  (swap! counts-atom update :fail inc)
  (println (str ansi-red "  ✗ " ansi-reset msg)))

(defn test-assert! [counts-atom pred msg]
  (if pred
    (test-pass! counts-atom msg)
    (do (test-fail! counts-atom msg)
        (throw (ex-info (str "FAIL: " msg) {})))))

(defn slug-server-env
  "Env map for `slugsocial-server` in bb integration tests (mock Google URLs, data dir, keys)."
  [tmp-dir base-url google-base-url slug-port]
  (merge base-env
         {"SLUG_DATA_DIR" tmp-dir
          "SLUG_KEYS"     "test:test"
          "PORT"          (str slug-port)
          "RUST_LOG"      "warn"
          "SLUG_PUBLIC_URL" base-url
          "SLUG_GOOGLE_AUTH_URL" (str google-base-url "/o/oauth2/v2/auth")
          "SLUG_GOOGLE_TOKEN_URL" (str google-base-url "/token")
          "SLUG_GOOGLE_CLIENT_ID" "mock"
          "SLUG_GOOGLE_CLIENT_SECRET" "mock"}))

(defn cargo-bin
  "Resolve cargo, preferring ~/.cargo/bin if not on PATH."
  []
  (if (fs/exists? (str cargo-home "/cargo"))
    (str cargo-home "/cargo")
    "cargo"))

(defn run-cargo-build-release!
  "`cargo build --release` for each package name in `packages` (e.g. [\"slugsocial-server\"])."
  [packages]
  @(p/process (into [(cargo-bin) "build" "--release"]
                    (mapcat (fn [p] ["-p" p]) packages))
              {:inherit true :env base-env}))

;; ---------------------------------------------------------------------------
;; port / server utilities
;; ---------------------------------------------------------------------------

(defn pick-port
  "Find a free port by binding :0 then closing."
  []
  (letlocals
   (bind ss (java.net.ServerSocket. 0))
   (bind port (.getLocalPort ss))
   (.close ss)
   port))

(defn wait-for-server
  "Poll /healthz until it returns 'ok', up to `timeout-ms`."
  [base-url timeout-ms]
  (letlocals
   (bind deadline (+ (System/currentTimeMillis) timeout-ms))
   (loop []
     (if (try (= "ok" (str/trim (slurp (str base-url "/healthz"))))
              (catch Exception _ false))
       true
       (if (< (System/currentTimeMillis) deadline)
         (do (Thread/sleep 200) (recur))
         false)))))

(defn start-server
  "Start the slugsocial-server binary with the given env map.
   Returns the babashka.process map.

   When `log-file` (string path) is provided, stdout and stderr are appended there
   instead of inheriting the parent descriptors. Inheriting shared pipes while the
   parent blocks on HTTP I/O can fill the pipe buffer and deadlock the server on log writes."
  ([server-bin env-map]
   (start-server server-bin env-map nil))
  ([server-bin env-map log-file]
   (p/process [server-bin]
              (if log-file
                ;; Two string paths (same file): babashka.process can deref the process cleanly.
                ;; :err :out + ProcessBuilder$Redirect breaks stream copying in deref/kill-server.
                {:env env-map :out log-file :err log-file}
                {:out :inherit :err :inherit :env env-map}))))

(defn kill-server
  "Forcibly kill a server process (babashka.process map) and wait for it to exit."
  [server]
  (.destroyForcibly (:proc server))
  (deref server))

;; ---------------------------------------------------------------------------
;; CLI runner
;; ---------------------------------------------------------------------------

(defn run-cli
  "Run the slugsocial CLI binary with args, return {:exit :out :err}.
   Optional `extra-env` map merged into process env (e.g. SLUG_BEARER_TOKEN)."
  [binary base-url args & {:keys [input extra-env]}]
  @(p/process (into [binary] args)
              (cond-> {:out :string :err :string
                       :env (merge base-env {"SLUG_SERVER" base-url} extra-env)}
                input (assoc :in input))))
