(ns test.browser-sse
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

(defn- login-user! [pg base-url username]
  (let [start-resp (oauth/http-post-json (str base-url "/api/v0/pending-session")
                                         {:agent (format "00000000-0000-0000-0000-%012d:test:browser/chrome"
                                                         (inc (count username)))})
        start-json (json/parse-string (:body start-resp) true)
        login-url  (:login_url start-json)]
    (page/navigate pg login-url)
    (is (wait-for-text pg "body" (str "@" username) 30000)
        (str "browser login completes for " username))))

(defn sse-browser-flow! []
  (println "\n━━━ browser SSE private-thread check ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-sse-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))

   (bind !server (atom nil))
   (bind !google (atom nil))
   (bind server-env (common/slug-server-env tmp-dir base-url google-url slug-port))
   (try
     (reset! !google (oauth/start-mock-google google-port
                                              :google-users ["google-user-alice" "google-user-bob"]))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           bob-token   (oauth/fetch-bearer-token! base-url :username "bob")
           create      (oauth/http-post-json (str base-url "/api/v0/rpc")
                                             [{"RoomCreate" {"slug" "browser-live-room"}}]
                                             :headers {"Authorization" (str "Bearer " alice-token)})
           create-json (json/parse-string (:body create) false)
           room-id     (get-in create-json ["results" 0 "result" "RoomCreated" "room_id"])
           _           (is (some? room-id) "room created for browser test")
           grant       (oauth/http-post-json (str base-url "/api/v0/rpc")
                                             [{"RoomGrant" {"room" room-id
                                                            "username" "bob"
                                                            "capabilities" ["view" "post" "vote" "add_item"]}}]
                                             :headers {"Authorization" (str "Bearer " alice-token)})
           grant-json  (json/parse-string (:body grant) false)
           _           (is (true? (get-in grant-json ["results" 0 "ok"])) "bob granted room access")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [alice-ctx (core/new-context browser)]
             (core/with-context [bob-ctx (core/new-context browser)]
               (core/with-page [alice-pg (core/new-page-from-context alice-ctx)]
                 (core/with-page [bob-pg (core/new-page-from-context bob-ctx)]
                  (login-user! alice-pg base-url "alice")
                  (login-user! bob-pg base-url "bob")

                   (let [[room-short room-slug] (str/split room-id #"/" 2)
                         room-url (str base-url "/r/" room-short "/" room-slug)
                         thread-url (str room-url "/t/sse-thread")]
                    ;; Object under test: slug_ui.js intercepts POST /ui, evals JS, morphs
                    ;; #room-new-thread-ui-slot (expand compose), then post_ingest redirects to thread.
                    (page/navigate alice-pg room-url)
                    (page/wait-for-load-state alice-pg :load)
                    (is (wait-for-text alice-pg "#room-new-thread-ui-slot"
                                       "new thread in this room" 30000)
                        "collapsed new-thread control in slot (page + slug_ui.js)")
                    (locator/click (page/locator alice-pg "#room-new-thread-ui-slot button.form-toggle"))
                    (is (wait-for-text alice-pg "#room-new-thread-ui-slot"
                                       "hide new thread form" 30000)
                        "compose expanded via POST /ui morph (slug_ui.js)")
                    (locator/fill (page/locator alice-pg "#room-new-tag") "sse-thread")
                    (locator/fill (page/locator alice-pg "#room-new-thread-compose textarea") "seed thread")
                    (locator/click (page/locator alice-pg "#room-new-thread-form button[type='submit']"))
                    (page/wait-for-url alice-pg thread-url {:timeout 90000.0})
                    (page/wait-for-load-state alice-pg :load)
                    (is (wait-for-text alice-pg "#thread-feed-region" "seed thread" 45000)
                        "alice sees first post after form ingest + redirect")

                    (page/navigate bob-pg thread-url)
                    (page/wait-for-load-state bob-pg :load)
                    (is (wait-for-text bob-pg "#thread-feed-region" "seed thread" 45000)
                        "bob sees seeded thread body before live update")

                    (locator/fill (page/locator alice-pg "#thread-compose textarea") "hello from alice over sse")
                    (locator/click (page/locator alice-pg "#thread-compose button[type='submit']"))

                    (is (wait-for-text bob-pg "#thread-feed-region" "hello from alice over sse" 45000)
                             "bob sees alice post body in live private thread without refresh via sse")))))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn private-thread-sse-browser-test [& _args]
  (sse-browser-flow!))

(deftest browser-sse-private-thread-check
  (sse-browser-flow!))
