(ns test.browser-redact-thread-index
  "Regression: after PostRedacted, rebuild_scope_content drops garden projection for the redacted
   ingest but ingests_by_scope_thread keeps tombstone ids so chronological indices stay aligned
   with /t/:tag/:index and GetRankHistory.thread_post_index. If redacted ids were removed from the
   deque without replumbing rank history / single-post resolution, the second post would incorrectly
   get thread_post_index 0 (and GetRankHistory would panic if history rows still referenced the
   redacted post_id)."
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

(defn- thread-post-at-index [forum-json idx]
  (some (fn [item]
          (when (and (= "post" (get item "kind"))
                     (= idx (get item "index")))
            item))
        (get-in forum-json ["results" 0 "result" "ForumThread" "items"])))

(defn redact-preserves-thread-chrono-index-flow! []
  (println "\n━━━ browser: PostRedact vs thread_post_index_chronological ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-redact-idx-"})))
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
           thread-tag "redact-thread-idx"
           item-second "~/rix/c"
           ;; Two chronologically ordered posts in the same thread with disjoint tilde paths so
           ;; replay after redacting the first only reapplies the second ingest successfully.
           text-first (str "# " thread-tag "\n\n"
                           "{alpha}\n~/rix/a\n"
                           "{beta}\n~/rix/b\n"
                           "{browser redact thread idx post one}\n~/rix/a 2:1 ~/rix/b\n")
           text-second (str "# " thread-tag "\n\n"
                            "{gamma}\n~/rix/c\n"
                            "{delta}\n~/rix/d\n"
                            "{browser redact thread idx post two}\n~/rix/c 2:1 ~/rix/d\n")
           _ (is (true? (get-in (json/parse-string
                                 (:body (oauth/http-post-json
                                         (str base-url "/api/v0/rpc")
                                         [{"Post" {"room" "public"
                                                   "thread_tag" thread-tag
                                                   "text" text-first
                                                   "return_rank_diff" false}}]
                                         :headers {"Authorization" (str "Bearer " alice-token)}))
                                 false)
                                ["results" 0 "ok"]))
                 "first Post rpc ok")
           _ (is (true? (get-in (json/parse-string
                                 (:body (oauth/http-post-json
                                         (str base-url "/api/v0/rpc")
                                         [{"Post" {"room" "public"
                                                   "thread_tag" thread-tag
                                                   "text" text-second
                                                   "return_rank_diff" false}}]
                                         :headers {"Authorization" (str "Bearer " alice-token)}))
                                 false)
                                ["results" 0 "ok"]))
                 "second Post rpc ok")
           thr (json/parse-string
                (:body (oauth/http-post-json
                        (str base-url "/api/v0/rpc")
                        [{"GetForumThread" {"room" "public"
                                            "thread_tag" thread-tag
                                            "offset" 0
                                            "limit" 50
                                            "since" nil
                                            "before" nil
                                            "actor" nil
                                            "post_id" nil}}]
                        :headers {}))
                false)
           p0 (thread-post-at-index thr 0)
           p1 (thread-post-at-index thr 1)
           id-oldest (get p0 "id")
           id-newer (get p1 "id")
           _ (is (some? id-oldest) "thread has chronological index 0")
           _ (is (some? id-newer) "thread has chronological index 1")
           _ (is (not= id-oldest id-newer) "two distinct post ids")
           rx (oauth/http-post-json
               (str base-url "/api/v0/rpc")
               [{"PostRedact" {"post_id" id-oldest}}]
               :headers {"Authorization" (str "Bearer " alice-token)})
           _ (is (true? (get-in (json/parse-string (:body rx) false) ["results" 0 "ok"]))
                 "PostRedact oldest succeeds")
           hist-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"GetRankHistory" {"room" "public"
                                          "item_path" item-second}}])
           hist-json (json/parse-string (:body hist-resp) false)
           history (get-in hist-json ["results" 0 "result" "RankHistory" "history"])]
       (is (true? (get-in hist-json ["results" 0 "ok"]))
           "GetRankHistory ok after redact")
       (is (seq history)
           "GetRankHistory returns rows for second-post-only item (garden replayed without first ingest)")
       (doseq [[i row] (map-indexed vector history)]
         (is (= 1 (get row "thread_post_index"))
             (str "RankHistory row " i ": thread_post_index must stay 1 for the newer post after "
                  "redacting chronological index 0. Stable indices require keeping redacted ids in "
                  "ingests_by_scope_thread; naively filtering them out makes this 0 (and can panic "
                  "rank history if rows still reference the redacted post).")))
       (let [url-0 (str base-url "/t/" thread-tag "/0")
             url-1 (str base-url "/t/" thread-tag "/1")
             g0 (oauth/http-get url-0)
             g1 (oauth/http-get url-1)]
         (is (= 200 (:status g0))
             "GET /t/:tag/0 responds after redact (tombstone slot preserved)")
         (is (str/includes? (:body g0) "deleted")
             "single-post URL index 0 shows tombstone after redact")
         (is (= 200 (:status g1))
             "GET /t/:tag/1 responds")
         (is (str/includes? (:body g1) "browser redact thread idx post two")
             "index 1 still resolves to the second post body (not shifted to 0)"))
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/t/" thread-tag "/0"))
               (is (wait-for-text pg "body" "deleted" 15000)
                   "browser: per-post view 0 shows deleted tombstone")
               (page/navigate pg (str base-url "/t/" thread-tag "/1"))
               (is (wait-for-text pg "body" "browser redact thread idx post two" 15000)
                   "browser: per-post view 1 still shows second post"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn browser-redact-thread-index-test [& _args]
  (redact-preserves-thread-chrono-index-flow!))

(deftest browser-redact-thread-index-stable-after-post-redacted
  (redact-preserves-thread-chrono-index-flow!))
