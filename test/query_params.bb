(ns query-params
  (:require
    [babashka.fs :as fs]
    [babashka.http-client :as http]
    [babashka.process :as p]
    [cheshire.core :as json]
    [clojure.string :as str]))

(def ansi-green "\u001b[32m")
(def ansi-red "\u001b[31m")
(def ansi-reset "\u001b[0m")

(defn assert! [pred msg]
  (when-not pred
    (throw (ex-info (str ansi-red "ASSERT FAILED: " msg ansi-reset) {}))))

(defn wait-for-server [base-url timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (when (> (System/currentTimeMillis) deadline)
        (throw (ex-info "timeout waiting for server" {:base-url base-url})))
      (let [resp (try
                   (http/get (str base-url "/healthz") {:throw false})
                   (catch Exception _ nil))]
        (if (and resp (= 200 (:status resp)))
          true
          (do (Thread/sleep 100) (recur)))))))

(defn urlenc [s]
  (java.net.URLEncoder/encode (str s) "UTF-8"))

(defn -main []
(println "testing axum query param decoding + repeated open=…")
  (let [tmp-dir (fs/create-temp-dir {:prefix "slug-query-params-"})
        data-dir (str tmp-dir)
        server-bin "./target/debug/slugsocial-server"
        base-url "http://127.0.0.1:18081"
        env {"PORT" "18081"
             "SLUG_DATA_DIR" data-dir
             "SLUG_EVENT_LOG" (str data-dir "/events.jsonl")
             "SLUG_KEYS" "dev:dev"
             "RUST_LOG" "warn"}
        !server (atom nil)]
    (try
      ;; build server if needed
      (when-not (fs/exists? server-bin)
        (println "building server binary…")
        (let [r (p/shell {:out :inherit :err :inherit} "cargo build -p slugsocial-server")]
          (assert! (zero? (:exit r)) (str "cargo build exits 0 (got " (:exit r) ")"))))

      (println "starting server…")
      (let [srv (p/process [server-bin] {:out :inherit :err :inherit :env env})]
        (reset! !server srv))
      (assert! (wait-for-server base-url 10000) "server responds to /healthz")

      (let [open1 "~/models/llms"
            open2 "~/music/jazz"
            q     "hello world"
            uri   (str base-url "/debug/query?"
                       "open=" (urlenc open1)
                       "&open=" (urlenc open2)
                       "&selected=" (urlenc open1)
                       "&q=" (urlenc q))
            resp  (http/get uri {:throw true})
            body  (json/parse-string (:body resp) true)]
        (assert! (= 200 (:status resp)) "debug endpoint returns 200")
        (assert! (= [open1 open2] (:open body))
                 (str "open[] repeated parses to vec (got " (:open body) ")"))
        (assert! (= open1 (:selected body))
                 (str "selected decodes (got " (:selected body) ")"))
        (assert! (= q (:q body))
                 (str "q decodes spaces (got " (:q body) ")")))

      (println (str "\n" ansi-green "━━━ query param checks passed ━━━" ansi-reset "\n"))
      (finally
        (when-some [s @!server]
          (println "killing server…")
          (.destroyForcibly (:proc s))
          (deref s))
        (fs/delete-tree tmp-dir)))))

(-main)

