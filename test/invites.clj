(ns test.invites
  "Ephemeral invite links: mint via RPC, GET /join → pending session + OAuth, redemption → GrantAdded,
  RoomAudit, post succeeds, second GET /join returns 404 when max_uses exhausted."
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
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

(defn- session-from-login-location [loc]
  (when loc
    (let [qpart (if (str/includes? loc "?")
                  (-> loc (str/split #"\?" 2) second)
                  "")
          qpart (if (str/includes? qpart "#")
                  (-> qpart (str/split #"#" 2) first)
                  qpart)
          m (oauth/parse-query qpart)]
      (some-> (get m :session) str))))

(defn- invite-token-from-url [invite-url]
  (second (re-find #"/join/(inv_[^/?#]+)" (str invite-url))))

(defn- register-user! [base-url session-agent username]
  (oauth/complete-registration! base-url
                                :agent session-agent
                                :username username
                                :assert! (fn [pred msg] (is pred msg))))

(defn- ingest! [base-url token room thread delegate text]
  (rpc-batch! base-url token
              [{"Post" {"room" room
                        "thread_tag" thread
                        "delegate" delegate
                        "text" text
                        "return_rank_diff" false}}]))

(defn invites-flow! []
  (println "\n━━━ ephemeral invite + audit integration check ━━━\n")

  (println "building server binary…")
  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-invites-"})))
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

           _ (println "\nalice creates private room…")
           create (rpc-batch! base-url alice-token
                              [{"RoomCreate" {"slug" "invite-demo"}}])
           _ (is (= 200 (:status create)) "room create HTTP 200")
           _ (is (rpc-line-ok? (:parsed create)) "room create RPC ok")
           room-id (get-in (:parsed create) ["results" 0 "result" "RoomCreated" "room_id"])
           _ (is (some? room-id) "room_id present")

           _ (println "\nalice mints invite (view,post,vote uses=1)…")
           mint (rpc-batch! base-url alice-token
                            [{"RoomMintInvite" {"room" room-id
                                                "capabilities" ["view" "post" "vote"]
                                                "max_uses" 1}}])
           _ (is (= 200 (:status mint)) "mint HTTP 200")
           _ (is (rpc-line-ok? (:parsed mint)) "mint RPC ok")
           invite-url (get-in (:parsed mint) ["results" 0 "result" "RoomInviteMinted" "invite_url"])
           inv-tok (invite-token-from-url invite-url)
           _ (is (some? inv-tok) "invite token parsed from URL")

           _ (println "\nGET /join/:token (expect redirect + session)…")
           join-resp (oauth/http-get-no-redirect (str base-url "/join/" inv-tok))
           _ (is (contains? #{302 307} (:status join-resp))
                      (str "join returns redirect (got status " (:status join-resp) ")"))
           sess (session-from-login-location (:location join-resp))
           _ (is (and (some? sess) (str/starts-with? sess "p_")) "Location carries session=p_…")

           _ (println "\nbob completes OAuth via invite session…")
           bob-token (oauth/complete-pending-session! base-url sess "bob"
                                                      :assert! (fn [pred msg] (is pred msg)))

           _ (println "\nalice runs RoomAudit…")
           audit (rpc-batch! base-url alice-token [{"RoomAudit" {"room" room-id}}])
           _ (is (rpc-line-ok? (:parsed audit)) "audit RPC ok")
           grants (get-in (:parsed audit) ["results" 0 "result" "RoomAudit" "grants"])
           bob-entry (first (filter #(= "bob" (get % "username")) grants))
           _ (is (some? bob-entry) "audit lists bob")
           bob-caps (set (get bob-entry "capabilities"))
           _ (is (= bob-caps #{"view" "post" "vote"}) "bob has view, post, vote")

           _ (println "\nbob posts prose to private room…")
           _ (is (rpc-line-ok? (:parsed (ingest! base-url bob-token room-id "main"
                                                      "00000000-0000-0000-0000-000000000002:test:local/dev"
                                                      "Hello via invite link.")))
                      "bob post succeeds")

           _ (println "\nsecond GET /join (invite exhausted → 404)…")
           join2 (oauth/http-get-no-redirect (str base-url "/join/" inv-tok))
           _ (is (= 404 (:status join2)) "exhausted invite returns 404")]

       (println "\ninvite lifecycle OK."))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn invites-test [& _args]
  (invites-flow!))

(deftest invites-integration-check
  (invites-flow!))

