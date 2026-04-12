(ns test.browser-post-redact
  "Tombstone expand/collapse uses POST /ui + __rpc__ (expand_redacted_post / collapse_redacted_post)
   and returns eval-able JS — covered here via Playwright, not HTTP-only tests."
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

(defn- wait-for-absent [pg selector substr timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (or (locator/text-content (page/locator pg selector)) "")]
        (if (str/includes? text substr)
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false)
          true)))))

(defn post-redact-flow! []
  (println "\n━━━ browser post redact / tombstone ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-redact-"})))
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
           thread-tag "redact-browser"
           ;; Items + vote so we can assert garden clears after redact
           raw (str "# " thread-tag "\n\n"
                    "~/tomb-a {tomb item a}\n"
                    "~/tomb-b {tomb item b}\n"
                    "~/tomb-a 2:1 ~/tomb-b {browser redact vote}\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed post via rpc")
           thr (oauth/http-post-json
                (str base-url "/api/v0/rpc")
                [{"GetForumThread" {"room" "public"
                                    "thread_tag" thread-tag
                                    "offset" nil
                                    "limit" nil
                                    "since" nil
                                    "before" nil
                                    "actor" nil
                                    "post_id" nil}}]
                :headers {})
           thr-json (json/parse-string (:body thr) false)
           post-id (get-in thr-json ["results" 0 "result" "ForumThread" "items" 0 "id"])
           _ (is (some? post-id) "forum thread returns post id")
           rank-before (oauth/http-post-json
                        (str base-url "/api/v0/rpc")
                        [{"GetGardenRank" {"room" "public"
                                          "parent_path" "~"
                                          "depth" 1}}])
           rb (json/parse-string (:body rank-before) false)
           rank-n (count (get-in rb ["results" 0 "result" "GardenRank" "components" 0 "ranking"]))]
       (is (pos? rank-n) "garden has ranked items before redact")
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session on login redirect")
               (page/navigate pg (str base-url "/t/" thread-tag))
               (is (wait-for-text pg "#thread-feed-region" "tomb item" 15000)
                        "thread shows post body before delete")
               (is (wait-for-text pg ".post-delete-btn" "delete" 5000)
                        "delete button visible for author")
               (let [rx (oauth/http-post-json
                         (str base-url "/api/v0/rpc")
                         [{"PostRedact" {"post_id" post-id}}]
                         :headers {"Authorization" (str "Bearer " alice-token)})]
                 (is (true? (get-in (json/parse-string (:body rx) false) ["results" 0 "ok"]))
                          "PostRedact rpc succeeds"))
               (page/navigate pg (str base-url "/t/" thread-tag))
               (is (wait-for-text pg "#thread-feed-region" "deleted" 15000)
                        "tombstone shows after redact")
               (is (wait-for-absent pg "#thread-feed-region" "tomb item" 10000)
                        "original body hidden in collapsed tombstone")
               (is (wait-for-absent pg "#thread-feed-region" "post-delete-btn" 5000)
                        "delete button gone after redact")
               (locator/click (page/locator pg ".show-deleted-link"))
               (is (wait-for-text pg "#thread-feed-region" "tomb item" 15000)
                        "expand shows stored content")
               (locator/click (page/locator pg ".hide-deleted-link"))
               (is (wait-for-absent pg "#thread-feed-region" "tomb item" 10000)
                        "collapse hides content again"))))))
     (let [rank-after (oauth/http-post-json
                       (str base-url "/api/v0/rpc")
                       [{"GetGardenRank" {"room" "public"
                                         "parent_path" "~"
                                         "depth" 1}}])
           ra (json/parse-string (:body rank-after) false)
           comps (get-in ra ["results" 0 "result" "GardenRank" "components"])
           empty-rank (or (empty? comps)
                          (empty? (get (first comps) "ranking")))]
       (is empty-rank "garden ranking cleared after redact"))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (babashka.fs/delete-tree tmp-dir)))

   nil))

(defn post-redact-browser-test [& _args]
  (post-redact-flow!))

(deftest browser-post-redact-tombstone-check
  (post-redact-flow!))
