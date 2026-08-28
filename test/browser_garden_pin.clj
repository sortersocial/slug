(ns test.browser-garden-pin
  "Pin uses POST /ui + __rpc__ set_garden_pin (303 + Set-Cookie), not a separate route.
   Seeds two ontology items in public, pins one from the item page, asserts HUD + vote link on the other."
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

(defn- wait-for-absence-substr [pg selector substr timeout-ms]
  "True once selector text does not contain substr (or is empty), or timeout."
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (locator/text-content (page/locator pg selector))]
        (if (not (and (string? text) (str/includes? text substr)))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn garden-pin-flow! []
  (println "\n━━━ browser garden pin (POST /ui set_garden_pin + HUD + vote link) ━━━\n")

  (common/letlocals
   (bind build (common/run-cargo-build-release! ["slugsocial-server"]))
   (is (zero? (:exit build)) "cargo build succeeds")
   (bind server-bin "target/release/slugsocial-server")

   (bind tmp-dir (str (fs/create-temp-dir {:prefix "slug-browser-garden-pin-"})))
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
           thread-tag "browser-garden-pin"
           raw (str "# " thread-tag "\n\n"
                    "~/gp-parent {parent}\n"
                    "~/gp-parent/pin-a {alpha}\n"
                    "~/gp-parent/pin-b {beta}\n"
                    "{pin test vote}\n"
                    "~/gp-parent/pin-a 2:1 ~/gp-parent/pin-b\n")
           post-resp (oauth/http-post-json
                      (str base-url "/api/v0/rpc")
                      [{"Post" {"room" "public"
                                "thread_tag" thread-tag
                                "text" raw
                                "return_rank_diff" false}}]
                      :headers {"Authorization" (str "Bearer " alice-token)})
           post-json (json/parse-string (:body post-resp) false)
           _ (is (true? (get-in post-json ["results" 0 "ok"])) "seed public garden items via rpc")]
       (core/with-playwright [pw]
         (core/with-browser [browser (core/launch-chromium pw {:headless true :channel "chrome"})]
           (core/with-context [ctx (core/new-context browser)]
             (core/with-page [pg (core/new-page-from-context ctx)]
               (page/navigate pg (str base-url "/login"))
               (is (wait-for-text pg "body" "@alice" 15000) "alice session after login")
               ;; Nested URL must 308 to the leaf; do not stay on /~/gp-parent/pin-a.
               (page/navigate pg (str base-url "/~/gp-parent/pin-a"))
               (let [url-a (or (page/url pg) "")]
                 (is (str/includes? url-a "/~/pin-a")
                     (str "nested /~/gp-parent/pin-a 308s to /~/pin-a (got " url-a ")"))
                 (is (not (str/includes? url-a "/~/gp-parent/pin-a"))
                     (str "must not remain on nested pin-a URL (got " url-a ")")))
               (is (wait-for-text pg ".ont-item-shell" "~/pin-a" 15000) "on leaf item a page")
               ;; Native form POST /ui (data-navigate=full) — not fetch/eval
               (locator/click (page/locator pg ".ont-item-pin-zone form.ont-pin-form button[type=submit]"))
               (is (wait-for-text pg "#slug-pin-hud" "~/pin-a" 15000)
                   "HUD shows leaf pin label after redirect")
               (page/navigate pg (str base-url "/~/gp-parent/pin-b"))
               (let [url-b (or (page/url pg) "")]
                 (is (str/includes? url-b "/~/pin-b")
                     (str "nested /~/gp-parent/pin-b 308s to /~/pin-b (got " url-b ")"))
                 (is (not (str/includes? url-b "/~/gp-parent/pin-b"))
                     (str "must not remain on nested pin-b URL (got " url-b ")")))
               (is (wait-for-text pg ".ont-item-shell" "~/pin-b" 15000) "on leaf item b page")
               (is (wait-for-text pg "a.ont-vote-compare-btn" "vote" 10000)
                   "vote link visible vs pinned item")
               ;; Unpin from ranked child groups on parent page (parent is already a leaf)
               (page/navigate pg (str base-url "/~/gp-parent"))
               (is (wait-for-text pg ".ont-tab-panel-children" "ranked child groups" 15000)
                   "parent page shows ranked child groups")
               (is (wait-for-text pg ".ont-ranking-list button.ont-garden-pin-ico-active" "📌" 10000)
                   "pinned row shows active pin in ranked list")
               (locator/click (page/locator pg ".ont-ranking-list button.ont-garden-pin-ico-active"))
               (is (wait-for-absence-substr pg "#slug-pin-hud" "~/pin-a" 15000)
                   "HUD clears after unpin from ranked child list")
               (is (wait-for-absence-substr pg ".ont-ranking-list" "ont-garden-vote-ico" 10000)
                   "vote icons removed from ranked list after unpin")
               (page/navigate pg (str base-url "/~/gp-parent/pin-b"))
               (is (str/includes? (or (page/url pg) "") "/~/pin-b")
                   "pin-b nested URL still 308s after unpin")
               (is (wait-for-text pg ".ont-item-shell" "~/pin-b" 15000) "on leaf item b page again")
               (is (wait-for-absence-substr pg "body" "ont-vote-compare-btn" 15000)
                   "compare vote CTA removed after ranked-list unpin")
               ;; HUD unpin still works from item page context
               (page/navigate pg (str base-url "/~/gp-parent/pin-a"))
               (is (str/includes? (or (page/url pg) "") "/~/pin-a")
                   "re-pin navigation lands on leaf /~/pin-a")
               (locator/click (page/locator pg ".ont-item-pin-zone form.ont-pin-form button[type=submit]"))
               (is (wait-for-text pg "#slug-pin-hud" "~/pin-a" 15000)
                   "re-pin from leaf item page for HUD unpin test")
               (page/navigate pg (str base-url "/~/gp-parent/pin-b"))
               (locator/click (page/locator pg "#slug-pin-hud button.slug-pin-hud-unpin-btn"))
               (is (wait-for-absence-substr pg "#slug-pin-hud" "~/pin-a" 15000)
                   "HUD clears after unpin from HUD button")
               (is (wait-for-absence-substr pg "body" "ont-vote-compare-btn" 15000)
                   "compare vote CTA removed after HUD unpin on same reload")
               (is (str/includes? (or (page/url pg) "") "/~/pin-b")
                   "still on leaf item b after HUD unpin")
               (is (not (str/includes? (or (page/url pg) "") "/~/gp-parent/pin-b"))
                   "HUD unpin must not revive the nested pin-b URL"))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn garden-pin-browser-test [& _args]
  (garden-pin-flow!))

(deftest browser-garden-pin-set-hud-vote-link
  (garden-pin-flow!))
