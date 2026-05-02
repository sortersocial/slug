(ns test.browser-public-garden
  "Regression: ingest ontology items in the public room via RPC, then open /~ in the browser
   and assert ranked + unranked paths appear (HTML garden index — not JSON API)."
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

(defn public-garden-flow! []
  (println "\n━━━ browser public room garden index (RPC seed + GET /~) ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-pub-garden-"})))
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
           thread-tag "browser-pub-garden"
           ;; Ranked pair at root + one isolate so both panels are exercised.
           raw (str "# " thread-tag "\n\n"
                    "~/br-pub-a {alpha}\n"
                    "~/br-pub-b {beta}\n"
                    "~/br-pub-c {gamma}\n"
                    "~/br-pub-a 2:1 ~/br-pub-b {browser regression vote}\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed public post via rpc")
           rank-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"GetGardenRank" {"room" "public"
                                         "parent_path" "~"
                                         "depth" 1}}])
           rank-json (json/parse-string (:body rank-resp) false)
           _ (is (true? (get-in rank-json ["results" 0 "ok"])) "GetGardenRank ok")
           comps (get-in rank-json ["results" 0 "result" "GardenRank" "components"])
           _ (is (pos? (count comps)) "rank API reports at least one component")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")
               (page/navigate pg (str base-url "/~"))
               (is (wait-for-text pg "body" "paths" 15000) "public garden index shows paths heading")
               (is (wait-for-text pg "body" "ordering" 10000)
                   "ranked child group meta visible")
               (is (wait-for-text pg "body" "~/br-pub-a" 10000) "ranked list shows ~/br-pub-a")
               (is (wait-for-text pg "body" "~/br-pub-b" 10000) "ranked list shows ~/br-pub-b")
               (is (wait-for-text pg "body" "unranked" 10000) "unranked section present")
               (is (wait-for-text pg "body" "~/br-pub-c" 10000)
                   "unranked list shows ~/br-pub-c"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn public-garden-browser-test [& _args]
  (public-garden-flow!))

(deftest browser-public-garden-index-check
  (public-garden-flow!))
