(ns test.xbot
  "X bot integration test. Starts a mock X API server, boots the slug server
   with bot enabled pointing at the mock, and verifies that:
   1. The bot polls mentions and ingests valid DSL tweets
   2. Invalid DSL tweets trigger an error reply (captured by the mock)
   3. Follower count is stored as provenance in the event log
   4. Duplicate tweets are not re-ingested after server restart"
  (:require [babashka.process :as p]
            [clojure.string :as str]
            [babashka.fs :as fs]
            [cheshire.core :as json]
            [org.httpkit.server :as http]
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
;; mock X API server
;; ---------------------------------------------------------------------------

(defn- make-mention
  "Build a tweet mention object for the mock API."
  [id author-id text]
  {"id" id "text" text "author_id" author-id})

(defn- make-user
  "Build a user object for the mock API includes."
  [id username followers]
  {"id" id "username" username "public_metrics" {"followers_count" followers}})

(defn- mentions-response
  "Build a full X API /2/users/:id/mentions response."
  [tweets users newest-id]
  {"data"     tweets
   "includes" {"users" users}
   "meta"     {"newest_id" newest-id}})

(defn- start-mock-x-api
  "Start a mock X API server. Returns {:stop-fn f :port n :state atom}.
   The state atom tracks:
     :mention-requests  — count of GET /2/users/.../mentions requests
     :reply-requests    — vec of POST /2/tweets bodies (reply attempts)
     :since-ids         — vec of since_id params received
   The mentions-fn is called with (since-id) and should return a response map."
  [port mentions-fn]
  (common/letlocals
   (bind state (atom {:mention-requests 0
                      :reply-requests   []
                      :since-ids        []}))
   (bind handler
         (fn [req]
           (cond
          ;; GET /2/users/:id/mentions
             (and (= :get (:request-method req))
                  (str/includes? (:uri req) "/mentions"))
             (common/letlocals
              (swap! state update :mention-requests inc)
              (bind query   (or (:query-string req) ""))
              (bind since   (some->> (re-find #"since_id=([^&]+)" query) second))
              (swap! state update :since-ids conj since)
              (bind body    (json/generate-string (mentions-fn since)))
              {:status  200
               :headers {"Content-Type" "application/json"}
               :body    body})

          ;; POST /2/tweets (bot reply)
             (and (= :post (:request-method req))
                  (= "/2/tweets" (:uri req)))
             (common/letlocals
              (bind body-str (slurp (:body req)))
              (bind body-json (json/parse-string body-str true))
              (swap! state update :reply-requests conj body-json)
              {:status  201
               :headers {"Content-Type" "application/json"}
               :body    (json/generate-string {"data" {"id" "reply-1" "text" (:text body-json)}})})

          ;; Everything else → 404
             :else
             {:status 404 :body "not found"})))

   (bind stop-fn (http/run-server handler {:port port}))
   {:stop-fn stop-fn :port port :state state}))

;; ---------------------------------------------------------------------------
;; test fixtures
;; ---------------------------------------------------------------------------

(def ^:private bot-user-id "999")
(def ^:private bot-handle  "slugbot")

;; Valid DSL tweet: defines items, votes, thread tag
(def ^:private valid-tweet-text
  (str "@slugbot #xbot-test\n"
       "~/xbot/alpha { First test item from X }\n"
       "~/xbot/beta  { Second test item from X }\n"
       "~/xbot/alpha 3:1 ~/xbot/beta { alpha is better }"))

;; Invalid DSL tweet: vote on undefined items
(def ^:private invalid-tweet-text
  "@slugbot ~/nonexistent/a 2:1 ~/nonexistent/b { this should fail }")

;; ---------------------------------------------------------------------------
;; main
;; ---------------------------------------------------------------------------

(defn xbot-test [& _args]
  (println "\n━━━ xbot integration check ━━━\n")
  (reset! counts {:pass 0 :fail 0})

  (println "building server binary…")
  (common/letlocals
   (bind build @(p/process [(common/cargo-bin) "build" "--release" "-p" "slugsocial-server"]
                           {:inherit true :env common/base-env}))
   (assert! (zero? (:exit build)) "cargo build succeeds")

   (bind server-bin  "target/release/slugsocial-server")
   (bind cli-bin     "target/release/slugsocial")
   (bind tmp-dir     (str (fs/create-temp-dir {:prefix "slug-xbot-"})))
   (bind slug-port   (common/pick-port))
   (bind mock-port   (common/pick-port))
   (bind base-url    (str "http://127.0.0.1:" slug-port))
   (bind mock-url    (str "http://127.0.0.1:" mock-port))
   (bind event-log   (str tmp-dir "/events.jsonl"))

   (bind !server     (atom nil))
   (bind !server2    (atom nil))
   (bind !mock       (atom nil))

    ;; Track which mentions have been served.
    ;; Phase 1: valid tweet (id "t-100")
    ;; Phase 2: invalid tweet (id "t-200") — should trigger reply
    ;; Phase 3: same tweets again — should be deduped
   (bind phase       (atom :valid))
   (bind valid-user  (make-user "u-alice" "alice" 14200))
   (bind bad-user    (make-user "u-bob" "bob" 500))

   (bind mentions-fn
         (fn [since-id]
           (case @phase
             :valid
             (mentions-response
              [(make-mention "t-100" "u-alice" valid-tweet-text)]
              [valid-user]
              "t-100")

             :invalid
             (mentions-response
              [(make-mention "t-200" "u-bob" invalid-tweet-text)]
              [bad-user]
              "t-200")

             :replay
          ;; Return the same tweets — bot should skip them (dedup)
             (mentions-response
              [(make-mention "t-100" "u-alice" valid-tweet-text)
               (make-mention "t-200" "u-bob" invalid-tweet-text)]
              [valid-user bad-user]
              "t-200")

          ;; Default: empty
             {"data" nil "meta" {}})))

   (bind server-env
         (merge common/base-env
                {"SLUG_DATA_DIR"        tmp-dir
                 "SLUG_KEYS"            "test:test"
                 "PORT"                 (str slug-port)
                 "RUST_LOG"             "info"
                 "SLUG_X_BEARER_TOKEN"  "mock-token"
                 "SLUG_X_BOT_USER_ID"  bot-user-id
                 "SLUG_X_BOT_HANDLE"   bot-handle
                 "SLUG_X_API_BASE_URL" mock-url
                 "SLUG_X_POLL_SECS"    "2"
                 "SLUG_X_WRITE_TOKEN"  "mock-write-token"
                 "SLUG_PUBLIC_URL"     "https://slug.social"}))

   (try
     (common/letlocals
        ;; 1. Start mock X API
      (println (str "\nstarting mock X API on :" mock-port))
      (bind mock (start-mock-x-api mock-port mentions-fn))
      (reset! !mock mock)
      (assert! (some? (:stop-fn mock)) "mock X API started")

        ;; 2. Boot slug server with bot enabled
      (println (str "starting slug server on :" slug-port " (bot polling every 2s)"))
      (bind server (common/start-server server-bin server-env))
      (reset! !server server)
      (assert! (common/wait-for-server base-url 10000) "server responds to /healthz")

        ;; 3. Wait for bot to poll and ingest the valid tweet
      (println "\nwaiting for bot to ingest valid mention…")
      (Thread/sleep 5000)

        ;; 4. Verify the valid tweet was ingested
      (bind mock-state @(:state mock))
      (assert! (pos? (:mention-requests mock-state))
               (str "mock received mention requests (got " (:mention-requests mock-state) ")"))

        ;; Check event log for XMention + ActorKeyRegistration + Ingest
      (assert! (fs/exists? event-log) "events.jsonl exists")
      (bind events (->> (slurp event-log) str/split-lines (remove str/blank?) (mapv #(json/parse-string % true))))
      (assert! (>= (count events) 3)
               (str "at least 3 events (XMention + ActorKeyRegistration + Ingest, got " (count events) ")"))

      (bind x-mention-events (filterv #(= "x_mention" (:type %)) events))
      (assert! (= 1 (count x-mention-events))
               (str "exactly 1 XMention event (got " (count x-mention-events) ")"))

      (bind xm (first x-mention-events))
      (assert! (= "t-100" (:tweet_id xm)) "XMention tweet_id is t-100")
      (assert! (= "alice" (:x_handle xm)) "XMention x_handle is alice")
      (assert! (= 14200 (:followers xm)) "XMention followers is 14200")
      (assert! (str/includes? (:actor xm) "x.com:alice") "actor contains x.com:alice")

        ;; Check rankings via CLI
      (println "\nverifying rankings from bot-ingested tweet…")
      (bind children-result (common/run-cli cli-bin base-url ["garden" "children" "xbot" "--json"]))
      (assert! (zero? (:exit children-result)) "garden children xbot exits 0")
      (bind children-resp (json/parse-string (:out children-result) true))
      (bind ranked (mapv :item (mapcat :ranking (:components children-resp))))
      (assert! (= 2 (count ranked)) (str "2 items ranked under /xbot (got " (count ranked) ")"))
      (assert! (str/ends-with? (first ranked) "xbot/alpha")
               (str "alpha is #1 (got " (first ranked) ")"))

        ;; Check thread was created
      (bind forum-result (common/run-cli cli-bin base-url ["forum" "--json"]))
      (assert! (zero? (:exit forum-result)) "forum exits 0")
      (bind forum-resp (json/parse-string (:out forum-result) true))
      (assert! (some (fn [t] (= "#xbot-test" (:thread t))) (:threads forum-resp))
               "thread #xbot-test visible in forum")

        ;; 5. Phase 2: invalid tweet → bot should reply with error
      (println "\nswitching to invalid tweet phase…")
      (reset! phase :invalid)
      (Thread/sleep 5000)

      (bind mock-state2 @(:state mock))
      (assert! (pos? (count (:reply-requests mock-state2)))
               (str "bot posted a reply (got " (count (:reply-requests mock-state2)) " replies)"))
      (bind reply (first (:reply-requests mock-state2)))
      (assert! (some? reply) "reply body captured")
      (assert! (= "t-200" (get-in reply [:reply :in_reply_to_tweet_id]))
               "reply is in response to t-200")
      (assert! (str/includes? (or (:text reply) "") "/try")
               "reply contains link to /try editor")

        ;; Verify the invalid tweet did NOT produce an Ingest event
      (bind events2 (->> (slurp event-log) str/split-lines (remove str/blank?) (mapv #(json/parse-string % true))))
      (bind ingest-events (filterv #(= "ingest" (:type %)) events2))
      (assert! (= 1 (count ingest-events))
               (str "still only 1 Ingest event (invalid tweet not ingested, got " (count ingest-events) ")"))

        ;; 6. Phase 3: replay — return both tweets, bot should skip both (dedup)
      (println "\nswitching to replay phase (dedup test)…")
      (reset! phase :replay)
      (bind pre-event-count (count events2))
      (Thread/sleep 5000)
      (bind events3 (->> (slurp event-log) str/split-lines (remove str/blank?)))
        ;; The invalid tweet t-200 gets a new XMention event each time (provenance), but no new Ingest.
        ;; The valid tweet t-100 should be fully skipped (seen_tweet_ids).
      (bind x-mention-count (count (filterv #(= "x_mention" (:type (json/parse-string % true))) events3)))
      (assert! (<= x-mention-count 2)
               (str "at most 2 XMention events total (dedup working, got " x-mention-count ")"))
      (bind ingest-count (count (filterv #(= "ingest" (:type (json/parse-string % true))) events3)))
      (assert! (= 1 ingest-count)
               (str "still only 1 Ingest event after replay (got " ingest-count ")"))

        ;; 7. Restart server — verify cursor persistence
      (println "\nkilling server for restart test…")
      (common/kill-server server)
      (reset! !server nil)

        ;; Switch to empty phase so no new mentions on restart
      (reset! phase :empty)
      (bind cursor-file (str tmp-dir "/x_bot_cursor.txt"))
      (assert! (fs/exists? cursor-file) "cursor file exists")
      (bind cursor-val (str/trim (slurp cursor-file)))
      (assert! (not (str/blank? cursor-val))
               (str "cursor file has value: " cursor-val))

      (println "restarting server…")
      (bind server2 (common/start-server server-bin server-env))
      (reset! !server2 server2)
      (assert! (common/wait-for-server base-url 10000) "restarted server responds to /healthz")

        ;; Verify rankings survived restart (replay determinism)
      (bind replay-result (common/run-cli cli-bin base-url ["garden" "children" "xbot" "--json"]))
      (assert! (zero? (:exit replay-result)) "garden children xbot exits 0 after restart")
      (bind replay-resp (json/parse-string (:out replay-result) true))
      (bind replay-ranked (mapv :item (mapcat :ranking (:components replay-resp))))
      (assert! (= 2 (count replay-ranked)) "2 items ranked after replay")
      (assert! (str/ends-with? (first replay-ranked) "xbot/alpha")
               (str "alpha still #1 after replay (got " (first replay-ranked) ")")))

     (finally
       (when-some [s @!server]
         (println "\nkilling server…")
         (common/kill-server s))
       (when-some [s @!server2]
         (println "\nkilling restarted server…")
         (common/kill-server s))
       (when-some [m @!mock]
         (println "stopping mock X API…")
         ((:stop-fn m)))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass} @counts)
   (println (str "\n" ansi-green "━━━ " pass " xbot checks passed ━━━" ansi-reset "\n"))))
