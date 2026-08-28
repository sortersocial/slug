(ns test.browser-draft-autosave
  "Playwright checks for compose draft autosave in localStorage."
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

(defn- draft-ls-key [draft-key]
  (str "slug-draft:" draft-key))

(defn- wait-for-local-storage [pg ls-key pred timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [raw (page/evaluate pg (str "localStorage.getItem(" (pr-str ls-key) ")"))]
        (if (pred raw)
          raw
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            nil))))))

(defn- textarea-value [pg selector]
  (page/evaluate pg (str "(document.querySelector(" (pr-str selector) ") || {}).value")))

(defn draft-autosave-flow! []
  (println "\n━━━ browser compose draft autosave (localStorage) ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-draft-"})))
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
           thread-tag "browser-draft-autosave"
           draft-text "PLAYWRIGHT_DRAFT_AUTOSAVE_MARKER"
           draft-key (str "reply:public/" thread-tag)
           ls-key (draft-ls-key draft-key)
           seed-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" (str "# " thread-tag "\n\nseed post for draft test")
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           seed-json (json/parse-string (:body seed-resp) false)
           _ (is (true? (get-in seed-json ["results" 0 "ok"])) "seed thread via rpc")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")

               ;; Thread reply compose: autosave + restore on reload
               (page/navigate pg (str base-url "/t/" thread-tag))
               (is (wait-for-text pg "#thread-compose" "post" 15000) "thread compose visible")
               (locator/fill (page/locator pg "#thread-compose textarea") draft-text)
               (let [stored (wait-for-local-storage
                             pg ls-key
                             (fn [raw]
                               (when raw
                                 (try
                                   (= draft-text (get (json/parse-string raw false) "text"))
                                   (catch Exception _ false))))
                             5000)]
                 (is (string? stored) "draft saved to localStorage after typing"))
               (page/reload pg)
               (is (wait-for-text pg "#thread-compose" "post" 15000) "compose after reload")
               (is (= draft-text (textarea-value pg "#thread-compose textarea"))
                   "draft restored into textarea after reload")

               ;; Successful post clears draft
               (locator/click (page/locator pg "#thread-compose-form button[type=submit]"))
               (is (wait-for-text pg "#thread-feed-region" draft-text 20000)
                   "posted draft text appears in feed")
               (let [cleared (wait-for-local-storage
                              pg ls-key
                              (fn [raw] (nil? raw))
                              5000)]
                 (is (nil? cleared) "draft removed from localStorage after successful post"))

               ;; New thread compose: tag + body persist across reload when expanded
               (page/navigate pg (str base-url "/t"))
               (is (wait-for-text pg "#new-thread-ui-slot" "+" 15000) "new thread slot on /t")
               (locator/click (page/locator pg "#new-thread-ui-slot button.form-toggle"))
               (is (wait-for-text pg "#new-thread-compose" "create thread" 15000) "new thread compose expanded")
               (locator/fill (page/locator pg "#new-thread-tag") "draft-tag-slug")
               (locator/fill (page/locator pg "#new-thread-compose textarea") "draft new thread body")
               (let [new-ls-key (draft-ls-key "new-thread:public")
                     stored (wait-for-local-storage
                             pg new-ls-key
                             (fn [raw]
                               (when raw
                                 (let [d (json/parse-string raw false)]
                                   (and (= "draft-tag-slug" (get d "thread_tag"))
                                        (= "draft new thread body" (get d "text"))))))
                             5000)]
                 (is (string? stored) "new-thread draft saved to localStorage"))
               (page/reload pg)
               (locator/click (page/locator pg "#new-thread-ui-slot button.form-toggle"))
               (is (wait-for-text pg "#new-thread-compose" "create thread" 15000) "new thread compose re-expanded")
               (is (= "draft-tag-slug" (textarea-value pg "#new-thread-tag"))
                   "new-thread tag restored after reload")
               (is (= "draft new thread body" (textarea-value pg "#new-thread-compose textarea"))
                   "new-thread body restored after reload"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn draft-autosave-browser-test [& _args]
  (draft-autosave-flow!))

(deftest browser-compose-draft-autosave
  (draft-autosave-flow!))
