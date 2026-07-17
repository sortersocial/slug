(ns test.browser-vote-login-redirect
  "Guest opens a shared pair link, clicks post vote → login → back to that pair."
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
            [clojure.test :refer [deftest is]]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as locator]
            [com.blockether.spel.page :as page]
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

(defn- wait-for-url-includes [pg needle timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [url (page/url pg)]
        (if (and (string? url) (str/includes? url needle))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- enc [^String s]
  (java.net.URLEncoder/encode s "UTF-8"))

(defn vote-login-redirect-flow! []
  (println "\n━━━ browser guest vote → login → back to pair ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-vote-login-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))

   (bind !server (atom nil))
   (bind !google (atom nil))
   (bind server-env (common/slug-server-env tmp-dir base-url google-url slug-port))
   (try
     (reset! !google (oauth/start-mock-google google-port
                                              :google-users ["google-user-alice"
                                                             "google-user-guest"]))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           thread-tag "browser-vote-login"
           left-url "https://slug.social/~/gp-login-a"
           right-url "https://slug.social/~/gp-login-b"
           raw (str "# " thread-tag "\n\n"
                    "~/gp-login-a {alpha}\n"
                    "~/gp-login-b {beta}\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed pair items via rpc")
           pair-path (str "/vote?left=" (enc left-url) "&right=" (enc right-url))
           cmp-url (str base-url pair-path)]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               ;; Fresh guest context — no session cookie.
               (page/navigate pg cmp-url)
               (is (wait-for-text pg "body.view-vote-compare" "compare" 15000)
                   "guest can open shared pair URL")
               (is (wait-for-text pg "#vote-compare-form" "post vote" 15000)
                   "guest sees post vote button")
               (locator/fill (page/locator pg "#vote-explain") "shared-link guest reason")
               (locator/click (page/locator pg "#vote-compare-form button[type=submit]"))

               ;; Unauth VoteComparePost → /login?next=<pair> → OAuth → choose-username.
               (is (wait-for-url-includes pg "/auth/choose-username" 20000)
                   "guest vote submit reaches choose-username via login?next")
               (is (wait-for-text pg "#choose-username-form" "username" 15000)
                   "choose-username form visible")
               (locator/fill (page/locator pg "input[name=username]") "guestvoter")
               (locator/click (page/locator pg "#choose-username-form button[type=submit]"))

               (is (wait-for-url-includes pg "/vote?" 20000)
                   "after login, redirected back to a vote pair URL")
               (is (wait-for-url-includes pg "gp-login-a" 5000)
                   "return URL keeps left item")
               (is (wait-for-url-includes pg "gp-login-b" 5000)
                   "return URL keeps right item")
               (is (wait-for-text pg "body.view-vote-compare" "compare" 15000)
                   "landed on vote compare page")
               (is (wait-for-text pg "body" "@guestvoter" 15000)
                   "session established as new user"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn vote-login-redirect-browser-test [& _args]
  (vote-login-redirect-flow!))

(deftest browser-vote-login-redirect-back-to-pair
  (vote-login-redirect-flow!))
