(ns test.browser-vote-compare
  "Vote compare: POST /ui morphs preview + edge history; thread iframe loads /embed/t/…"
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

(defn- enc [^String s]
  (java.net.URLEncoder/encode s "UTF-8"))

(defn vote-compare-flow! []
  (println "\n━━━ browser vote compare (POST /ui + edge history + preview) ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-vote-compare-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))

   (bind !server (atom nil))
   (bind !google (atom nil))
   (bind server-env (common/slug-server-env tmp-dir base-url google-url slug-port))
   (try
     (reset! !google (oauth/start-mock-google google-port
                                              :google-users ["google-user-alice"]))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           thread-tag "browser-vote-compare"
           left-url "https://slug.social/~/gp-vote-a"
           right-url "https://slug.social/~/gp-vote-b"
           raw (str "# " thread-tag "\n\n"
                    "~/gp-vote-a {one}\n"
                    "~/gp-vote-b {two}\n"
                    "~/gp-vote-a 1:1 ~/gp-vote-b {seed edge vote}\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed items + edge vote via rpc")
           cmp-url (str base-url "/vote/compare?left=" (enc left-url) "&right=" (enc right-url))]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")
               (page/navigate pg cmp-url)
               (is (wait-for-text pg ".vote-compare-shell" "compare" 15000) "vote compare shell")
               (is (wait-for-text pg "ol.vote-edge-history" "seed edge vote" 15000)
                   "edge history lists canonical-order vote")
               (locator/fill (page/locator pg "#vote-explain") "because playwright says so")
               (locator/click (page/locator pg "#vote-compare-form button[type=submit]"))
               (is (wait-for-text pg "#vote-compare-preview" "because playwright" 20000)
                   "preview region morphs new post card")
               (is (wait-for-text pg "ol.vote-edge-history" "because playwright" 20000)
                   "new vote appears in edge history after morph"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn vote-compare-browser-test [& _args]
  (vote-compare-flow!))

(deftest browser-vote-compare-post-and-edge-history
  (vote-compare-flow!))
