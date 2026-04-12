(ns test.browser-ui-morph
  "Playwright (Spel) checks for JS responses that morph the DOM — e.g. POST /ui __rpc__
   expand_post_full. These paths are not covered by HTTP-only integration tests."
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

(defn expand-post-full-via-ui-rpc-test [& _args]
  (println "\n━━━ browser UI morph: expand_post_full (__rpc__ + POST /ui) ━━━\n")
  (reset! counts {:pass 0 :fail 0})

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (assert! (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-ui-morph-"})))
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
     (assert! (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           thread-tag "ui-morph-long"
           ;; Truncation kicks in when ing.raw len > 2000 (forum markup).
           tail "END_UI_MORPH_TAIL_MARKER"
           long-body (str "# " thread-tag "\n\n"
                          (str/join (repeat 400 "abcdefghij"))
                          tail)
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" long-body
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (assert! (true? (get-in post-json ["results" 0 "ok"])) "seed long post via rpc")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (assert! (wait-for-text pg "body" "@alice" 15000) "alice session after login")
               (page/navigate pg (str base-url "/t/" thread-tag))
               (assert! (wait-for-text pg "#thread-feed-region" "[show full post]" 15000)
                        "truncated card shows expand link")
               (let [before (or (locator/text-content (page/locator pg "#thread-feed-region")) "")]
                 (assert! (not (str/includes? before tail))
                          "tail marker not visible before expand"))
               (locator/click (page/locator pg ".show-full-link"))
               (assert! (wait-for-text pg "#thread-feed-region" tail 15000)
                        "POST /ui morph reveals full body (tail marker visible)"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   (bind {pass :pass fail :fail} @counts)
   (if (zero? fail)
     (println (str "\n" common/ansi-green "━━━ " pass " UI morph browser checks passed ━━━" common/ansi-reset "\n"))
     (do (println (str "\n" common/ansi-red "━━━ " fail " UI morph browser checks FAILED ━━━" common/ansi-reset "\n"))
         (System/exit 1)))))
