(ns test.browser-room-delete
  "Playwright (Spel): expand members panel, confirm delete room, land on home with room gone."
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

(defn delete-room-flow! []
  (println "\n━━━ browser delete room (manage panel + POST /ui) ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-room-delete-"})))
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
           slug "pw-delete-room"
           create (oauth/http-post-json
                   (str base-url "/api/v0/rpc")
                   [{"RoomCreate" {"slug" slug}}]
                   :headers {"Authorization" (str "Bearer " alice-token)})
           create-json (json/parse-string (:body create) false)
           _ (is (true? (get-in create-json ["results" 0 "ok"])) "room created via rpc")
           room-id (get-in create-json ["results" 0 "result" "RoomCreated" "room_id"])
           _ (is (string? room-id) "room id present")
           [room-short room-slug] (str/split room-id #"/" 2)
           room-path (str "/r/" room-short room-slug)]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")
               (page/navigate pg (str base-url room-path))
               (is (wait-for-text pg "body" "members & permissions" 15000)
                   "room page shows members toggle")
               ;; Do not use bare `.form-toggle`: the new-thread slot uses the same class.
               (locator/click (page/locator pg "#room-members-section button.form-toggle"))
               (is (wait-for-text pg "[data-testid=\"delete-room\"]" "delete room" 10000)
                   "delete room button visible for manager")
               (page/once-dialog pg (fn [d]
                                      (is (= "confirm" (page/dialog-type d)) "browser confirm dialog")
                                      (page/dialog-accept! d)))
               (locator/click (page/locator pg "[data-testid=\"delete-room\"]"))
               (is (wait-for-text pg "body" "slug.social" 15000)
                   "redirected to home after delete")
               (let [list-resp (oauth/http-post-json
                                (str base-url "/api/v0/rpc")
                                ["RoomList"]
                                :headers {"Authorization" (str "Bearer " alice-token)})
                     list-json (json/parse-string (:body list-resp) false)
                     rooms (vec (get-in list-json ["results" 0 "result" "RoomList" "rooms"]))]
                 (is (true? (get-in list-json ["results" 0 "ok"])) "RoomList ok after delete")
                 (is (not (some #{room-id} rooms))
                     "deleted room absent from RoomList")))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn delete-room-via-ui-test [& _args]
  (delete-room-flow!))

(deftest browser-delete-room-check
  (delete-room-flow!))
