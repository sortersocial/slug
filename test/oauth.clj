(ns test.oauth
  "Shared Babashka HTTP helpers + mock Google OAuth + token handoff for integration tests."
  (:require [cheshire.core :as json]
            [clojure.string :as str]
            [org.httpkit.server :as http]))

(def ^:private connect-timeout (java.time.Duration/ofSeconds 15))
(def ^:private request-timeout (java.time.Duration/ofSeconds 60))

(defn http-client []
  (-> (java.net.http.HttpClient/newBuilder)
      (.followRedirects java.net.http.HttpClient$Redirect/ALWAYS)
      (.connectTimeout connect-timeout)
      (.build)))

(defn http-get [url & {:keys [headers]}]
  (let [b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b (.timeout request-timeout) (.GET) (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

(defn http-get-no-redirect
  "GET without following redirects; returns `:location` from the first `Location` header when present."
  [url & {:keys [headers]}]
  (let [client (-> (java.net.http.HttpClient/newBuilder)
                   (.followRedirects java.net.http.HttpClient$Redirect/NEVER)
                   (.connectTimeout connect-timeout)
                   (.build))
        b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b (.timeout request-timeout) (.GET) (.build))
          resp (.send client req (java.net.http.HttpResponse$BodyHandlers/ofString))
          loc (first (get (.map (.headers resp)) "location"))]
      {:status (.statusCode resp) :body (.body resp) :location loc})))

(defn http-post-json [url data & {:keys [headers]}]
  (let [body (json/generate-string data)
        b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (.header b "Content-Type" "application/json")
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b
                  (.timeout request-timeout)
                  (.POST (java.net.http.HttpRequest$BodyPublishers/ofString body))
                  (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

(defn http-post-form [url form & {:keys [headers]}]
  (let [pairs (->> form
                   (map (fn [[k v]]
                          (str (java.net.URLEncoder/encode (name k) "UTF-8")
                               "="
                               (java.net.URLEncoder/encode (str v) "UTF-8"))))
                   (str/join "&"))
        b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (.header b "Content-Type" "application/x-www-form-urlencoded")
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b
                  (.timeout request-timeout)
                  (.POST (java.net.http.HttpRequest$BodyPublishers/ofString pairs))
                  (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

(defn complete-pending-session!
  "Finish OAuth for an existing pending session id (e.g. created by `GET /join/inv_…`). Returns bearer token."
  [base-url session-id username & {:keys [assert!]}]
  (let [check! (fn [pred msg resp]
                 (if assert!
                   (assert! pred msg)
                   (when-not pred
                     (throw (ex-info msg {:resp resp})))))]
    (let [enc (java.net.URLEncoder/encode session-id "UTF-8")
          login-url (str base-url "/auth/login?session=" enc)
          login-get (http-get login-url)]
      (check! (= 200 (:status login-get))
              (str "oauth redirect chain for session " session-id)
              login-get)
      (let [choose (http-post-form (str base-url "/auth/choose-username")
                                   {:session session-id :username username})]
        (check! (= 200 (:status choose))
                (str "choose-username for " username " returns 200")
                choose)
        (let [poll (http-get (str base-url "/api/v0/pending-session/" session-id))]
          (check! (= 200 (:status poll))
                  (str "pending-session poll returns 200")
                  poll)
          (let [poll-json (json/parse-string (:body poll) true)]
            (check! (:complete poll-json)
                    (str "pending session complete for " username)
                    poll)
            (:token poll-json)))))))

(defn parse-query [s]
  (into {}
        (for [part (str/split (or s "") #"&")
              :when (not (str/blank? part))]
          (let [[k v] (str/split part #"=" 2)]
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

(defn start-mock-google
  "Minimal Google OAuth stub. With multiple `google-users`, each `/token` call returns the next
   `sub` in order (wraps), so several registrations get distinct Google identities."
  [port & {:keys [google-users] :or {google-users ["google-user-1"]}}]
  (let [users (vec google-users)
        multi? (> (count users) 1)
        !call-count (when multi? (atom 0))
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
            (let [sub (if multi?
                        (let [n (swap! !call-count inc)]
                          (nth users (mod (dec n) (count users))))
                        (first users))]
              {:status 200
               :headers {"Content-Type" "application/json"}
               :body (json/generate-string {:id_token (make-id-token sub)})})

            :else
            {:status 404 :body "not found"}))
        stop-fn (http/run-server handler {:port port})]
    {:stop-fn stop-fn :port port}))

(def ^:private default-agent "00000000-0000-0000-0000-000000000000:cli:local/dev")

(defn complete-registration!
  "Run pending-session → login redirect → choose-username → poll; return bearer token string.
   With `assert!`, use `(fn [pred msg] …)` for test-style checks; without it, throw `ex-info` on failure."
  [base-url & {:keys [username agent assert!]
               :or {username "user" agent default-agent}}]
  (let [check! (fn [pred msg resp]
                 (if assert!
                   (assert! pred msg)
                   (when-not pred
                     (throw (ex-info msg {:resp resp})))))]
    (let [start-resp (http-post-json (str base-url "/api/v0/pending-session")
                                     {:agent agent})]
      (check! (= 200 (:status start-resp))
              (str "pending-session for " username " returns 200")
              start-resp)
      (let [start-json (json/parse-string (:body start-resp) true)
            login-get (http-get (:login_url start-json))]
        (check! (= 200 (:status login-get))
                (str "login redirect chain for " username)
                login-get)
        (let [choose (http-post-form (str base-url "/auth/choose-username")
                                     {:session (:session start-json) :username username})]
          (check! (= 200 (:status choose))
                  (str "choose-username for " username " returns 200")
                  choose)
          (let [poll (http-get (str base-url "/api/v0/pending-session/" (:session start-json)))]
            (check! (= 200 (:status poll))
                    (str "pending-session poll for " username " returns 200")
                    poll)
            (let [poll-json (json/parse-string (:body poll) true)]
              (check! (:complete poll-json)
                      (str "pending session complete for " username)
                      poll)
              (:token poll-json))))))))

(defn fetch-bearer-token!
  "Simulate CLI identity OAuth + username choice; returns `slug_…` bearer token.
   Pass `:agent` (default local/dev) when the test will CLI-ingest with `--delegate`
   so first write can `AgentBound`. Browser UI posts use no delegate."
  [base-url & {:keys [username agent] :or {username "intuser" agent default-agent}}]
  (let [token (complete-registration! base-url :username username :agent agent)]
    (when-not (str/starts-with? token "slug_")
      (throw (ex-info "bad token in poll response" {:token token})))
    token))
