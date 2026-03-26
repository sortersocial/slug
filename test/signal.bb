(ns test.signal
  "Signal bot integration test. Starts a mock signal-cli-api server, boots the
   slug server with signal bot enabled pointing at the mock, and verifies that:
   1. The bot polls messages and ingests valid DSL messages
   2. Invalid DSL messages trigger an error reply (captured by the mock)
   3. Phone hash is stored as provenance (no PII in event log)
   4. Duplicate messages are not re-ingested after server restart"
  (:require [babashka.process :as p]
            [clojure.string :as str]
            [babashka.fs :as fs]
            [cheshire.core :as json]
            [org.httpkit.server :as http]))

;; ---------------------------------------------------------------------------
;; letlocals
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
;; helpers
;; ---------------------------------------------------------------------------

(def ^:private ansi-green "\033[32m")
(def ^:private ansi-red   "\033[31m")
(def ^:private ansi-reset "\033[0m")

(def ^:private counts (atom {:pass 0 :fail 0}))

(defn- pass [msg]
  (swap! counts update :pass inc)
  (println (str ansi-green "  \u2713 " ansi-reset msg)))

(defn- fail [msg]
  (swap! counts update :fail inc)
  (println (str ansi-red "  \u2717 " ansi-reset msg)))

(defn- assert! [pred msg]
  (if pred
    (pass msg)
    (do (fail msg)
        (throw (ex-info (str "FAIL: " msg) {})))))

(defn- pick-port []
  (letlocals
    (bind ss (java.net.ServerSocket. 0))
    (bind port (.getLocalPort ss))
    (.close ss)
    port))

(defn- wait-for-server [base-url timeout-ms]
  (letlocals
    (bind deadline (+ (System/currentTimeMillis) timeout-ms))
    (loop []
      (if (try (= "ok" (str/trim (slurp (str base-url "/healthz"))))
               (catch Exception _ false))
        true
        (if (< (System/currentTimeMillis) deadline)
          (do (Thread/sleep 200) (recur))
          false)))))

(def ^:private cargo-home (str (System/getProperty "user.home") "/.cargo/bin"))

(def ^:private base-env
  (-> (into {} (System/getenv))
      (as-> env
        (if (str/includes? (get env "PATH" "") cargo-home)
          env
          (assoc env "PATH" (str cargo-home ":" (get env "PATH" "")))))))

(defn- cargo-bin []
  (if (fs/exists? (str cargo-home "/cargo"))
    (str cargo-home "/cargo")
    "cargo"))

(defn- run-cli [binary base-url args & {:keys [input]}]
  @(p/process (into [binary] args)
              (cond-> {:out :string :err :string
                       :env (merge base-env {"SLUG_SERVER" base-url})}
                input (assoc :in input))))

;; ---------------------------------------------------------------------------
;; mock signal-cli-api server
;; ---------------------------------------------------------------------------

(defn- make-envelope
  "Build a signal-cli-api envelope for a data message."
  [source timestamp message]
  {"envelope" {"source"      source
               "timestamp"   timestamp
               "dataMessage" {"message"   message
                              "timestamp" timestamp}}})

(defn- start-mock-signal-api
  "Start a mock signal-cli-api server. Returns {:stop-fn f :port n :state atom}.
   The state atom tracks:
     :receive-requests — count of GET /v1/receive/... requests
     :send-requests    — vec of POST /v2/send bodies (reply attempts)
   The messages-fn is called with () and should return a vec of envelope maps."
  [port messages-fn]
  (letlocals
    (bind state (atom {:receive-requests 0
                       :send-requests    []}))
    (bind handler
      (fn [req]
        (cond
          ;; GET /v1/receive/{number}
          (and (= :get (:request-method req))
               (str/includes? (:uri req) "/v1/receive/"))
          (letlocals
            (swap! state update :receive-requests inc)
            (bind body (json/generate-string (messages-fn)))
            {:status  200
             :headers {"Content-Type" "application/json"}
             :body    body})

          ;; POST /v2/send (bot reply)
          (and (= :post (:request-method req))
               (= "/v2/send" (:uri req)))
          (letlocals
            (bind body-str (slurp (:body req)))
            (bind body-json (json/parse-string body-str true))
            (swap! state update :send-requests conj body-json)
            {:status  201
             :headers {"Content-Type" "application/json"}
             :body    (json/generate-string {"timestamp" (System/currentTimeMillis)})})

          ;; Everything else -> 404
          :else
          {:status 404 :body "not found"})))

    (bind stop-fn (http/run-server handler {:port port}))
    {:stop-fn stop-fn :port port :state state}))

;; ---------------------------------------------------------------------------
;; test fixtures
;; ---------------------------------------------------------------------------

(def ^:private bot-phone "+15550001234")

;; Valid DSL message: defines items, votes, thread tag
(def ^:private valid-message-text
  (str "#signal-test\n"
       "~/signal/alpha { First test item from Signal }\n"
       "~/signal/beta  { Second test item from Signal }\n"
       "~/signal/alpha 3:1 ~/signal/beta { alpha is better }"))

;; Invalid DSL message: vote on undefined items
(def ^:private invalid-message-text
  "~/nonexistent/a 2:1 ~/nonexistent/b { this should fail }")

;; ---------------------------------------------------------------------------
;; main
;; ---------------------------------------------------------------------------

(defn signal-test [& _args]
  (println "\n\u2501\u2501\u2501 signal bot integration check \u2501\u2501\u2501\n")
  (reset! counts {:pass 0 :fail 0})

  (println "building server binary\u2026")
  (letlocals
    (bind build @(p/process [(cargo-bin) "build" "--release" "-p" "slugsocial-server"]
                            {:inherit true :env base-env}))
    (assert! (zero? (:exit build)) "cargo build succeeds")

    (bind server-bin  "target/release/slugsocial-server")
    (bind cli-bin     "target/release/slugsocial")
    (bind tmp-dir     (str (fs/create-temp-dir {:prefix "slug-signal-"})))
    (bind slug-port   (pick-port))
    (bind mock-port   (pick-port))
    (bind base-url    (str "http://127.0.0.1:" slug-port))
    (bind mock-url    (str "http://127.0.0.1:" mock-port))
    (bind event-log   (str tmp-dir "/events.jsonl"))

    (bind !server     (atom nil))
    (bind !server2    (atom nil))
    (bind !mock       (atom nil))

    ;; Track which messages have been served.
    ;; Phase 1: valid message (ts 1000001)
    ;; Phase 2: invalid message (ts 1000002) — should trigger reply
    ;; Phase 3: same messages again — should be deduped
    (bind phase       (atom :valid))

    (bind messages-fn
      (fn []
        (case @phase
          :valid
          [(make-envelope "+15559991111" 1000001 valid-message-text)]

          :invalid
          [(make-envelope "+15559992222" 1000002 invalid-message-text)]

          :replay
          ;; Return both messages — bot should skip both (dedup)
          [(make-envelope "+15559991111" 1000001 valid-message-text)
           (make-envelope "+15559992222" 1000002 invalid-message-text)]

          ;; Default: empty
          [])))

    (bind server-env
      (merge base-env
             {"SLUG_DATA_DIR"         tmp-dir
              "SLUG_KEYS"             "test:test"
              "PORT"                  (str slug-port)
              "RUST_LOG"              "info"
              "SLUG_SIGNAL_API_URL"   mock-url
              "SLUG_SIGNAL_PHONE"     bot-phone
              "SLUG_SIGNAL_POLL_SECS" "2"
              "SLUG_PUBLIC_URL"       "https://slug.social"}))

    (try
      (letlocals
        ;; 1. Start mock signal-cli-api
        (println (str "\nstarting mock signal-cli-api on :" mock-port))
        (bind mock (start-mock-signal-api mock-port messages-fn))
        (reset! !mock mock)
        (assert! (some? (:stop-fn mock)) "mock signal-cli-api started")

        ;; 2. Boot slug server with signal bot enabled
        (println (str "starting slug server on :" slug-port " (signal bot polling every 2s)"))
        (bind server (p/process [server-bin] {:out :inherit :err :inherit :env server-env}))
        (reset! !server server)
        (assert! (wait-for-server base-url 10000) "server responds to /healthz")

        ;; 3. Wait for bot to poll and ingest the valid message
        (println "\nwaiting for bot to ingest valid signal message\u2026")
        (Thread/sleep 5000)

        ;; 4. Verify the valid message was ingested
        (bind mock-state @(:state mock))
        (assert! (pos? (:receive-requests mock-state))
                 (str "mock received poll requests (got " (:receive-requests mock-state) ")"))

        ;; Check event log for SignalMessage + ActorKeyRegistration + Ingest
        (assert! (fs/exists? event-log) "events.jsonl exists")
        (bind events (->> (slurp event-log) str/split-lines (remove str/blank?) (mapv #(json/parse-string % true))))
        (assert! (>= (count events) 3)
                 (str "at least 3 events (SignalMessage + ActorKeyRegistration + Ingest, got " (count events) ")"))

        (bind signal-events (filterv #(= "signal_message" (:type %)) events))
        (assert! (= 1 (count signal-events))
                 (str "exactly 1 SignalMessage event (got " (count signal-events) ")"))

        (bind sm (first signal-events))
        (assert! (= "1000001" (:signal_ts sm)) "SignalMessage signal_ts is 1000001")
        (assert! (some? (:phone_hash sm)) "SignalMessage has phone_hash")
        (assert! (not (str/includes? (or (:phone_hash sm) "") "+1555"))
                 "phone_hash does not contain raw phone number")
        (assert! (= 64 (count (:phone_hash sm)))
                 (str "phone_hash is 64-char SHA-256 hex (got " (count (:phone_hash sm)) ")"))
        (assert! (str/includes? (:actor sm) "signal") "actor contains 'signal'")

        ;; Check rankings via CLI
        (println "\nverifying rankings from bot-ingested message\u2026")
        (bind children-result (run-cli cli-bin base-url ["garden" "children" "signal" "--json"]))
        (assert! (zero? (:exit children-result)) "garden children signal exits 0")
        (bind children-resp (json/parse-string (:out children-result) true))
        (bind ranked (mapv :item (mapcat :ranking (:components children-resp))))
        (assert! (= 2 (count ranked)) (str "2 items ranked under /signal (got " (count ranked) ")"))
        (assert! (str/ends-with? (first ranked) "signal/alpha")
                 (str "alpha is #1 (got " (first ranked) ")"))

        ;; Check thread was created
        (bind forum-result (run-cli cli-bin base-url ["forum" "--json"]))
        (assert! (zero? (:exit forum-result)) "forum exits 0")
        (bind forum-resp (json/parse-string (:out forum-result) true))
        (assert! (some (fn [t] (= "#signal-test" (:thread t))) (:threads forum-resp))
                 "thread #signal-test visible in forum")

        ;; 5. Phase 2: invalid message -> bot should reply with error
        (println "\nswitching to invalid message phase\u2026")
        (reset! phase :invalid)
        (Thread/sleep 5000)

        (bind mock-state2 @(:state mock))
        (assert! (pos? (count (:send-requests mock-state2)))
                 (str "bot posted a reply (got " (count (:send-requests mock-state2)) " replies)"))
        (bind reply (first (:send-requests mock-state2)))
        (assert! (some? reply) "reply body captured")
        (assert! (str/includes? (or (:message reply) "") "/try")
                 "reply contains link to /try editor")

        ;; Verify the invalid message did NOT produce an Ingest event
        (bind events2 (->> (slurp event-log) str/split-lines (remove str/blank?) (mapv #(json/parse-string % true))))
        (bind ingest-events (filterv #(= "ingest" (:type %)) events2))
        (assert! (= 1 (count ingest-events))
                 (str "still only 1 Ingest event (invalid msg not ingested, got " (count ingest-events) ")"))

        ;; 6. Phase 3: replay — return both messages, bot should skip both (dedup)
        (println "\nswitching to replay phase (dedup test)\u2026")
        (reset! phase :replay)
        (Thread/sleep 5000)
        (bind events3 (->> (slurp event-log) str/split-lines (remove str/blank?)))
        (bind signal-msg-count (count (filterv #(= "signal_message" (:type (json/parse-string % true))) events3)))
        (assert! (<= signal-msg-count 2)
                 (str "at most 2 SignalMessage events total (dedup working, got " signal-msg-count ")"))
        (bind ingest-count (count (filterv #(= "ingest" (:type (json/parse-string % true))) events3)))
        (assert! (= 1 ingest-count)
                 (str "still only 1 Ingest event after replay (got " ingest-count ")"))

        ;; 7. Restart server — verify events survive replay
        (println "\nkilling server for restart test\u2026")
        (.destroyForcibly (:proc server))
        (deref server)
        (reset! !server nil)

        ;; Switch to empty phase so no new messages on restart
        (reset! phase :empty)

        (println "restarting server\u2026")
        (bind server2 (p/process [server-bin] {:out :inherit :err :inherit :env server-env}))
        (reset! !server2 server2)
        (assert! (wait-for-server base-url 10000) "restarted server responds to /healthz")

        ;; Verify rankings survived restart (replay determinism)
        (bind replay-result (run-cli cli-bin base-url ["garden" "children" "signal" "--json"]))
        (assert! (zero? (:exit replay-result)) "garden children signal exits 0 after restart")
        (bind replay-resp (json/parse-string (:out replay-result) true))
        (bind replay-ranked (mapv :item (mapcat :ranking (:components replay-resp))))
        (assert! (= 2 (count replay-ranked)) "2 items ranked after replay")
        (assert! (str/ends-with? (first replay-ranked) "signal/alpha")
                 (str "alpha still #1 after replay (got " (first replay-ranked) ")")))

      (finally
        (when-some [s @!server]
          (println "\nkilling server\u2026")
          (.destroyForcibly (:proc s))
          (deref s))
        (when-some [s @!server2]
          (println "\nkilling restarted server\u2026")
          (.destroyForcibly (:proc s))
          (deref s))
        (when-some [m @!mock]
          (println "stopping mock signal-cli-api\u2026")
          ((:stop-fn m)))
        (fs/delete-tree tmp-dir)))

    (bind {pass :pass} @counts)
    (println (str "\n" ansi-green "\u2501\u2501\u2501 " pass " signal bot checks passed \u2501\u2501\u2501" ansi-reset "\n"))))
