(ns scripts.perf
  "Performance test: fires concurrent HTTP requests at HTML routes to detect
   blocking I/O in the view count path (or anywhere else).
   Boots a server, hammers it, reports p50/p95/p99/max latencies."
  (:require [babashka.process :as p]
            [babashka.http-client :as http]
            [clojure.string :as str]
            [babashka.fs :as fs]))

;; ---------------------------------------------------------------------------
;; helpers (shared with integration test pattern)
;; ---------------------------------------------------------------------------

(def ^:private ansi-green "\033[32m")
(def ^:private ansi-red   "\033[31m")
(def ^:private ansi-yellow "\033[33m")
(def ^:private ansi-bold  "\033[1m")
(def ^:private ansi-reset "\033[0m")

(defn- pick-port []
  (let [ss (java.net.ServerSocket. 0)]
    (try (.getLocalPort ss) (finally (.close ss)))))

(defn- wait-for-server [base-url timeout-ms]
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

(defn- cargo-bin []
  (let [local (str cargo-home "/cargo")]
    (if (fs/exists? local) local "cargo")))

;; ---------------------------------------------------------------------------
;; HTTP timing
;; ---------------------------------------------------------------------------

(defn- timed-get
  "GET a URL, return {:status :ms :error?}."
  [url]
  (let [start (System/nanoTime)]
    (try
      (let [resp       (http/get url {:throw false :timeout 30000})
            elapsed-ms (/ (- (System/nanoTime) start) 1e6)]
        {:status (:status resp) :ms elapsed-ms :error? false})
      (catch Exception e
        (let [elapsed-ms (/ (- (System/nanoTime) start) 1e6)]
          {:status 0 :ms elapsed-ms :error? true :msg (.getMessage e)})))))

(defn- percentile [sorted-vals p]
  (let [n (count sorted-vals)
        idx (min (dec n) (int (Math/floor (* p n))))]
    (nth sorted-vals idx)))

(defn- report-latencies [label results]
  (let [times   (sort (mapv :ms results))
        errors  (count (filter :error? results))
        n       (count times)
        p50     (percentile times 0.50)
        p95     (percentile times 0.95)
        p99     (percentile times 0.99)
        max-ms  (last times)
        color   (cond
                  (> max-ms 5000) ansi-red
                  (> p95 1000)    ansi-red
                  (> p95 200)     ansi-yellow
                  :else           ansi-green)]
    (println (str "\n  " ansi-bold label ansi-reset
                  "  (" n " requests, " errors " errors)"))
    (println (str "    p50=" (format "%.0fms" p50)
                  "  p95=" (format "%.0fms" p95)
                  "  p99=" (format "%.0fms" p99)
                  "  " color "max=" (format "%.0fms" max-ms) ansi-reset))
    {:label label :p50 p50 :p95 p95 :p99 p99 :max max-ms :errors errors}))

;; ---------------------------------------------------------------------------
;; test scenarios
;; ---------------------------------------------------------------------------

(defn- run-sequential
  "Fire `n` sequential GETs to `url`."
  [url n]
  (mapv (fn [_] (timed-get url)) (range n)))

(defn- run-concurrent
  "Fire `n` concurrent GETs to `url` using virtual threads."
  [url n]
  (let [futures (mapv (fn [_]
                        (future (timed-get url)))
                      (range n))]
    (mapv deref futures)))

;; ---------------------------------------------------------------------------
;; main
;; ---------------------------------------------------------------------------

(defn perf [& _args]
  (println "\n━━━ slug performance test ━━━\n")

  (println "building release binary…")
  (let [build @(p/process [(cargo-bin) "build" "--release" "-p" "slugsocial-server"]
                          {:inherit true :env base-env})]
    (when-not (zero? (:exit build))
      (println "build failed")
      (System/exit 1)))

  (let [server-bin "target/release/slugsocial-server"
        tmp-dir    (str (fs/create-temp-dir {:prefix "slug-perf-"}))
        port       (pick-port)
        base-url   (str "http://127.0.0.1:" port)]

    (try
      (println (str "starting server on :" port " (data: " tmp-dir ")"))
      (let [server (p/process [server-bin]
                              {:out :inherit :err :inherit
                               :env (merge base-env
                                           {"SLUG_DATA_DIR" tmp-dir
                                            "SLUG_KEYS"     "test:test"
                                            "PORT"          (str port)
                                            "RUST_LOG"      "warn"})})]
        (try
          (when-not (wait-for-server base-url 10000)
            (println "server failed to start")
            (System/exit 1))

          (println "server ready\n")

          ;; Warm up
          (println "warming up (10 requests)…")
          (run-sequential (str base-url "/") 10)

          ;; Test 1: sequential GETs to / (forum index)
          (let [r1 (report-latencies
                     "sequential 50x GET /"
                     (run-sequential (str base-url "/") 50))]

            ;; Test 2: sequential GETs to /~ (garden index)
            (let [r2 (report-latencies
                       "sequential 50x GET /~"
                       (run-sequential (str base-url "/~") 50))]

              ;; Test 3: concurrent GETs to / (this is where blocking I/O kills you)
              (let [r3 (report-latencies
                         "concurrent 50x GET /"
                         (run-concurrent (str base-url "/") 50))]

                ;; Test 4: concurrent GETs to /~ 
                (let [r4 (report-latencies
                           "concurrent 50x GET /~"
                           (run-concurrent (str base-url "/~") 50))]

                  ;; Test 5: burst — 200 concurrent to mixed routes
                  (let [urls  (cycle [(str base-url "/")
                                      (str base-url "/~")
                                      (str base-url "/healthz")])
                        r5 (report-latencies
                             "concurrent 200x GET mixed (/, /~, /healthz)"
                             (let [futures (mapv (fn [url] (future (timed-get url)))
                                                (take 200 urls))]
                               (mapv deref futures)))]

                    ;; Test 6: /healthz baseline (no view counts, no templates)
                    (let [r6 (report-latencies
                               "concurrent 50x GET /healthz (baseline)"
                               (run-concurrent (str base-url "/healthz") 50))]

                      ;; Summary
                      (println (str "\n━━━ summary ━━━"))
                      (let [all-results [r1 r2 r3 r4 r5 r6]
                            worst-p95   (apply max (map :p95 all-results))
                            worst-max   (apply max (map :max all-results))]
                        (if (or (> worst-p95 1000) (> worst-max 5000))
                          (do
                            (println (str ansi-red
                                         "\n  FAIL: worst p95=" (format "%.0fms" worst-p95)
                                         " max=" (format "%.0fms" worst-max)
                                         ansi-reset))
                            (println "  view count path is likely blocking the runtime"))
                          (println (str ansi-green
                                       "\n  PASS: worst p95=" (format "%.0fms" worst-p95)
                                       " max=" (format "%.0fms" worst-max)
                                       ansi-reset))))))))))

          (finally
            (.destroyForcibly (:proc server))
            (deref server))))

      (finally
        (fs/delete-tree tmp-dir))))

  (println))
