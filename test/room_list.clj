(ns test.room-list
  "Room list integration test: list rooms user has access to via POST /api/v0/rpc.

  Covers:
  - user with no rooms -> empty list
  - user with one room -> list contains that room
  - user with multiple rooms -> list contains all rooms"
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.set :as set]
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

(defn- register-user! [base-url session-agent username]
  (oauth/complete-registration! base-url
                                :agent session-agent
                                :username username
                                :assert! (fn [pred msg] (is pred msg))))

(defn room-list-flow! []
  (println "\n━━━ room list integration check ━━━\n")

  (println "building server binary…")
  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-room-list-"})))
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
                                              :google-users ["google-user-alice"
                                                             "google-user-bob"
                                                             "google-user-carol"]))

     (println (str "starting server on :" slug-port))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (println "\nregistering alice, bob, carol…")
     (let [alice-token (register-user! base-url
                                       "00000000-0000-0000-0000-000000000001:test:local/dev"
                                       "alice")
           bob-token   (register-user! base-url
                                       "00000000-0000-0000-0000-000000000002:test:local/dev"
                                       "bob")
           carol-token (register-user! base-url
                                       "00000000-0000-0000-0000-000000000003:test:local/dev"
                                       "carol")

           ;; Alice creates two private rooms
           _ (println "\nalice creates two rooms…")
           room-id-1 (-> (rpc-batch! base-url alice-token [{"RoomCreate" {"slug" "alice-room-one"}}])
                         (get-in [:parsed "results" 0 "result" "RoomCreated" "room_id"]))
           _ (is (some? room-id-1) "alice room-one created")
           room-id-2 (-> (rpc-batch! base-url alice-token [{"RoomCreate" {"slug" "alice-room-two"}}])
                         (get-in [:parsed "results" 0 "result" "RoomCreated" "room_id"]))
           _ (is (some? room-id-2) "alice room-two created")

           ;; Carol creates her own room
           _ (println "carol creates her own room…")
           carol-room (-> (rpc-batch! base-url carol-token [{"RoomCreate" {"slug" "carol-room"}}])
                          (get-in [:parsed "results" 0 "result" "RoomCreated" "room_id"]))
           _ (is (some? carol-room) "carol room created")]

       ;; --- isolation: alice only sees her rooms, not carol's ---
       (println "\nalice sees her 2 rooms but not carol's…")
       (let [rooms (-> (rpc-batch! base-url alice-token ["RoomList"])
                       (get-in [:parsed "results" 0 "result" "RoomList" "rooms"])
                       set)]
         (is (= #{room-id-1 room-id-2} rooms)
                  "alice sees exactly her 2 rooms")
         (is (not (contains? rooms carol-room))
                  "alice does NOT see carol's room"))

       ;; --- isolation: carol only sees her room, not alice's ---
       (println "carol sees only her room…")
       (let [rooms (-> (rpc-batch! base-url carol-token ["RoomList"])
                       (get-in [:parsed "results" 0 "result" "RoomList" "rooms"])
                       set)]
         (is (= #{carol-room} rooms)
                  "carol sees exactly her own room")
         (is (not (contains? rooms room-id-1))
                  "carol does NOT see alice's room-one")
         (is (not (contains? rooms room-id-2))
                  "carol does NOT see alice's room-two"))

       ;; --- bob sees nothing yet: alice has 3 rooms total but bob is in none ---
       (println "bob (no grants) sees no rooms despite 3 existing…")
       (let [rooms (-> (rpc-batch! base-url bob-token ["RoomList"])
                       (get-in [:parsed "results" 0 "result" "RoomList" "rooms"]))]
         (is (zero? (count rooms))
                  "bob sees 0 rooms even though 3 exist in the system"))

       ;; --- partial grant: alice grants bob room-one only ---
       (println "\nalice grants bob view on room-one only…")
       (is (rpc-line-ok? (:parsed (rpc-batch! base-url alice-token
                                                   [{"RoomGrant" {"room" room-id-1
                                                                  "username" "bob"
                                                                  "capabilities" ["view"]}}])))
                "grant ok")

       ;; bob sees room-one but NOT room-two or carol's room
       (println "bob sees room-one but not room-two or carol's room…")
       (let [rooms (-> (rpc-batch! base-url bob-token ["RoomList"])
                       (get-in [:parsed "results" 0 "result" "RoomList" "rooms"])
                       set)]
         (is (= #{room-id-1} rooms)
                  "bob sees exactly room-one")
         (is (not (contains? rooms room-id-2))
                  "bob does NOT see alice's room-two (not granted)")
         (is (not (contains? rooms carol-room))
                  "bob does NOT see carol's room (not granted)"))

       ;; alice's view is unchanged
       (println "alice's view unchanged after granting bob…")
       (let [rooms (-> (rpc-batch! base-url alice-token ["RoomList"])
                       (get-in [:parsed "results" 0 "result" "RoomList" "rooms"])
                       set)]
         (is (= #{room-id-1 room-id-2} rooms)
                  "alice still sees exactly her 2 rooms after granting bob")))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn room-list-test [& _args]
  (room-list-flow!))

(deftest room-list-integration-check
  (room-list-flow!))
