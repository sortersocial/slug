(ns test.browser-arena-resolver
  "Playwright coverage for on-demand are.na channel imports."
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
            [clojure.test :refer [deftest is]]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as locator]
            [com.blockether.spel.page :as page]
            [org.httpkit.server :as http]
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

(defn- wait-for-http-text [url expected timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (try
                   (:body (oauth/http-get url))
                   (catch Exception _ nil))]
        (if (and (string? text) (str/includes? text expected))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- wait-for-http-absent [url needle timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (try
                   (:body (oauth/http-get url))
                   (catch Exception _ nil))]
        (if (and (string? text) (not (str/includes? text needle)))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- block-json [id title type]
  {:id id
   :type type
   :title title
   :user {:name "Ada"}
   :connection {:connected_by {:name "Charles"}
                :connected_at "2024-01-01T00:00:00Z"}})

(defn- start-mock-arena [port !entries]
  (let [!paths (atom [])
        handler (fn [req]
                  (swap! !paths conj (:uri req))
                  (case (:uri req)
                    "/v3/channels/my-chan/contents"
                    {:status 200
                     :headers {"Content-Type" "application/json"}
                     :body (json/generate-string
                            {:meta {:current_page 1 :has_more_pages false}
                             :data (vec @!entries)})}

                    {:status 404 :body "not found"}))
        stop-fn (http/run-server handler {:port port})]
    {:stop-fn stop-fn :paths !paths}))

(defn arena-resolver-flow! []
  (println "\n━━━ browser are.na resolver: on-demand channel import ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-arena-resolver-"})))
   (bind slug-port (common/pick-port))
   (bind google-port (common/pick-port))
   (bind arena-port (common/pick-port))
   (bind base-url (str "http://127.0.0.1:" slug-port))
   (bind google-url (str "http://127.0.0.1:" google-port))
   (bind arena-url (str "http://127.0.0.1:" arena-port))

   (bind !server (atom nil))
   (bind !google (atom nil))
   (bind !arena (atom nil))
   (bind !entries (atom [(block-json 1 "First block" "Link")
                         (block-json 2 "Second block" "Image")
                         {:id 9 :type "Channel" :title "Nested" :slug "nested-chan"
                          :counts {:contents 3}}]))
   (bind server-env (assoc (common/slug-server-env tmp-dir base-url google-url slug-port)
                           "SLUG_ARENA_API_BASE_URL" arena-url
                           "SLUG_ARENA_RESOLVER_COOLDOWN_MS" "0"))
   (try
     (reset! !google (oauth/start-mock-google google-port
                                              :google-users ["google-user-alice"]))
     (reset! !arena (start-mock-arena arena-port !entries))
     (reset! !server (common/start-server server-bin server-env))
     (is (common/wait-for-server base-url 10000) "server responds to /healthz")

     (let [alice-token (oauth/fetch-bearer-token! base-url :username "alice")
           seed-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" "arena-resolver-seed"
                                "text" "https://www.are.na/channel/my-chan {Seeded are.na channel}"
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           seed-json (json/parse-string (:body seed-resp) false)]
       (is (true? (get-in seed-json ["results" 0 "ok"])) "seed pasted are.na channel")

       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")

               (page/navigate pg (str base-url "/-/https://www.are.na/channel/my-chan"))
               (is (wait-for-text pg "#external-resolver-panel" "Are.na resolver" 15000)
                   "channel page shows are.na resolver panel")
               (locator/click (page/locator pg "[data-testid=\"arena-resolve-children\"]"))
               (is (wait-for-http-text (str base-url "/-/https://www.are.na/channel/my-chan")
                                       "-/https://www.are.na/block/1"
                                       15000)
                   "channel resolver imports block 1")
               (is (wait-for-http-text (str base-url "/-/https://www.are.na/channel/my-chan")
                                       "-/https://www.are.na/block/2"
                                       15000)
                   "channel resolver imports block 2")
               (is (wait-for-http-text (str base-url "/-/https://www.are.na/channel/my-chan")
                                       "-/https://www.are.na/channel/nested-chan"
                                       15000)
                   "channel resolver imports nested channels")

               ;; Block 2 leaves the channel on are.na; refresh should redact its system post.
               (reset! !entries [(block-json 1 "First block" "Link")])
               (page/navigate pg (str base-url "/-/https://www.are.na/channel/my-chan"))
               (locator/click (page/locator pg "[data-testid=\"arena-resolve-children\"]"))
               (is (wait-for-http-absent (str base-url "/-/https://www.are.na/channel/my-chan")
                                         "-/https://www.are.na/block/2"
                                         15000)
                   "removed block leaves the garden after refresh")
               (is (wait-for-http-absent (str base-url "/-/https://www.are.na/channel/my-chan")
                                         "-/https://www.are.na/channel/nested-chan"
                                         15000)
                   "removed nested channel leaves the garden after refresh")
               (is (wait-for-http-text (str base-url "/-/https://www.are.na/channel/my-chan")
                                       "-/https://www.are.na/block/1"
                                       15000)
                   "still-connected block kept after refresh")

               ;; Legacy /:user/:channel URLs resolve onto the canonical channel item.
               (page/navigate pg (str base-url "/-/https://www.are.na/some-user/my-chan"))
               (is (wait-for-text pg "#external-resolver-panel" "Are.na resolver" 15000)
                   "legacy channel URL shows are.na resolver panel")
               (locator/click (page/locator pg "[data-testid=\"arena-resolve-children\"]"))
               (is (wait-for-text pg "body" "-/https://www.are.na/block/1" 15000)
                   "legacy URL import lands on canonical channel page with children")))))

       (is (some #{"/v3/channels/my-chan/contents"} @(:paths @!arena))
           "mock are.na saw channel contents request")
       (is (>= (count (filter #{"/v3/channels/my-chan/contents"} @(:paths @!arena))) 2)
           "contents endpoint hit on import and refresh"))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (when-some [a @!arena] ((:stop-fn a)))))))

(deftest browser-arena-resolver-test
  (arena-resolver-flow!))
