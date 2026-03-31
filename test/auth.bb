(ns test.auth
  "Auth v3 integration check: bb-mocked Google OAuth + pending session polling + whoami."
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
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

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
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

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
      {:status (.statusCode resp) :body (.body resp) :headers (.map (.headers resp))})))

(defn- parse-query [s]
  (into {}
        (for [part (clojure.string/split (or s "") #"&")
              :when (not (clojure.string/blank? part))]
          (let [[k v] (clojure.string/split part #"=" 2)]
            [(keyword (java.net.URLDecoder/decode k "UTF-8"))
             (some-> v (java.net.URLDecoder/decode "UTF-8"))]))))

(defn- start-mock-google [port]
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

(defn auth-test [& _args]
  (println "\n━━━ auth v3 integration check ━━━\n")
  (reset! counts {:pass 0 :fail 0})

  (println "building server binary…")
  (common/letlocals
   (bind build @(p/process [(common/cargo-bin) "build" "--release" "-p" "slugsocial-server"]
                           {:inherit true :env common/base-env}))
   (assert! (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")
   (assert! (fs/exists? server-bin) "server binary exists")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-auth-"})))
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
     (println (str "\nstarting mock google on :" google-port))
     (reset! !google (start-mock-google google-port))
     (assert! (some? (:stop-fn @!google)) "mock google started")

     (println (str "starting server on :" slug-port))
     (reset! !server (common/start-server server-bin server-env))
     (assert! (common/wait-for-server base-url 10000) "server responds to /healthz")

     (println "\nstarting pending session…")
     (let [start-resp (http-post-json (str base-url "/api/v0/pending-session")
                                      {:agent "@@00000000-0000-0000-0000-000000000000:bb:local/dev"})
           _ (assert! (= 200 (:status start-resp)) "pending-session start returns 200")
           start-json (json/parse-string (:body start-resp) true)]
       (assert! (clojure.string/starts-with? (:session start-json) "p_") "session id has p_ prefix")
       (assert! (clojure.string/includes? (:login_url start-json) "/auth/login") "login_url provided")

       (println "\nsimulating browser oauth redirects…")
       ;; This will follow redirects: /auth/login -> mock google -> /auth/callback -> /auth/choose-username
       (let [login-get (http-get (:login_url start-json))]
         (assert! (= 200 (:status login-get)) "choose-username page reachable after oauth callback"))

       (println "\nchoosing username…")
       (let [choose (http-post-form (str base-url "/auth/choose-username")
                                    {:session (:session start-json) :username "bbuser"})]
         (assert! (= 200 (:status choose)) "choose-username POST returns 200"))

       (println "\npolling pending session…")
       (let [poll (http-get (str base-url "/api/v0/pending-session/" (:session start-json)))]
         (assert! (= 200 (:status poll)) "pending-session poll returns 200")
         (let [poll-json (json/parse-string (:body poll) true)]
           (assert! (:complete poll-json) "pending session complete=true")
           (assert! (= "@bbuser" (:user poll-json)) "poll returns @bbuser")
           (assert! (clojure.string/starts-with? (:token poll-json) "slug_") "poll returns bearer token")

           (println "\nwhoami…")
           (let [who (http-get (str base-url "/api/v0/whoami")
                               :headers {"Authorization" (str "Bearer " (:token poll-json))})]
             (assert! (= 200 (:status who)) "whoami returns 200")
             (let [who-json (json/parse-string (:body who) true)]
              (assert! (= "@bbuser" (:user who-json)) "whoami user is @bbuser"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass} @counts)
   (println (str "\n" ansi-green "━━━ " pass " auth checks passed ━━━" ansi-reset "\n"))))

