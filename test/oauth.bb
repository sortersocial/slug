(ns test.oauth
  "Shared Babashka HTTP helpers + mock Google OAuth + token handoff for integration tests."
  (:require [cheshire.core :as json]
            [org.httpkit.server :as http]))

(defn http-client []
  (-> (java.net.http.HttpClient/newBuilder)
      (.followRedirects java.net.http.HttpClient$Redirect/ALWAYS)
      (.build)))

(defn http-get [url & {:keys [headers]}]
  (let [b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b (.GET) (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

(defn http-post-json [url data & {:keys [headers]}]
  (let [body (json/generate-string data)
        b (java.net.http.HttpRequest/newBuilder (java.net.URI/create url))]
    (.header b "Content-Type" "application/json")
    (doseq [[k v] (or headers {})]
      (.header b k v))
    (let [req (-> b
                  (.POST (java.net.http.HttpRequest$BodyPublishers/ofString body))
                  (.build))
          resp (.send (http-client) req (java.net.http.HttpResponse$BodyHandlers/ofString))]
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

(defn http-post-form [url form]
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
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

(defn parse-query [s]
  (into {}
        (for [part (clojure.string/split (or s "") #"&")
              :when (not (clojure.string/blank? part))]
          (let [[k v] (clojure.string/split part #"=" 2)]
            [(keyword (java.net.URLDecoder/decode k "UTF-8"))
             (some-> v (java.net.URLDecoder/decode "UTF-8"))]))))

(defn start-mock-google [port]
  (let [handler
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
            {:status 200
             :headers {"Content-Type" "application/json"}
             :body (json/generate-string {:sub "google-user-1"})}

            :else
            {:status 404 :body "not found"}))
        stop-fn (http/run-server handler {:port port})]
    {:stop-fn stop-fn :port port}))

(def ^:private default-agent "@@00000000-0000-0000-0000-000000000000:cli:local/dev")

(defn fetch-bearer-token!
  "Simulate browser OAuth + username choice; returns `slug_…` bearer token.
   Agent must match CLI default `SLUG_DELEGATE` for ingest binding."
  [base-url & {:keys [username agent] :or {username "intuser" agent default-agent}}]
  (let [start-resp (http-post-json (str base-url "/api/v0/pending-session")
                                   {:agent agent})]
    (when-not (= 200 (:status start-resp))
      (throw (ex-info "pending-session start failed" {:resp start-resp})))
    (let [start-json (json/parse-string (:body start-resp) true)
          login-get (http-get (:login_url start-json))]
      (when-not (= 200 (:status login-get))
        (throw (ex-info "login redirect chain failed" {:resp login-get})))
      (let [choose (http-post-form (str base-url "/auth/choose-username")
                                   {:session (:session start-json) :username username})]
        (when-not (= 200 (:status choose))
          (throw (ex-info "choose-username failed" {:resp choose})))
        (let [poll (http-get (str base-url "/api/v0/pending-session/" (:session start-json)))]
          (when-not (= 200 (:status poll))
            (throw (ex-info "pending-session poll failed" {:resp poll})))
          (let [poll-json (json/parse-string (:body poll) true)]
            (when-not (:complete poll-json)
              (throw (ex-info "pending session not complete" {:poll poll-json})))
            (when-not (clojure.string/starts-with? (:token poll-json) "slug_")
              (throw (ex-info "bad token in poll response" {:poll poll-json})))
            (:token poll-json)))))))
