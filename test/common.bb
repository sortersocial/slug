(ns test.common
  "Shared utilities for slug test rigs: port allocation, server lifecycle,
   environment helpers, CLI runner, and the letlocals macro."
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

(defn cargo-bin
  "Resolve cargo, preferring ~/.cargo/bin if not on PATH."
  []
  (if (fs/exists? (str cargo-home "/cargo"))
    (str cargo-home "/cargo")
    "cargo"))

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
   Returns the babashka.process map."
  [server-bin env-map]
  (p/process [server-bin] {:out :inherit :err :inherit :env env-map}))

(defn kill-server
  "Forcibly kill a server process (babashka.process map) and wait for it to exit."
  [server]
  (.destroyForcibly (:proc server))
  (deref server))

;; ---------------------------------------------------------------------------
;; CLI runner
;; ---------------------------------------------------------------------------

(defn run-cli
  "Run the slugsocial CLI binary with args, return {:exit :out :err}."
  [binary base-url args & {:keys [input]}]
  @(p/process (into [binary] args)
              (cond-> {:out :string :err :string
                       :env (merge base-env {"SLUG_SERVER" base-url})}
                input (assoc :in input))))
