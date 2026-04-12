(ns test.browser-sse
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as locator]
            [com.blockether.spel.page :as page]
            [test.common :as common]
            [test.oauth :as oauth]))

(def ^:private counts (atom {:pass 0 :fail 0}))

(defn- assert! [pred msg]
  (common/test-assert! counts pred msg))

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
    (assert! (wait-for-text pg "body" (str "@" username) 15000)
             (str "browser login completes for " username))))

(defn private-thread-sse-browser-test [& _args]
  (println "\n━━━ browser SSE private-thread check ━━━\n")
  (reset! counts {:pass 0 :fail 0})

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (assert! (zero? (:exit build)) "cargo build succeeds")
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
     (assert! (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           bob-token   (oauth/fetch-bearer-token! base-url :username "bob")
           create      (oauth/http-post-json (str base-url "/api/v0/rpc")
                                             [{"RoomCreate" {"slug" "browser-live-room"}}]
                                             :headers {"Authorization" (str "Bearer " alice-token)})
           create-json (json/parse-string (:body create) false)
           room-id     (get-in create-json ["results" 0 "result" "RoomCreated" "room_id"])
           _           (assert! (some? room-id) "room created for browser test")
           grant       (oauth/http-post-json (str base-url "/api/v0/rpc")
                                             [{"RoomGrant" {"room" room-id
                                                            "username" "bob"
                                                            "capabilities" ["view" "post" "vote" "add_item"]}}]
                                             :headers {"Authorization" (str "Bearer " alice-token)})
           grant-json  (json/parse-string (:body grant) false)
           _           (assert! (true? (get-in grant-json ["results" 0 "ok"])) "bob granted room access")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [alice-ctx (core/new-context browser)]
             (core/with-context [bob-ctx (core/new-context browser)]
               (core/with-page [alice-pg (core/new-page-from-context alice-ctx)]
                 (core/with-page [bob-pg (core/new-page-from-context bob-ctx)]
                  (login-user! alice-pg base-url "alice")
                  (login-user! bob-pg base-url "bob")

                   (let [[room-short room-slug] (str/split room-id #"/" 2)
                         room-url (str base-url "/r/" room-short "/" room-slug)]
                    (page/navigate alice-pg room-url)
                    ;; Room new-thread compose is collapsed until POST /ui expands it (slug_ui.js must be loaded).
                    (assert! (wait-for-text alice-pg "#room-new-thread-ui-slot"
                                           "new thread in this room" 15000)
                             "alice sees collapsed new-thread toggle")
                    (locator/click (page/locator alice-pg "#room-new-thread-ui-slot button.form-toggle"))
                    (assert! (wait-for-text alice-pg "#room-new-thread-ui-slot" "hide new thread form" 15000)
                             "alice expands new thread compose via POST /ui")
                    (locator/fill (page/locator alice-pg "#room-new-tag") "sse-thread")
                    (locator/fill (page/locator alice-pg "#room-new-thread-compose textarea") "seed thread")
                    (locator/click (page/locator alice-pg "#room-new-thread-compose button[type='submit']"))

                    (assert! (wait-for-text alice-pg "#thread-feed-region" "seed thread" 15000)
                             "alice lands on seeded thread page")

                    (page/navigate bob-pg (str room-url "/t/sse-thread"))
                    (assert! (wait-for-text bob-pg "#thread-feed-region" "seed thread" 15000)
                             "bob sees seeded thread body before live update")

                    (locator/fill (page/locator alice-pg "#thread-compose textarea") "hello from alice over sse")
                    (locator/click (page/locator alice-pg "#thread-compose button[type='submit']"))

                    (assert! (wait-for-text bob-pg "#thread-feed-region" "hello from alice over sse" 15000)
                             "bob sees alice post body in live private thread without refresh via sse")))))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass fail :fail} @counts)
   (if (zero? fail)
     (println (str "\n" common/ansi-green "━━━ " pass " browser SSE checks passed ━━━" common/ansi-reset "\n"))
     (do (println (str "\n" common/ansi-red "━━━ " fail " browser SSE checks FAILED ━━━" common/ansi-reset "\n"))
         (System/exit 1)))))
