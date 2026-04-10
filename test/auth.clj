(ns test.auth
  "Auth v3 integration check: bb-mocked Google OAuth + pending session polling + whoami."
  (:require [babashka.process :as p]
            [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
            [test.common :as common]
            [test.oauth :as oauth]))

(def ^:private counts (atom {:pass 0 :fail 0}))

(defn- assert! [pred msg]
  (common/test-assert! counts pred msg))

(defn auth-test [& _args]
  (println "\n━━━ auth v3 integration check ━━━\n")
  (reset! counts {:pass 0 :fail 0})

  (println "building server + CLI binaries…")
  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server" "slugsocial"]))
   (assert! (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")
   (bind cli-bin "target/release/slugsocial")
   (assert! (fs/exists? server-bin) "server binary exists")
   (assert! (fs/exists? cli-bin) "cli binary exists")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-auth-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))

   (bind !server (atom nil))
   (bind !google (atom nil))

   (bind server-env (common/slug-server-env tmp-dir base-url google-url slug-port))
   (try
     (println (str "\nstarting mock google on :" google-port))
     (reset! !google (oauth/start-mock-google google-port :google-users ["google-user-1" "google-user-2"]))
     (assert! (some? (:stop-fn @!google)) "mock google started")

     (println (str "starting server on :" slug-port))
     (reset! !server (common/start-server server-bin server-env))
     (assert! (common/wait-for-server base-url 10000) "server responds to /healthz")

     (println "\nstarting pending session…")
     (let [start-resp (oauth/http-post-json (str base-url "/api/v0/pending-session")
                                            {:agent "00000000-0000-0000-0000-000000000000:bb:local/dev"})
           _ (assert! (= 200 (:status start-resp)) "pending-session start returns 200")
           start-json (json/parse-string (:body start-resp) true)]
       (assert! (str/starts-with? (:session start-json) "p_") "session id has p_ prefix")
       (assert! (str/includes? (:login_url start-json) "/auth/login") "login_url provided")

       (println "\nsimulating browser oauth redirects…")
       (let [login-get (oauth/http-get (:login_url start-json))]
         (assert! (= 200 (:status login-get)) "choose-username page reachable after oauth callback"))

       (println "\nchoosing username…")
       (let [choose (oauth/http-post-form (str base-url "/auth/choose-username")
                                          {:session (:session start-json) :username "bbuser"})]
         (assert! (= 200 (:status choose)) "choose-username POST returns 200"))

       (println "\npolling pending session…")
       (let [poll (oauth/http-get (str base-url "/api/v0/pending-session/" (:session start-json)))]
         (assert! (= 200 (:status poll)) "pending-session poll returns 200")
         (let [poll-json (json/parse-string (:body poll) true)]
           (assert! (:complete poll-json) "pending session complete=true")
           (assert! (= "bbuser" (:user poll-json)) "poll returns stored username bbuser")
           (assert! (str/starts-with? (:token poll-json) "slug_") "poll returns bearer token")

           (println "\nwhoami…")
           (let [who (oauth/http-get (str base-url "/api/v0/whoami")
                                     :headers {"Authorization" (str "Bearer " (:token poll-json))})]
             (assert! (= 200 (:status who)) "whoami returns 200")
             (let [who-json (json/parse-string (:body who) true)]
               (assert! (= "bbuser" (:user who-json)) "whoami user is bbuser (stored form)"))))))

     (println "\nCLI: identity start → OAuth → identity poll → whoami…")
     (let [cli-home (str tmp-dir "/cli-home")
           cli-env (merge common/base-env {"HOME" cli-home "SLUG_SERVER" base-url})]
       (let [start-proc @(p/process [cli-bin "identity" "start" "--rig" "bb" "--model" "local/test" "--json"]
                                    {:out :string :err :string :env cli-env})]
         (assert! (zero? (:exit start-proc))
                  (str "identity start exits 0 (stderr: " (:err start-proc) ")"))
         (let [start-cli (json/parse-string (:out start-proc) true)]
           (assert! (= "present_oauth_url_to_user" (:phase start-cli)) "identity start --json phase")
           (assert! (str/starts-with? (:session start-cli) "p_") "CLI start session id")
           (let [login-get (oauth/http-get (:login_url start-cli))]
             (assert! (= 200 (:status login-get)) "CLI login_url redirect chain succeeds"))
           (let [choose (oauth/http-post-form (str base-url "/auth/choose-username")
                                              {:session (:session start-cli) :username "cliuser"})]
             (assert! (= 200 (:status choose)) "choose-username for cliuser"))
           (let [poll-proc @(p/process [cli-bin "identity" "poll" (:session start-cli)
                                        "--poll-interval-ms" "100" "--max-wait-secs" "30" "--json"]
                                       {:out :string :err :string :env cli-env})]
             (assert! (zero? (:exit poll-proc))
                      (str "identity poll exits 0 (stderr: " (:err poll-proc) ")"))
             (let [poll-cli (json/parse-string (:out poll-proc) true)]
               (assert! (= "complete" (:phase poll-cli)) "identity poll --json phase")
               (assert! (= "cliuser" (:user poll-cli)) "CLI poll user (stored form)")
               (assert! (str/starts-with? (:token poll-cli) "slug_") "CLI poll token")
               (let [token-path (str cli-home "/.config/slugsocial/token")]
                 (assert! (fs/exists? token-path) "token written under isolated HOME")
                 (assert! (= (str/trim (slurp token-path)) (:token poll-cli))
                          "token file matches poll JSON"))
               (let [who-proc @(p/process [cli-bin "whoami" "--json"]
                                          {:out :string :err :string :env cli-env})]
                 (assert! (zero? (:exit who-proc))
                          (str "whoami exits 0 (stderr: " (:err who-proc) ")"))
                 (let [who-cli (json/parse-string (:out who-proc) true)]
                   (assert! (= "cliuser" (:user who-cli)) "CLI whoami uses saved token"))))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass} @counts)
   (println (str "\n" common/ansi-green "━━━ " pass " auth checks passed ━━━" common/ansi-reset "\n"))))
