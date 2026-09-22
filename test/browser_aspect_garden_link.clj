(ns test.browser-aspect-garden-link
  "Clicking :aspect in a thread opens the garden ranking for that scope."
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

(defn aspect-garden-link-flow! []
  (println "\n━━━ browser :aspect link → garden sort ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-aspect-garden-"})))
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
           thread-tag "celestial-hierarchy"
           raw (str "#celestial-hierarchy { movie poster }\n"
                    "~/celestial-hierarchy/named-angels { named angels }\n"
                    "~/celestial-hierarchy/named-angels/gabriel { gabriel }\n"
                    "~/celestial-hierarchy/named-angels/michael { michael }\n"
                    ":top-billing { whose name goes first }\n"
                    "{ gabriel first }\n"
                    "~gabriel 3:2 ~michael\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed aspect ranking via rpc")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/t/" thread-tag))
               (is (wait-for-text pg "body" ":top-billing" 15000) "thread shows :top-billing")
               (let [href (locator/get-attribute
                           (page/locator pg "a.pre-link[href*='aspect-top-billing']")
                           "href")]
                 (is (str/includes? (or href "") "/~/named-angels#aspect-top-billing")
                     (str ":top-billing href is garden sort, got " href)))
               (locator/click (page/locator pg "a.pre-link[href*='aspect-top-billing']"))
               (is (wait-for-text pg "#aspect-top-billing" ":top-billing" 15000)
                   "garden aspect ranking section is visible")
               (is (wait-for-text pg "#aspect-top-billing" "~/gabriel" 10000)
                   "ranked list includes gabriel")
               (let [url (or (page/url pg) "")]
                 (is (str/includes? url "/~/named-angels")
                     (str "landed on named-angels garden page, url=" url))
                 (is (str/includes? url "aspect-top-billing")
                     (str "hash targets the aspect ranking, url=" url))))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(deftest browser-aspect-ref-opens-garden-sort
  (aspect-garden-link-flow!))
