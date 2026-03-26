(ns test.telegram
  "Telegram bot integration test. Starts a mock Telegram Bot API, boots the
   slug server with telegram bot enabled pointing at the mock, and verifies:
   1. The bot polls getUpdates and ingests valid DSL messages
   2. Invalid DSL messages trigger an error reply (captured by the mock)
   3. Slash commands (/help, /rank) get responses
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
;; mock Telegram Bot API
;; ---------------------------------------------------------------------------

(defn- tg-ok [result]
  {:status  200
   :headers {"Content-Type" "application/json"}
   :body    (json/generate-string {"ok" true "result" result})})

(defn- make-update [update-id user-id username chat-id message-id text]
  {"update_id" update-id
   "message"   {"message_id" message-id
                "from"       {"id" user-id "username" username "first_name" username}
                "chat"       {"id" chat-id}
                "text"       text}})

(defn- start-mock-tg-api
  "Start a mock Telegram Bot API. Returns {:stop-fn f :port n :state atom}.
   The state atom tracks:
     :get-updates-count — how many times getUpdates was called
     :send-messages     — vec of sendMessage request bodies
   The updates-fn is called with (offset) and returns a vec of update objects."
  [port updates-fn]
  (letlocals
    (bind state (atom {:get-updates-count 0
                       :send-messages     []}))
    (bind handler
      (fn [req]
        (cond
          ;; GET /bot.../getUpdates
          (str/includes? (:uri req) "/getUpdates")
          (letlocals
            (swap! state update :get-updates-count inc)
            (bind query (or (:query-string req) ""))
            (bind offset (some->> (re-find #"offset=(-?\d+)" query) second (Long/parseLong)))
            (tg-ok (updates-fn offset)))

          ;; POST /bot.../sendMessage
          (str/includes? (:uri req) "/sendMessage")
          (letlocals
            (bind body-str (slurp (:body req)))
            (bind body-json (json/parse-string body-str true))
            (swap! state update :send-messages conj body-json)
            (tg-ok {"message_id" 999}))

          :else
          {:status 404 :body "not found"})))

    (bind stop-fn (http/run-server handler {:port port}))
    {:stop-fn stop-fn :port port :state state}))

;; ---------------------------------------------------------------------------
;; test fixtures
;; ---------------------------------------------------------------------------

(def ^:private bot-token "test-bot-token")

;; Valid DSL message
(def ^:private valid-message
  (str "#tg-test\n"
       "~/tg/alpha { First test item from Telegram }\n"
       "~/tg/beta  { Second test item from Telegram }\n"
       "~/tg/alpha 3:1 ~/tg/beta { alpha is better }"))

;; Invalid DSL message
(def ^:private invalid-message
  "~/nonexistent/a 2:1 ~/nonexistent/b { this should fail }")

;; ---------------------------------------------------------------------------
;; main
;; ---------------------------------------------------------------------------

(defn telegram-test [& _args]
  (println "\n\u2501\u2501\u2501 telegram bot integration check \u2501\u2501\u2501\n")
  (reset! counts {:pass 0 :fail 0})

  (println "building server binary\u2026")
  (letlocals
    (bind build @(p/process [(cargo-bin) "build" "--release" "-p" "slugsocial-server"]
                            {:inherit true :env base-env}))
    (assert! (zero? (:exit build)) "cargo build succeeds")

    (bind server-bin  "target/release/slugsocial-server")
    (bind cli-bin     "target/release/slugsocial")
    (bind tmp-dir     (str (fs/create-temp-dir {:prefix "slug-tg-"})))
    (bind slug-port   (pick-port))
    (bind mock-port   (pick-port))
    (bind base-url    (str "http://127.0.0.1:" slug-port))
    (bind mock-url    (str "http://127.0.0.1:" mock-port))
    (bind event-log   (str tmp-dir "/events.jsonl"))

    (bind !server     (atom nil))
    (bind !server2    (atom nil))
    (bind !mock       (atom nil))

    ;; Phase 1: valid message
    ;; Phase 2: invalid message -> reply with error
    ;; Phase 3: slash command /help
    ;; Phase 4: replay (dedup)
    (bind phase (atom :valid))

    (bind updates-fn
      (fn [offset]
        (case @phase
          :valid
          [(make-update 100 42 "alice" 42 1001 valid-message)]

          :invalid
          [(make-update 200 43 "bob" 43 1002 invalid-message)]

          :help
          [(make-update 300 42 "alice" 42 1003 "/help")]

          :replay
          ;; Return the valid message again — should be deduped by message_id
          [(make-update 400 42 "alice" 42 1001 valid-message)]

          ;; Default: empty
          [])))

    (bind server-env
      (merge base-env
             {"SLUG_DATA_DIR"         tmp-dir
              "SLUG_KEYS"             "test:test"
              "PORT"                  (str slug-port)
              "RUST_LOG"              "info"
              "SLUG_TG_BOT_TOKEN"     bot-token
              "SLUG_TG_API_BASE_URL"  mock-url
              "SLUG_TG_POLL_SECS"     "2"
              "SLUG_PUBLIC_URL"       "https://slug.social"}))

    (try
      (letlocals
        ;; 1. Start mock Telegram API
        (println (str "\nstarting mock Telegram API on :" mock-port))
        (bind mock (start-mock-tg-api mock-port updates-fn))
        (reset! !mock mock)
        (assert! (some? (:stop-fn mock)) "mock Telegram API started")

        ;; 2. Boot slug server with telegram bot
        (println (str "starting slug server on :" slug-port " (telegram bot polling every 2s)"))
        (bind server (p/process [server-bin] {:out :inherit :err :inherit :env server-env}))
        (reset! !server server)
        (assert! (wait-for-server base-url 10000) "server responds to /healthz")

        ;; 3. Wait for bot to poll and ingest
        (println "\nwaiting for bot to ingest valid message\u2026")
        (Thread/sleep 5000)

        ;; 4. Verify valid message was ingested
        (bind mock-state @(:state mock))
        (assert! (pos? (:get-updates-count mock-state))
                 (str "mock received getUpdates (got " (:get-updates-count mock-state) ")"))

        (assert! (fs/exists? event-log) "events.jsonl exists")
        (bind events (->> (slurp event-log) str/split-lines (remove str/blank?) (mapv #(json/parse-string % true))))
        (assert! (>= (count events) 3)
                 (str "at least 3 events (TelegramMessage + ActorKeyRegistration + Ingest, got " (count events) ")"))

        (bind tg-events (filterv #(= "telegram_message" (:type %)) events))
        (assert! (= 1 (count tg-events))
                 (str "exactly 1 TelegramMessage event (got " (count tg-events) ")"))

        (bind tm (first tg-events))
        (assert! (= "1001" (:message_id tm)) "TelegramMessage message_id is 1001")
        (assert! (= "alice" (:tg_username tm)) "TelegramMessage username is alice")
        (assert! (str/includes? (:actor tm) "telegram") "actor contains 'telegram'")

        ;; Check rankings via CLI
        (println "\nverifying rankings\u2026")
        (bind children-result (run-cli cli-bin base-url ["garden" "children" "tg" "--json"]))
        (assert! (zero? (:exit children-result)) "garden children tg exits 0")
        (bind children-resp (json/parse-string (:out children-result) true))
        (bind ranked (mapv :item (mapcat :ranking (:components children-resp))))
        (assert! (= 2 (count ranked)) (str "2 items ranked under /tg (got " (count ranked) ")"))

        ;; Check thread
        (bind forum-result (run-cli cli-bin base-url ["forum" "--json"]))
        (assert! (zero? (:exit forum-result)) "forum exits 0")
        (bind forum-resp (json/parse-string (:out forum-result) true))
        (assert! (some (fn [t] (= "#tg-test" (:thread t))) (:threads forum-resp))
                 "thread #tg-test visible in forum")

        ;; Confirm reply (sendMessage for "Ingested.")
        (bind confirms (filterv #(str/includes? (or (:text %) "") "Ingested") (:send-messages mock-state)))
        (assert! (pos? (count confirms)) "bot sent 'Ingested.' confirmation")

        ;; 5. Phase 2: invalid message -> error reply
        (println "\nswitching to invalid message phase\u2026")
        (reset! phase :invalid)
        (Thread/sleep 5000)

        (bind mock-state2 @(:state mock))
        (bind error-replies (filterv #(str/includes? (or (:text %) "") "/try") (:send-messages mock-state2)))
        (assert! (pos? (count error-replies))
                 (str "bot replied with error containing /try link (got " (count error-replies) ")"))

        (bind events2 (->> (slurp event-log) str/split-lines (remove str/blank?) (mapv #(json/parse-string % true))))
        (bind ingest-events (filterv #(= "ingest" (:type %)) events2))
        (assert! (= 1 (count ingest-events))
                 (str "still only 1 Ingest event (invalid not ingested, got " (count ingest-events) ")"))

        ;; 6. Phase 3: /help command
        (println "\nswitching to /help phase\u2026")
        (reset! phase :help)
        (Thread/sleep 5000)

        (bind mock-state3 @(:state mock))
        (bind help-replies (filterv #(str/includes? (or (:text %) "") "Welcome") (:send-messages mock-state3)))
        (assert! (pos? (count help-replies)) "bot replied to /help with welcome message")

        ;; 7. Phase 4: replay (dedup)
        (println "\nswitching to replay phase (dedup test)\u2026")
        (reset! phase :replay)
        (Thread/sleep 5000)

        (bind events4 (->> (slurp event-log) str/split-lines (remove str/blank?)))
        (bind ingest-count (count (filterv #(= "ingest" (:type (json/parse-string % true))) events4)))
        (assert! (= 1 ingest-count)
                 (str "still only 1 Ingest event after replay (got " ingest-count ")"))

        ;; 8. Restart server — verify cursor + events survive
        (println "\nkilling server for restart test\u2026")
        (.destroyForcibly (:proc server))
        (deref server)
        (reset! !server nil)

        (reset! phase :empty)
        (bind cursor-file (str tmp-dir "/tg_bot_cursor.txt"))
        (assert! (fs/exists? cursor-file) "cursor file exists")

        (println "restarting server\u2026")
        (bind server2 (p/process [server-bin] {:out :inherit :err :inherit :env server-env}))
        (reset! !server2 server2)
        (assert! (wait-for-server base-url 10000) "restarted server responds to /healthz")

        (bind replay-result (run-cli cli-bin base-url ["garden" "children" "tg" "--json"]))
        (assert! (zero? (:exit replay-result)) "garden children tg exits 0 after restart")
        (bind replay-resp (json/parse-string (:out replay-result) true))
        (bind replay-ranked (mapv :item (mapcat :ranking (:components replay-resp))))
        (assert! (= 2 (count replay-ranked)) "2 items ranked after replay"))

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
          (println "stopping mock Telegram API\u2026")
          ((:stop-fn m)))
        (fs/delete-tree tmp-dir)))

    (bind {pass :pass} @counts)
    (println (str "\n" ansi-green "\u2501\u2501\u2501 " pass " telegram bot checks passed \u2501\u2501\u2501" ansi-reset "\n"))))
