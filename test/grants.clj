(ns test.grants
  "Grant enforcement integration test: private room capabilities via POST /api/v0/rpc.

  Covers:
  - user without any grant -> RPC line ok=false on private room post
  - user with View but no Post -> ok=false for prose
  - user with View + Post -> ok=true for prose
  - user with View + Post but no Vote -> ok=false for vote
  - user with View + Post + Vote -> ok=true for vote"
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.test :refer [deftest is]]
            [test.common :as common]
            [test.oauth :as oauth]))

(defn- bearer [token] {"Authorization" (str "Bearer " token)})

(defn- rpc-batch! [base-url token cmds]
  (let [resp (oauth/http-post-json (str base-url "/api/v0/rpc") cmds :headers (bearer token))]
    {:status (:status resp)
     :parsed (json/parse-string (:body resp) false)}))

(defn- rpc-line-ok? [parsed]
  (true? (get-in parsed ["results" 0 "ok"])))

(defn- ingest! [base-url token room thread delegate text]
  (rpc-batch! base-url token
              [{"Post" {"room" room
                        "thread_tag" thread
                        "delegate" delegate
                        "text" text
                        "return_rank_diff" false}}]))

(defn- register-user! [base-url session-agent username]
  (oauth/complete-registration! base-url
                                :agent session-agent
                                :username username
                                :assert! (fn [pred msg] (is pred msg))))

(defn grants-flow! []
  (println "\n━━━ grants enforcement integration check ━━━\n")

  (println "building server binary…")
  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-grants-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))

   (bind !server (atom nil))
   (bind !google (atom nil))

   (bind server-env (common/slug-server-env tmp-dir base-url google-url slug-port))
   (try
     (println (str "starting mock google on :" google-port))
     (reset! !google (oauth/start-mock-google google-port
                                              :google-users ["google-user-alice" "google-user-bob"]))

     (println (str "starting server on :" slug-port))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (println "\nregistering alice…")
     (let [alice-token (register-user! base-url
                                       "00000000-0000-0000-0000-000000000001:test:local/dev"
                                       "alice")
           _ (println "registering bob…")
           bob-token   (register-user! base-url
                                       "00000000-0000-0000-0000-000000000002:test:local/dev"
                                       "bob")

           ;; Alice creates a private room.
           _ (println "\nalice creates private room…")
           create (rpc-batch! base-url alice-token
                              [{"RoomCreate" {"slug" "secret-project"}}])
           _ (is (= 200 (:status create)) "room create HTTP 200")
           _ (is (rpc-line-ok? (:parsed create)) "room create RPC ok")
           room-id (get-in (:parsed create) ["results" 0 "result" "RoomCreated" "room_id"])
           _ (is (some? room-id) "room_id present in RPC result")]

       ;; Alice (owner) can post prose to her own private room.
       (println "\nalice posts prose to her private room…")
       (is (rpc-line-ok? (:parsed (ingest! base-url alice-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000001:test:local/dev"
                                                 "Hello from alice.")))
                "alice prose post succeeds")

       ;; Bob has no grants — RPC line fails.
       (println "\nbob (no grants) tries to post prose…")
       (is (not (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                      "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                      "Hello from bob, unauthorized."))))
                "bob without grants gets RPC failure")

       ;; Alice grants bob View only.
       (println "\nalice grants bob View only…")
       (is (rpc-line-ok? (:parsed (rpc-batch! base-url alice-token
                                                   [{"RoomGrant" {"room" room-id "username" "bob" "capabilities" ["view"]}}])))
                "grant View RPC ok")

       (println "\nbob (View only) tries to post prose…")
       (is (not (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                      "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                      "Hello from bob, view only."))))
                "bob with View but no Post gets RPC failure")

       ;; Alice grants bob Post.
       (println "\nalice grants bob Post…")
       (is (rpc-line-ok? (:parsed (rpc-batch! base-url alice-token
                                                   [{"RoomGrant" {"room" room-id "username" "bob" "capabilities" ["post"]}}])))
                "grant Post RPC ok")

       (println "\nbob (View + Post) posts prose…")
       (is (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                 "Hello from bob, now authorised.")))
                "bob with View + Post succeeds for prose")

       ;; Alice defines two items and votes in the private room.
       (println "\nalice posts items + vote to private room…")
       (is (rpc-line-ok? (:parsed (ingest! base-url alice-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000001:test:local/dev"
                                                 "{ A crisp red apple. }\n~/fruits/apple\n{ A yellow banana. }\n~/fruits/banana\n{ apples are better }\n~/fruits/apple > ~/fruits/banana")))
                "alice vote in private room succeeds")

       ;; Bob (View + Post, no Vote) tries to vote.
       (println "\nbob (no Vote) tries to vote…")
       (is (not (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                     "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                     "{ bob's take }
~/fruits/apple > ~/fruits/banana"))))
                "bob without Vote gets RPC failure")

       ;; Alice grants bob Vote.
       (println "\nalice grants bob Vote…")
       (is (rpc-line-ok? (:parsed (rpc-batch! base-url alice-token
                                                   [{"RoomGrant" {"room" room-id "username" "bob" "capabilities" ["vote"]}}])))
                "grant Vote RPC ok")

       (println "\nbob (View + Post + Vote) votes…")
       (is (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                 "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                 "{ bob's take }
~/fruits/apple > ~/fruits/banana")))
                "bob with Vote succeeds"))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn grants-test [& _args]
  (grants-flow!))

(deftest grants-enforcement-integration-check
  (grants-flow!))
