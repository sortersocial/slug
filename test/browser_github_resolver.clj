(ns test.browser-github-resolver
  "Playwright coverage for on-demand GitHub external resolver imports."
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
            [clojure.test :refer [deftest is]]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as locator]
            [com.blockether.spel.page :as page]
            [org.httpkit.server :as http]
            [test.common :as common]
            [test.oauth :as oauth]))

(defn- wait-for-text [pg selector expected timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (locator/text-content (page/locator pg selector))]
        (if (and (string? text) (str/includes? text expected))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- wait-for-http-text [url expected timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (try
                   (:body (oauth/http-get url))
                   (catch Exception _ nil))]
        (if (and (string? text) (str/includes? text expected))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- wait-for-http-absent [url needle timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (try
                   (:body (oauth/http-get url))
                   (catch Exception _ nil))]
        (if (and (string? text) (not (str/includes? text needle)))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- issue-json [number title]
  {:number number
   :title title
   :body (str "Body for #" number)
   :state "open"
   :user {:login "octo"}})

(defn- start-mock-github [port !issues]
  (let [!paths (atom [])
        handler (fn [req]
                  (swap! !paths conj (:uri req))
                  (case (:uri req)
                    "/users/octo/repos"
                    {:status 200
                     :headers {"Content-Type" "application/json"}
                     :body (json/generate-string
                            [{:name "hello"
                              :full_name "octo/hello"
                              :description "Hello repo"}
                             {:name "other"
                              :full_name "octo/other"
                              :description "Other repo"}])}

                    "/repos/octo/hello/issues"
                    {:status 200
                     :headers {"Content-Type" "application/json"}
                     :body (json/generate-string
                            (conj (vec @!issues)
                                  {:number 99
                                   :title "PR should be filtered"
                                   :pull_request {}}))}

                    {:status 404 :body "not found"}))
        stop-fn (http/run-server handler {:port port})]
    {:stop-fn stop-fn :paths !paths}))

(defn github-resolver-flow! []
  (println "\n━━━ browser GitHub resolver: on-demand children and siblings ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-github-resolver-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind github-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))
   (bind github-url (str "http://127.0.0.1:" github-port))

   (bind !server (atom nil))
   (bind !google (atom nil))
   (bind !github (atom nil))
   (bind !issues (atom [(issue-json 42 "Seeded issue")
                        (issue-json 43 "Imported sibling")]))
   (bind server-env (assoc (common/slug-server-env tmp-dir base-url google-url slug-port)
                           "SLUG_GITHUB_API_BASE_URL" github-url
                           "SLUG_GITHUB_RESOLVER_COOLDOWN_MS" "0"))
   (try
     (reset! !google (oauth/start-mock-google google-port
                                              :google-users ["google-user-alice"]))
     (reset! !github (start-mock-github github-port !issues))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           seed-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" "github-resolver-seed"
                                "text" "https://github.com/octo/hello/issues/42 {Seed pasted issue}"
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           seed-json (json/parse-string (:body seed-resp) false)]
       (is (true? (get-in seed-json ["results" 0 "ok"])) "seed pasted GitHub issue")

       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")

               (page/navigate pg (str base-url "/-/github.com/octo/hello"))
               ;; Legacy host-first URL should land on canonical /-/https://…
               (is (wait-for-text pg "body" "GitHub resolver" 15000)
                   "legacy URL reaches resolver page after redirect")
               (let [url (page/url pg)]
                 (is (str/includes? url "/-/https://github.com/octo/hello")
                     (str "address bar uses canonical https wire form, got " url)))

               (page/navigate pg (str base-url "/-/https://github.com/octo"))
               (is (wait-for-text pg "#external-resolver-panel" "GitHub resolver" 15000)
                   "user page shows resolver panel")
               (locator/click (page/locator pg "[data-testid=\"github-resolve-children\"]"))
               (is (wait-for-text pg "body" "-/https://github.com/octo/other" 15000)
                   "user resolver imports repos")

               (page/navigate pg (str base-url "/-/https://github.com/octo/hello"))
               (locator/click (page/locator pg "[data-testid=\"github-resolve-children\"]"))
               (is (wait-for-text pg "body" "-/https://github.com/octo/hello/pulls" 15000)
                   "repo resolver imports structural children")
               (is (wait-for-text pg "body" "-/https://github.com/octo/hello/commits" 15000)
                   "repo resolver imports commits section")
               (is (wait-for-text pg "body" "-/https://github.com/octo/hello/releases" 15000)
                   "repo resolver imports releases section")

               (page/navigate pg (str base-url "/-/https://github.com/octo/hello/issues"))
               (locator/click (page/locator pg "[data-testid=\"github-resolve-children\"]"))
               (is (wait-for-http-text (str base-url "/-/https://github.com/octo/hello/issues")
                                       "-/https://github.com/octo/hello/issues/43"
                                       15000)
                   "issues resolver imports open issues as children")
               (is (wait-for-http-text (str base-url "/-/https://github.com/octo/hello/issues")
                                       "-/https://github.com/octo/hello/issues/42"
                                       15000)
                   "issues resolver imports issue 42")

               ;; Close #43 on GitHub; refresh should redact its system post.
               (reset! !issues [(issue-json 42 "Seeded issue")])
               (page/navigate pg (str base-url "/-/https://github.com/octo/hello/issues"))
               (locator/click (page/locator pg "[data-testid=\"github-resolve-children\"]"))
               (is (wait-for-http-absent (str base-url "/-/https://github.com/octo/hello/issues")
                                         "-/https://github.com/octo/hello/issues/43"
                                         15000)
                   "closed issue removed from garden after refresh")
               (is (wait-for-http-text (str base-url "/-/https://github.com/octo/hello/issues")
                                       "-/https://github.com/octo/hello/issues/42"
                                       15000)
                   "still-open issue kept after refresh")))))

       (is (some #{"/users/octo/repos"} @(:paths @!github))
           "mock GitHub saw user repos request")
       (is (some #{"/repos/octo/hello/issues"} @(:paths @!github))
           "mock GitHub saw repo issues request")
       (is (>= (count (filter #{"/repos/octo/hello/issues"} @(:paths @!github))) 2)
           "issues endpoint hit on import and refresh"))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (when-some [g @!github] ((:stop-fn g)))))))

(deftest browser-github-resolver-test
  (github-resolver-flow!))
