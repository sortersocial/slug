(ns test.grants
  "Grant enforcement integration test: private room capabilities via POST /api/v0/rpc.

  Covers:
  - user without any grant -> RPC line ok=false on private room post
  - user with View but no Post -> ok=false for prose
  - user with View + Post -> ok=true for prose
  - user with View + Post but no Vote -> ok=false for vote
  - user with View + Post + Vote -> ok=true for vote"
  (:require [babashka.process :as p]
            [babashka.fs :as fs]
            [cheshire.core :as json]
            [org.httpkit.server :as http]
            [test.common :as common]))

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

(defn- http-client []
  (-> (java.net.http.HttpClient/newBuilder)
      (.followRedirects java.net.http.HttpClient$Redirect/ALWAYS)
      (.connectTimeout (java.time.Duration/ofSeconds 15))
      (.build)))

(defn- http-get [url & {:keys [headers]}]
  (let [b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b (.timeout (java.time.Duration/ofSeconds 60)) (.GET) (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp)})))

(defn- http-post-json [url data & {:keys [headers]}]
  (let [body (json/generate-string data)
        b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (.header b "Content-Type" "application/json")
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b
                  (.timeout (java.time.Duration/ofSeconds 60))
                  (.POST (java.net.http.HttpRequest$BodyPublishers/ofString body))
                  (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp)})))

(defn- http-post-form [url form]
  (let [pairs (->> form
                   (map (fn [[k v]]
                          (str (java.net.URLEncoder/encode (name k) "UTF-8")
                               "="
                               (java.net.URLEncoder/encode (str v) "UTF-8"))))
                   (clojure.string/join "&"))
        b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (.header b "Content-Type" "application/x-www-form-urlencoded")
    (let [req (-> b
                  (.timeout (java.time.Duration/ofSeconds 60))
                  (.POST (java.net.http.HttpRequest$BodyPublishers/ofString pairs))
                  (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp)})))

(defn- parse-query [s]
  (into {}
        (for [part (clojure.string/split (or s "") #"&")
              :when (not (clojure.string/blank? part))]
          (let [[k v] (clojure.string/split part #"=" 2)]
            [(keyword (java.net.URLDecoder/decode k "UTF-8"))
             (some-> v (java.net.URLDecoder/decode "UTF-8"))]))))

(defn- b64url [s]
  (-> (java.util.Base64/getUrlEncoder)
      (.withoutPadding)
      (.encodeToString (.getBytes s "UTF-8"))))

(defn- make-id-token [sub]
  (str (b64url "{\"alg\":\"RS256\",\"typ\":\"JWT\"}")
       "."
       (b64url (json/generate-string {:sub sub}))
       ".fakesig"))

;; Stateful mock: each /token call returns the next user in the list.
;; This lets us register two distinct users in one test run.
(defn- start-mock-google [port]
  (let [google-users ["google-user-alice" "google-user-bob"]
        !call-count (atom 0)
        handler
        (fn [req]
          (cond
            (and (= :get (:request-method req))
                 (= "/o/oauth2/v2/auth" (:uri req)))
            (let [q (parse-query (:query-string req))
                  redirect-uri (:redirect_uri q)
                  state (:state q)
                  loc (str redirect-uri "?code=mockcode&state=" state)]
              {:status 302 :headers {"Location" loc} :body ""})

            (and (= :post (:request-method req))
                 (= "/token" (:uri req)))
            (let [n   (swap! !call-count inc)
                  sub (nth google-users (mod (dec n) (count google-users)))]
              {:status 200
               :headers {"Content-Type" "application/json"}
               :body (json/generate-string {:id_token (make-id-token sub)})})

            :else
            {:status 404 :body "not found"}))
        stop-fn (http/run-server handler {:port port})]
    {:stop-fn stop-fn :port port}))

(defn- register-user
  "Walk the full OAuth flow for one user. Returns the bearer token."
  [base-url session-agent username]
  (let [start-resp (http-post-json (str base-url "/api/v0/pending-session")
                                   {:agent session-agent})
        _ (assert! (= 200 (:status start-resp))
                   (str "pending-session for " username " returns 200"))
        start-json (json/parse-string (:body start-resp) true)
        _login (http-get (:login_url start-json))
        _choose (http-post-form (str base-url "/auth/choose-username")
                                {:session (:session start-json) :username username})
        _ (assert! (= 200 (:status _choose))
                   (str "choose-username for " username " returns 200"))
        poll (http-get (str base-url "/api/v0/pending-session/" (:session start-json)))
        poll-json (json/parse-string (:body poll) true)
        _ (assert! (:complete poll-json)
                   (str "pending session complete for " username))]
    (:token poll-json)))

(defn- bearer [token] {"Authorization" (str "Bearer " token)})

(defn- rpc-batch! [base-url token cmds]
  (let [resp (http-post-json (str base-url "/api/v0/rpc") cmds :headers (bearer token))]
    {:status (:status resp)
     :parsed (json/parse-string (:body resp) false)}))

(defn- rpc-line-ok? [parsed]
  (true? (get-in parsed ["results" 0 "ok"])))

(defn- ingest! [base-url token room thread delegate text]
  (rpc-batch! base-url token
              [{"Post" {"room" room
                        "thread_tag" thread
                        "delegate" delegate
                        "text" text
                        "return_rank_diff" false}}]))

(defn grants-test [& _args]
  (println "\n━━━ grants enforcement integration check ━━━\n")
  (reset! counts {:pass 0 :fail 0})

  (println "building server binary…")
  (common/letlocals
   (bind build @(p/process [(common/cargo-bin) "build" "--release" "-p" "slugsocial-server"]
                           {:inherit true :env common/base-env}))
   (assert! (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-grants-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))

   (bind !server (atom nil))
   (bind !google (atom nil))

   (bind server-env (merge common/base-env
                           {"SLUG_DATA_DIR" tmp-dir
                            "SLUG_KEYS"     "test:test"
                            "PORT"          (str slug-port)
                            "RUST_LOG"      "warn"
                            "SLUG_PUBLIC_URL" base-url
                            "SLUG_GOOGLE_AUTH_URL" (str google-url "/o/oauth2/v2/auth")
                            "SLUG_GOOGLE_TOKEN_URL" (str google-url "/token")
                            "SLUG_GOOGLE_CLIENT_ID" "mock"
                            "SLUG_GOOGLE_CLIENT_SECRET" "mock"}))
   (try
     (println (str "starting mock google on :" google-port))
     (reset! !google (start-mock-google google-port))

     (println (str "starting server on :" slug-port))
     (reset! !server (common/start-server server-bin server-env))
     (assert! (common/wait-for-server base-url 10000) "server responds to /healthz")

     ;; Register two users. The mock google cycles through google-user-alice then google-user-bob.
     (println "\nregistering alice…")
     (let [alice-token (register-user base-url
                                      "00000000-0000-0000-0000-000000000001:test:local/dev"
                                      "alice")
           _ (println "registering bob…")
           bob-token   (register-user base-url
                                      "00000000-0000-0000-0000-000000000002:test:local/dev"
                                      "bob")

           ;; Alice creates a private room.
           _ (println "\nalice creates private room…")
           create (rpc-batch! base-url alice-token
                              [{"RoomCreate" {"slug" "secret-project" "visibility" "private"}}])
           _ (assert! (= 200 (:status create)) "room create HTTP 200")
           _ (assert! (rpc-line-ok? (:parsed create)) "room create RPC ok")
           room-id (get-in (:parsed create) ["results" 0 "result" "RoomCreated" "room_id"])
           _ (assert! (some? room-id) "room_id present in RPC result")]

       ;; Alice (owner) can post prose to her own private room.
       (println "\nalice posts prose to her private room…")
       (assert! (rpc-line-ok? (:parsed (ingest! base-url alice-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000001:test:local/dev"
                                                 "Hello from alice.")))
                "alice prose post succeeds")

       ;; Bob has no grants — RPC line fails.
       (println "\nbob (no grants) tries to post prose…")
       (assert! (not (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                      "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                      "Hello from bob, unauthorized."))))
                "bob without grants gets RPC failure")

       ;; Alice grants bob View only.
       (println "\nalice grants bob View only…")
       (assert! (rpc-line-ok? (:parsed (rpc-batch! base-url alice-token
                                                   [{"RoomGrant" {"room" room-id "username" "bob" "capability" "view"}}])))
                "grant View RPC ok")

       (println "\nbob (View only) tries to post prose…")
       (assert! (not (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                      "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                      "Hello from bob, view only."))))
                "bob with View but no Post gets RPC failure")

       ;; Alice grants bob Post.
       (println "\nalice grants bob Post…")
       (assert! (rpc-line-ok? (:parsed (rpc-batch! base-url alice-token
                                                   [{"RoomGrant" {"room" room-id "username" "bob" "capability" "post"}}])))
                "grant Post RPC ok")

       (println "\nbob (View + Post) posts prose…")
       (assert! (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                 "Hello from bob, now authorised.")))
                "bob with View + Post succeeds for prose")

       ;; Alice defines two items and votes in the private room.
       (println "\nalice posts items + vote to private room…")
       (assert! (rpc-line-ok? (:parsed (ingest! base-url alice-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000001:test:local/dev"
                                                 "~/fruits/apple { A crisp red apple. }\n~/fruits/banana { A yellow banana. }\n~/fruits/apple > ~/fruits/banana { apples are better }")))
                "alice vote in private room succeeds")

       ;; Bob (View + Post, no Vote) tries to vote.
       (println "\nbob (no Vote) tries to vote…")
       (assert! (not (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                     "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                     "~/fruits/apple > ~/fruits/banana { bob's take }"))))
                "bob without Vote gets RPC failure")

       ;; Alice grants bob Vote.
       (println "\nalice grants bob Vote…")
       (assert! (rpc-line-ok? (:parsed (rpc-batch! base-url alice-token
                                                   [{"RoomGrant" {"room" room-id "username" "bob" "capability" "vote"}}])))
                "grant Vote RPC ok")

       (println "\nbob (View + Post + Vote) votes…")
       (assert! (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                 "~/fruits/apple > ~/fruits/banana { bob's take }")))
                "bob with Vote succeeds"))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass fail :fail} @counts)
   (if (zero? fail)
     (println (str "\n" ansi-green "━━━ " pass " grant checks passed ━━━" ansi-reset "\n"))
     (do (println (str "\n" ansi-red "━━━ " fail " grant checks FAILED ━━━" ansi-reset "\n"))
         (System/exit 1)))))
