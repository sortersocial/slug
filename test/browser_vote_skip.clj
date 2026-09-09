(ns test.browser-vote-skip
  "Skip a /vote pair, review it on /vote/skipped, then unskip."
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
      (let [text (try (locator/text-content (page/locator pg selector)) (catch Exception _ nil))]
        (if (and (string? text) (str/includes? text expected))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- enc [^String s]
  (java.net.URLEncoder/encode s "UTF-8"))

(defn vote-skip-flow! []
  (println "\n━━━ browser vote skip / unskip ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-vote-skip-"})))
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
           thread-tag "browser-vote-skip"
           raw (str "# " thread-tag "\n\n"
                    "~/skippool/a {one}\n"
                    "~/skippool/b {two}\n"
                    "~/skippool/c {three}\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed items via rpc")
           cmp-url (str base-url "/vote?left=" (enc "~/skippool/a")
                        "&right=" (enc "~/skippool/b")
                        "&pool=" (enc "~/skippool"))]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")
               (page/navigate pg cmp-url)
               (is (wait-for-text pg "body.view-vote-compare" "compare" 15000) "vote compare page")
               (is (wait-for-text pg "[data-testid=\"vote-skip\"]" "skip" 15000) "skip control")
               (locator/click (page/locator pg "[data-testid=\"vote-skip\"]"))
               (is (wait-for-text pg ".vote-compare-pair" "~/c" 15000)
                   "skip redirects to a remaining pair including c")
               (page/navigate pg (str base-url "/vote/skipped"))
               (is (wait-for-text pg "[data-testid=\"vote-skipped-row\"]" "~/a" 15000)
                   "skipped list shows the hidden pair")
               (locator/click (page/locator pg "[data-testid=\"vote-unskip\"]"))
               (is (wait-for-text pg "#vote-skipped-region" "no skipped pairs" 15000)
                   "unskip morphs the list empty"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn vote-skip-browser-test [& _args]
  (vote-skip-flow!))

(deftest browser-vote-skip-and-unskip
  (vote-skip-flow!))
