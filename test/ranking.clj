(ns test.ranking
  "Drives sorterc on .sorter fixtures and asserts ranking properties.

   Regression coverage for issue #146 — pure forward star at default ratio
   (2:1 for `>`) used to produce tied uniform scores because the random walk
   on the normalized edge weights was bipartite. Fixed by switching to the
   degree-based d_max from Negahban–Oh–Shah rank centrality (§3.1)."
  (:require [clojure.test :refer [deftest is testing]]
            [babashka.process :as p]
            [cheshire.core :as json]
            [clojure.java.io :as io]))

(def sorterc-bin
  "Path to the locally-built sorterc binary. Builds on demand if missing."
  (let [dbg     "target/debug/sorterc"
        release "target/release/sorterc"]
    (cond
      (.exists (io/file release)) release
      (.exists (io/file dbg))     dbg
      :else
      (do (println "building sorterc…")
          (let [r (p/shell {:out :string :err :string :continue true}
                           "cargo build -p sorterc")]
            (when-not (zero? (:exit r))
              (throw (ex-info "cargo build -p sorterc failed"
                              {:stderr (:err r)}))))
          dbg))))

(defn compile-sorter [fixture-path]
  (let [{:keys [out exit]} (p/shell {:out :string :err :string :continue true}
                                    sorterc-bin "compile" fixture-path)]
    (when-not (zero? exit)
      (throw (ex-info "sorterc exit nonzero" {:fixture fixture-path :out out})))
    (json/parse-string out true)))

(defn first-component-ranking [result]
  (-> result :rankings first :components first :ranking))

(defn item-leaf [item]
  (last (clojure.string/split item #"/")))

(deftest chain-ranks-head-first
  (let [ranking (first-component-ranking (compile-sorter "test/fixtures/ranking/chain.sorter"))
        names   (mapv (comp item-leaf :item) ranking)]
    (is (= ["a" "b" "c"] names)
        "chain a>b>c should rank a, b, c in order")
    (is (apply > (map :score ranking))
        "scores should attenuate strictly down the chain")))

(deftest inverse-star-puts-winners-on-top
  (let [ranking (first-component-ranking (compile-sorter "test/fixtures/ranking/star_inverse.sorter"))
        names   (mapv (comp item-leaf :item) ranking)]
    (is (= "win" (last names))
        "the item that lost to both others should be ranked last")))

(deftest cycle-produces-uniform-scores
  (let [ranking (first-component-ranking (compile-sorter "test/fixtures/ranking/cycle.sorter"))
        scores  (map :score ranking)]
    (is (every? #(< (Math/abs (- % 1/3)) 1e-3) scores)
        "a perfectly symmetric 3-cycle should give every node ~1/3")))

(deftest star-topology-winner-at-top
  ;; Issue #146 regression: source-only star at default `>` ratio (2:1).
  ;; Pre-fix produced uniform 1/3 scores; alphabetical fallback put the
  ;; unambiguous winner at the bottom. Post-fix the chain is aperiodic and
  ;; converges to π_zebra = 1/2, π_alpha = π_beta = 1/4.
  (let [ranking (first-component-ranking (compile-sorter "test/fixtures/ranking/star.sorter"))
        names   (mapv (comp item-leaf :item) ranking)
        by-name (into {} (map (juxt (comp item-leaf :item) :score) ranking))]
    (is (= "zebra" (first names))
        "zebra won both votes and should rank #1")
    (is (< (Math/abs (- (by-name "zebra") 0.5)) 1e-3))
    (is (< (Math/abs (- (by-name "alpha") 0.25)) 1e-3))
    (is (< (Math/abs (- (by-name "beta") 0.25)) 1e-3))))
