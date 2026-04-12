(ns test.walkthrough-fixture
  "Launch a local slug server with mock OAuth and seed browser-friendly demo data."
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
            [test.common :as common]
            [test.oauth :as oauth]))

(defn- assert! [pred msg]
  (when-not pred
    (throw (ex-info msg {}))))

(defn- default-agent [n]
  (format "00000000-0000-4000-8000-%012d:test:walkthrough/browser" n))

(defn- rpc! [base-url bearer payload]
  (oauth/http-post-json
    (str base-url "/api/v0/rpc")
    payload
    :headers {"Authorization" (str "Bearer " bearer)}))

(defn- parse-json [resp]
  (json/parse-string (:body resp) true))

(defn- ok-result [resp]
  (let [body (parse-json resp)
        line (get-in body [:results 0])]
    (assert! (= 200 (:status resp)) (str "expected HTTP 200, got " (:status resp)))
    (assert! (true? (:ok line)) (str "rpc line failed: " line))
    (:result line)))

(defn- seed-demo! [base-url]
  (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice" :agent (default-agent 1))
        bob-token (oauth/fetch-bearer-token! base-url :username "bob" :agent (default-agent 2))
        room-result (ok-result (rpc! base-url alice-token [{"RoomCreate" {"slug" "walkthrough-room"}}]))
        room-id (get-in room-result [:RoomCreated :room_id])
        [room-short room-slug] (str/split room-id #"/" 2)]
    (ok-result
      (rpc! base-url alice-token
            [{"RoomGrant" {"room" room-id
                           "username" "bob"
                           "capabilities" ["view" "post" "vote" "add_item"]}}]))
    (let [wall-text (str "Walkthrough seed — multi-paragraph stress test.\n\n"
                         "Second paragraph: the garden holds ~/secret/item and ~/secret/other. "
                         "Votes like ~/secret/item 3:1 ~/secret/other {because} should still parse.\n\n"
                         "Third block: lorem-style filler so wrapping and vertical rhythm are obvious. "
                         "We want enough prose that the thread view scrolls and pre blocks show overflow "
                         "behavior (long lines, slug links, embed URLs) without looking like toy data.\n\n"
                         "Overflow torture (no spaces — browser must break or scroll):\n"
                         "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n\n"
                         "https://open.spotify.com/track/4iV5W9uYEdYUVa79Axb7U9 https://www.youtube.com/watch?v=dQw4w9WgXcQ\n\n"
                         "~/secret/item {classified}\n"
                         "~/secret/other {secondary}\n"
                         "~/secret/item 3:1 ~/secret/other {because}\n")
          rpc (json/generate-string
                {:action "post_ingest"
                 :room room-id
                 :thread_tag "walkthrough-thread"
                 :text wall-text})
          post-resp (oauth/http-post-form
                      (str base-url "/ui")
                      {:__rpc__ rpc}
                      :headers {"Authorization" (str "Bearer " alice-token)})]
      (assert! (= 200 (:status post-resp)) "seed post must succeed"))
    (let [bob-reply (str "Bob here — reply with another long block so the thread has multiple cards.\n\n"
                         "Paragraph two: repeating slugs ~/secret/item and ~/secret/other for cross-post link styling. "
                         "If everything wraps cleanly, monospace pre + serif body (in craft theme) should still feel readable.\n\n"
                         "More overflow: XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX\n")
          bob-rpc (json/generate-string
                    {:action "post_ingest"
                     :room room-id
                     :thread_tag "walkthrough-thread"
                     :text bob-reply})
          bob-post (oauth/http-post-form
                     (str base-url "/ui")
                     {:__rpc__ bob-rpc}
                     :headers {"Authorization" (str "Bearer " bob-token)})]
      (assert! (= 200 (:status bob-post)) "seed reply post must succeed"))
    {:users {:alice {:token alice-token}
             :bob {:token bob-token}}
     :room {:id room-id
            :short room-short
            :slug room-slug
            :url (str base-url "/r/" room-short "/" room-slug)
            :thread_url (str base-url "/r/" room-short "/" room-slug "/t/walkthrough-thread")
            :garden_url (str base-url "/r/" room-short "/" room-slug "/~/secret/item")}}))

(defn run-fixture [& _args]
  (let [build (common/run-cargo-build-release! ["slugsocial-server"])
        _ (assert! (zero? (:exit build)) "cargo build --release failed")
        server-bin "target/release/slugsocial-server"
        tmp-dir (str (fs/create-temp-dir {:prefix "slug-walkthrough-"}))
        slug-port (common/pick-port)
        google-port (common/pick-port)
        base-url (str "http://127.0.0.1:" slug-port)
        google-url (str "http://127.0.0.1:" google-port)
        stable-dir "/tmp/slug-walkthrough-fixture"
        summary-path (str stable-dir "/summary.json")
        !server (atom nil)
        !google (atom nil)
        server-env (common/slug-server-env tmp-dir base-url google-url slug-port)]
    (try
      (fs/create-dirs stable-dir)
      (reset! !google
              (oauth/start-mock-google google-port
                                       :google-users ["google-user-alice" "google-user-bob"]))
      (reset! !server (common/start-server server-bin server-env))
      (assert! (common/wait-for-server base-url 10000) "server did not become healthy")
      (let [seeded (seed-demo! base-url)
            summary {:base_url base-url
                     :mock_google_url google-url
                     :data_dir tmp-dir
                     :summary seeded}]
        (spit summary-path (json/generate-string summary {:pretty true}))
        (println "")
        (println "walkthrough fixture ready")
        (println (str "  base url:      " base-url))
        (println (str "  room page:     " (get-in summary [:summary :room :url])))
        (println (str "  thread page:   " (get-in summary [:summary :room :thread_url])))
        (println (str "  garden page:   " (get-in summary [:summary :room :garden_url])))
        (println (str "  summary json:  " summary-path))
        (println "")
        (println "seeded users")
        (println "  alice / bob via mock OAuth")
        (println "")
        (println "press Ctrl-C to stop")
        (flush)
        (while true
          (Thread/sleep 1000)))
      (finally
        (when-some [s @!server] (common/kill-server s))
        (when-some [g @!google] ((:stop-fn g)))
        (fs/delete-tree tmp-dir)))))
