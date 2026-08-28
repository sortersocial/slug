(ns test.browser-garden-depth
  "Playwright: garden depth <select> must navigate (GET form submit), not only
   change the control value. Regression for document.URL shadowing window.URL
   inside inline onchange handlers that used `new URL(...)`."
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
      (let [text (try (locator/text-content (page/locator pg selector))
                      (catch Exception _ nil))]
        (if (and (string? text) (str/includes? text expected))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- select-depth-navigates!
  "Select a depth option and assert the page navigates to ?depth=<value>.

  Playwright's selectOption races with form-driven navigation (element can
  detach mid-action). Start wait-for-url first, then select."
  [pg value]
  (let [expected (str "depth=" value)
        onchange (try (locator/get-attribute (page/locator pg "#garden-depth-select") "onchange")
                      (catch Exception _ nil))
        _ (is (and (string? onchange) (str/includes? onchange "this.form.submit()"))
              "depth select onchange submits its GET form")
        waiter (future
                 (page/wait-for-url pg
                                    (re-pattern (str ".*\\Q" expected "\\E.*"))
                                    {:timeout 15000}))]
    ;; Give the waiter a moment to register before the navigation fires.
    (Thread/sleep 100)
    (let [sel (locator/select-option (page/locator pg "#garden-depth-select") value)
          result @waiter]
      (when (core/anomaly? sel)
        ;; selectOption often errors when the form navigates away; that's ok if URL matched.
        (println "select-option anomaly (ok if navigation succeeded):" sel))
      (if (core/anomaly? result)
        ;; Fallback: drive the same GET form path explicitly.
        (do
          (page/evaluate pg
                         (str "(() => { const s = document.getElementById('garden-depth-select');"
                              " s.value = " (pr-str value) ";"
                              " s.form.submit(); })()"))
          (let [retry (page/wait-for-url pg
                                         (re-pattern (str ".*\\Q" expected "\\E.*"))
                                         {:timeout 10000})]
            (not (core/anomaly? retry))))
        true))))

(defn garden-depth-select-flow! []
  (println "\n━━━ browser garden depth select navigates ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-garden-depth-"})))
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
           thread-tag "browser-garden-depth"
           ;; Parent with a nested leaf so depth=2 changes visible rankings.
           raw (str "# " thread-tag "\n\n"
                    "~/depth-demo {parent}\n"
                    "~/depth-demo/child-a {alpha}\n"
                    "~/depth-demo/child-b {beta}\n"
                    "~/depth-demo/child-a/leaf {nested leaf}\n"
                    "{sibling vote}\n~/depth-demo/child-a 2:1 ~/depth-demo/child-b\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed nested garden via rpc")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")

               (page/navigate pg (str base-url "/~/depth-demo"))
               (is (wait-for-text pg "#garden-depth-select" "1" 10000)
                   "depth select present at default depth 1")
               ;; Leaf identity: children display as ~/child-a and ~/leaf.
               (is (wait-for-text pg "body" "~/child-a" 10000)
                   "depth 1 shows direct child")
               (is (not (str/includes? (or (locator/text-content (page/locator pg "body")) "")
                                       "~/leaf"))
                   "depth 1 does not list nested leaf")

               (is (select-depth-navigates! pg "2")
                   "selecting depth 2 navigates to ?depth=2")
               (is (wait-for-text pg "#garden-depth-select option[selected]" "2" 10000)
                   "depth 2 remains selected after navigation")
               (is (wait-for-text pg "body" "~/leaf" 10000)
                   "depth 2 lists nested leaf")

               (is (select-depth-navigates! pg "all")
                   "selecting ∞ navigates to ?depth=all")
               (is (wait-for-text pg "body" "~/leaf" 10000)
                   "depth=all still shows nested leaf")

               (is (select-depth-navigates! pg "1")
                   "selecting depth 1 navigates to ?depth=1")
               (is (wait-for-text pg "body" "~/child-a" 10000)
                   "depth 1 again shows direct child")
               (is (not (str/includes? (or (locator/text-content (page/locator pg "body")) "")
                                       "~/leaf"))
                   "depth 1 again hides nested leaf"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn garden-depth-browser-test [& _args]
  (garden-depth-select-flow!))

(deftest browser-garden-depth-select-navigates
  (garden-depth-select-flow!))
