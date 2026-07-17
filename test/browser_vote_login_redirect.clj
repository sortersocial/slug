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
      (let [text (try (locator/text-content (page/locator pg selector))
                      (catch Exception _ nil))]
        (if (and (string? text) (str/includes? text expected))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- wait-for-url [pg pred timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [url (try (or (page/url pg) "") (catch Exception _ ""))]
        (if (pred url)
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
     ;; Single Google identity: alice seeds the pair, then the guest browser
     ;; OAuth reuses that identity (existing-user path) so /login?next= returns
     ;; via HTTP redirect with Set-Cookie — the path shared-link guests take
     ;; after their first account already exists.
     (reset! !google (oauth/start-mock-google google-port
                                              :google-users ["google-user-alice"]))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           thread-tag "browser-vote-login"
           left-item "~/gp-login-a"
           right-item "~/gp-login-b"
           raw (str "# " thread-tag "\n\n"
                    left-item " {alpha}\n"
                    right-item " {beta}\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed pair items via rpc")
           pair-path (str "/vote?left=" (enc left-item) "&right=" (enc right-item))
           cmp-url (str base-url pair-path)]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               ;; Fresh guest context — no session cookie.
               (page/navigate pg cmp-url)
               (is (wait-for-text pg "body.view-vote-compare" "compare" 15000)
                   "guest can open shared pair URL")
               (is (wait-for-text pg "a.vote-compare-login-cta" "post vote" 15000)
                   "guest sees post vote login CTA")
               (let [href (locator/get-attribute
                           (page/locator pg "a.vote-compare-login-cta") "href")]
                 (is (and (string? href)
                          (str/includes? href "/login?next=")
                          (str/includes? href (enc pair-path)))
                     (str "CTA href carries login?next=<pair>, got: " href)))
               (locator/click (page/locator pg "a.vote-compare-login-cta"))

               ;; Existing Google identity → HTTP redirect chain back to pair.
               (is (wait-for-url
                    pg
                    (fn [u]
                      (and (str/includes? u "/vote?")
                           (str/includes? u "gp-login-a")
                           (str/includes? u "gp-login-b")
                           (not (str/includes? u "/login"))
                           (not (str/includes? u "/auth/"))))
                    25000)
                   (str "after login, back on this pair; url=" (page/url pg)))
               (is (wait-for-text pg "body.view-vote-compare" "compare" 15000)
                   "landed on vote compare page")
               ;; Chromeless vote page has no @user chrome; the submit button
               ;; (vs guest login CTA) is the signed-in signal.
               (is (wait-for-text pg "#vote-compare-form button[type=submit]" "post vote" 15000)
                   "signed-in user sees real post vote submit")
               (is (not (wait-for-text pg "a.vote-compare-login-cta" "post vote" 1000))
                   "guest login CTA gone after session redirect"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn vote-login-redirect-browser-test [& _args]
  (vote-login-redirect-flow!))

(deftest browser-vote-login-redirect-back-to-pair
  (vote-login-redirect-flow!))
