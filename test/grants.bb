(ns test.grants
  "Grant enforcement integration test: private thread access control on POST /api/v0/ingest.

  Covers:
  - user without any grant -> 403 on private thread
  - user with View but no Post -> 403 for prose
  - user with View + Post -> 200 for prose
  - user with View + Post but no Vote -> 403 for vote (requires items in scope)
  - user with View + Post + Vote -> 200 for vote"
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
      (.build)))

(defn- http-get [url & {:keys [headers]}]
  (let [b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b (.GET) (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp)})))

(defn- http-post-json [url data & {:keys [headers]}]
  (let [body (json/generate-string data)
        b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (.header b "Content-Type" "application/json")
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b
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

;; Stateful mock: each userinfo call returns the next user in the list.
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
            {:status 200
             :headers {"Content-Type" "application/json"}
             :body (json/generate-string {:access_token "mock_access_token"})}

            (and (= :get (:request-method req))
                 (= "/userinfo" (:uri req)))
            (let [idx (mod (swap! !call-count inc) (count google-users))
                  sub (nth google-users (dec idx))]
              {:status 200
               :headers {"Content-Type" "application/json"}
               :body (json/generate-string {:sub sub})})

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

(defn- ingest! [base-url token thread delegate text]
  (http-post-json (str base-url "/api/v0/ingest")
                  {:thread thread :delegate delegate :text text}
                  :headers (bearer token)))

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
                            "SLUG_GOOGLE_BASE_URL" google-url
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
     (bind alice-token (register-user base-url
                                      "@@00000000-0000-0000-0000-000000000001:test:local/dev"
                                      "alice"))
     (println "registering bob…")
     (bind bob-token   (register-user base-url
                                      "@@00000000-0000-0000-0000-000000000002:test:local/dev"
                                      "bob"))

     ;; Alice creates a private thread.
     (println "\nalice creates private thread…")
     (bind create-resp (http-post-json (str base-url "/api/v0/thread")
                                       {:slug "secret-project" :visibility "private"}
                                       :headers (bearer alice-token)))
     (assert! (= 200 (:status create-resp)) "thread create returns 200")
     (bind create-json (json/parse-string (:body create-resp) true))
     (bind thread-id (:thread_id create-json))
     (assert! (some? thread-id) "thread_id present in response")

     ;; Alice (owner) can post prose to her own private thread.
     (println "\nalice posts prose to her private thread…")
     (bind alice-prose (ingest! base-url alice-token thread-id
                                "@@00000000-0000-0000-0000-000000000001:test:local/dev"
                                "Hello from alice."))
     (assert! (= 200 (:status alice-prose)) "alice prose post succeeds")

     ;; Bob has no grants at all — should get 403.
     (println "\nbob (no grants) tries to post prose…")
     (bind bob-no-grant (ingest! base-url bob-token thread-id
                                 "@@00000000-0000-0000-0000-000000000002:test:local/dev"
                                 "Hello from bob, unauthorized."))
     (assert! (= 403 (:status bob-no-grant)) "bob without grants gets 403")

     ;; Alice grants bob View only — still not enough to post prose.
     (println "\nalice grants bob View only…")
     (bind grant-view (http-post-json (str base-url "/api/v0/thread/" thread-id "/grants")
                                      {:username "bob" :capabilities ["view"]}
                                      :headers (bearer alice-token)))
     (assert! (= 200 (:status grant-view)) "grant View returns 200")

     (println "\nbob (View only) tries to post prose…")
     (bind bob-view-only (ingest! base-url bob-token thread-id
                                  "@@00000000-0000-0000-0000-000000000002:test:local/dev"
                                  "Hello from bob, view only."))
     (assert! (= 403 (:status bob-view-only)) "bob with View but no Post gets 403")

     ;; Alice grants bob Post — now prose should work.
     (println "\nalice grants bob Post…")
     (bind grant-post (http-post-json (str base-url "/api/v0/thread/" thread-id "/grants")
                                      {:username "bob" :capabilities ["post"]}
                                      :headers (bearer alice-token)))
     (assert! (= 200 (:status grant-post)) "grant Post returns 200")

     (println "\nbob (View + Post) posts prose…")
     (bind bob-prose (ingest! base-url bob-token thread-id
                              "@@00000000-0000-0000-0000-000000000002:test:local/dev"
                              "Hello from bob, now authorised."))
     (assert! (= 200 (:status bob-prose)) "bob with View + Post succeeds for prose")

     ;; Alice defines two items and votes on them in the private thread.
     (println "\nalice posts items + vote to private thread…")
     (bind alice-vote (ingest! base-url alice-token thread-id
                               "@@00000000-0000-0000-0000-000000000001:test:local/dev"
                               "~/fruits/apple { A crisp red apple. }\n~/fruits/banana { A yellow banana. }\n~/fruits/apple > ~/fruits/banana { apples are better }"))
     (assert! (= 200 (:status alice-vote)) "alice vote in private thread succeeds")

     ;; Bob (View + Post, no Vote) tries to vote — should be 403.
     (println "\nbob (no Vote) tries to vote…")
     (bind bob-no-vote (ingest! base-url bob-token thread-id
                                "@@00000000-0000-0000-0000-000000000002:test:local/dev"
                                "~/fruits/apple > ~/fruits/banana { bob's take }"))
     (assert! (= 403 (:status bob-no-vote)) "bob without Vote gets 403")

     ;; Alice grants bob Vote — now voting should work.
     (println "\nalice grants bob Vote…")
     (bind grant-vote (http-post-json (str base-url "/api/v0/thread/" thread-id "/grants")
                                      {:username "bob" :capabilities ["vote"]}
                                      :headers (bearer alice-token)))
     (assert! (= 200 (:status grant-vote)) "grant Vote returns 200")

     (println "\nbob (View + Post + Vote) votes…")
     (bind bob-vote (ingest! base-url bob-token thread-id
                             "@@00000000-0000-0000-0000-000000000002:test:local/dev"
                             "~/fruits/apple > ~/fruits/banana { bob's take }"))
     (assert! (= 200 (:status bob-vote)) "bob with Vote succeeds")

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass fail :fail} @counts)
   (if (zero? fail)
     (println (str "\n" ansi-green "━━━ " pass " grant checks passed ━━━" ansi-reset "\n"))
     (do (println (str "\n" ansi-red "━━━ " fail " grant checks FAILED ━━━" ansi-reset "\n"))
         (System/exit 1)))))
