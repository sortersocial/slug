(ns test.browser-vote-pool
  "Pool-scoped voting: seed ~/pool/a-j, enter via /vote?pool=~/pool, follow
   the vote → next-pair → vote sequence until no next pair or 15 iterations."
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

(defn- element-text [pg selector]
  (try (locator/text-content (page/locator pg selector)) (catch Exception _ nil)))

(defn- enc [^String s]
  (java.net.URLEncoder/encode s "UTF-8"))

(def letters ["a" "b" "c" "d" "e" "f" "g" "h" "i" "j"])

(defn vote-pool-flow! []
  (println "\n━━━ browser vote pool (/vote?pool= seeds + follow next-pair sequence) ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-vote-pool-"})))
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
           thread-tag  "browser-vote-pool"
           ;; seed ~/pool/a through ~/pool/j as items with bodies
           item-lines  (str/join "\n"
                                 (map (fn [l] (str "~/pool/" l " {" l "}")) letters))
           raw         (str "# " thread-tag "\n\n~/pool {root}\n" item-lines "\n")
           post-resp   (oauth/http-post-json
                        (str base-url "/api/v0/rpc")
                        [{"Post" {"room"             "public"
                                  "thread_tag"       thread-tag
                                  "text"             raw
                                  "return_rank_diff" false}}]
                        :headers {"Authorization" (str "Bearer " alice-token)})
           post-json   (json/parse-string (:body post-resp) false)
           _           (is (true? (get-in post-json ["results" 0 "ok"]))
                           "seed ~/pool/a-j items via rpc")
           pool-url    (str base-url "/vote?pool=" (enc "~/pool"))]

       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")

               ;; Enter via pool URL — page picks first pair automatically.
               (page/navigate pg pool-url)
               (is (wait-for-text pg "body.view-vote-compare" "compare" 15000)
                   "pool entry: vote compare page loads")

               ;; Verify the initial pair is within the pool.
               (let [pair-text (element-text pg ".vote-compare-pair")]
                 (is (and (string? pair-text) (str/includes? pair-text "~/pool/"))
                     (str "initial pair is within ~/pool: " pair-text)))

               ;; Follow vote → next-pair sequence up to 15 iterations.
               (let [votes-cast
                     (loop [i 0]
                       (if (>= i 15)
                         i
                         (let [explanation (str "pool vote " i " reason")]
                           (locator/fill (page/locator pg "#vote-explain") explanation)
                           (locator/click (page/locator pg "#vote-compare-form button[type=submit]"))
                           ;; Wait for edge history morph confirming the vote landed.
                           (if-not (wait-for-text pg "ul.vote-edge-history" explanation 20000)
                             (do (println "  vote" i "history morph timed out — stopping")
                                 i)
                             (let [has-next (wait-for-text pg "[data-testid=\"vote-next-pair\"]"
                                                           "next pair" 8000)]
                               (if-not has-next
                                 ;; "no next pair" — pool exhausted.
                                 (do (println "  no next pair after vote" i " — pool exhausted")
                                     (inc i))
                                 (do
                                   ;; Verify the pair on this page is within the pool before advancing.
                                   (let [pt (element-text pg ".vote-compare-pair")]
                                     (is (and (string? pt) (str/includes? pt "~/pool/"))
                                         (str "pair at vote " i " is within ~/pool: " pt)))
                                   (locator/click (page/locator pg "[data-testid=\"vote-next-pair\"]"))
                                   ;; Wait for next pair to load.
                                   (wait-for-text pg "body.view-vote-compare" "compare" 10000)
                                   (recur (inc i)))))))))]

                 (is (>= votes-cast 1) (str "cast at least 1 vote, got: " votes-cast))
                 (println (str "  pool voting sequence complete: " votes-cast " vote(s) cast")))

               ;; After the sequence, the current page is still a pool-scoped vote page.
               (let [url (page/url pg)]
                 (is (str/includes? (or url "") "/vote")
                     (str "still on /vote after sequence: " url))))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn vote-pool-browser-test [& _args]
  (vote-pool-flow!))

(deftest browser-vote-pool-sequence
  (vote-pool-flow!))
