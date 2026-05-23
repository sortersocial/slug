(ns test.browser-vote-pool
  "Pool-scoped voting: seed ~/pool/a-j (10 letters), follow the
   vote → next-pair sequence for all C(10,2)=45 pairs voting the
   alphabetically-earlier item each time, then assert the garden
   ranking is a…j in order."
  (:require [babashka.fs :as fs]
            [cheshire.core :as json]
            [clojure.string :as str]
            [clojure.test :refer [deftest is]]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as locator]
            [com.blockether.spel.page :as page]
            [test.common :as common]
            [test.oauth :as oauth]))

(def letters ["a" "b" "c" "d" "e" "f" "g" "h" "i" "j"])
(def total-pairs (/ (* (count letters) (dec (count letters))) 2)) ; C(10,2) = 45

(defn- wait-for-text [pg selector expected timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [text (try (locator/text-content (page/locator pg selector)) (catch Exception _ nil))]
        (if (and (string? text) (str/includes? text expected))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 150) (recur))
            false))))))

(defn- element-text [pg selector]
  (try (locator/text-content (page/locator pg selector)) (catch Exception _ "")))

(defn- enc [^String s]
  (java.net.URLEncoder/encode s "UTF-8"))

;; Extract the terminal path segment, e.g. "~/pool/c" → "c".
(defn- leaf [path] (last (str/split path #"/")))

;; Set the hidden ratio inputs so the alphabetically-earlier item wins.
(defn- set-ratio! [pg left-text right-text]
  (let [[rl rr] (if (neg? (compare (leaf left-text) (leaf right-text)))
                  [99 1]   ; left is earlier → prefer left
                  [1 99])] ; right is earlier → prefer right
    (page/evaluate pg (str "document.getElementById('vote-ratio-left').value='" rl "'"))
    (page/evaluate pg (str "document.getElementById('vote-ratio-right').value='" rr "'"))))

(defn vote-pool-flow! []
  (println (str "\n━━━ browser vote pool (all " total-pairs " pairs → sorted ranking) ━━━\n"))

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

               (page/navigate pg pool-url)
               (is (wait-for-text pg "body.view-vote-compare" "compare" 15000)
                   "pool entry: vote compare page loads")

               ;; Vote all 45 pairs, always preferring the alphabetically-earlier item.
               (loop [votes-cast 0]
                 (when (< votes-cast total-pairs)
                   (let [left-text  (element-text pg ".vote-compare-left code")
                         right-text (element-text pg ".vote-compare-right code")]
                     (is (str/includes? left-text "~/pool/")
                         (str "vote " votes-cast ": left is in pool: " left-text))
                     (is (str/includes? right-text "~/pool/")
                         (str "vote " votes-cast ": right is in pool: " right-text))
                     (set-ratio! pg left-text right-text)
                     (let [winner (if (neg? (compare (leaf left-text) (leaf right-text)))
                                    (leaf left-text) (leaf right-text))]
                       (locator/fill (page/locator pg "#vote-explain")
                                     (str "prefer " winner)))
                     (locator/click (page/locator pg "#vote-compare-form button[type=submit]"))
                     (is (wait-for-text pg "ul.vote-edge-history" "prefer " 20000)
                         (str "vote " votes-cast " appears in edge history"))
                     (when (< (inc votes-cast) total-pairs)
                       (is (wait-for-text pg "[data-testid=\"vote-next-pair\"]" "next pair" 8000)
                           (str "next pair available after vote " votes-cast))
                       (locator/click (page/locator pg "[data-testid=\"vote-next-pair\"]"))
                       (is (wait-for-text pg "body.view-vote-compare" "compare" 10000)
                           (str "vote page loaded for pair " (inc votes-cast))))
                     (recur (inc votes-cast)))))

               (println (str "  cast all " total-pairs " votes"))

               ;; Query the ranking via RPC and assert alphabetical order.
               (let [rank-resp  (oauth/http-post-json
                                 (str base-url "/api/v0/rpc")
                                 [{"GetGardenRank" {"room"        "public"
                                                    "parent_path" "~/pool"}}]
                                 :headers {"Authorization" (str "Bearer " alice-token)})
                     rank-json  (json/parse-string (:body rank-resp) true)
                     result     (get-in rank-json [:results 0 :result :GardenRank])
                     components (:components result)
                     unranked   (:unranked_items result)
                     ranked     (mapv :item (mapcat :ranking components))
                     ranked-leaves (mapv #(last (str/split % #"[/~]+")) ranked)]
                 (is (= 1 (count components))
                     (str "all 10 items form one connected component (got " (count components) ")"))
                 (is (empty? unranked)
                     (str "no unranked items (got " (count unranked) ")"))
                 (is (= 10 (count ranked))
                     (str "10 items ranked (got " (count ranked) ")"))
                 (is (= letters ranked-leaves)
                     (str "ranking is alphabetical a→j (got " ranked-leaves ")"))))))))

     (finally
       (when-some [s @!server] (common/kill-server s))
       (when-some [g @!google] ((:stop-fn g)))
       (fs/delete-tree tmp-dir)))

   nil))

(defn vote-pool-browser-test [& _args]
  (vote-pool-flow!))

(deftest browser-vote-pool-sequence
  (vote-pool-flow!))
