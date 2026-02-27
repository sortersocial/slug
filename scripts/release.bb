(ns scripts.release
  (:require [babashka.process :as p]
            [clojure.string :as str]))

(defn bump-version [version bump-type]
  (let [[major minor patch] (map #(Integer/parseInt %) (str/split version #"\."))
        [new-major new-minor new-patch]
        (case bump-type
          "major" [(inc major) 0 0]
          "minor" [major (inc minor) 0]
          "patch" [major minor (inc patch)]
          [major minor (inc patch)])]
    (str new-major "." new-minor "." new-patch)))

(defn release [& args]
  (let [bump-type-or-version (first args)
        explicit-version (when (and bump-type-or-version
                                    (re-matches #"^\d+\.\d+\.\d+$" bump-type-or-version))
                           bump-type-or-version)
        bump-type (if explicit-version "patch" (or bump-type-or-version "patch"))

        latest-tag (-> (p/process ["git" "tag" "--sort=-version:refname"] {:out :string})
                       deref
                       :out
                       str/trim
                       (str/split-lines)
                       first
                       (or ""))
        latest-version (if (empty? latest-tag)
                         "0.0.0"
                         (str/replace latest-tag #"^v" ""))

        version (if explicit-version
                  (str/replace explicit-version #"^v" "")
                  (bump-version latest-version bump-type))
        tag (str "v" version)]

    ;; Check for clean working tree
    (let [result @(p/process ["git" "diff" "--quiet"] {:continue true})]
      (when (not= 0 (:exit result))
        (println "error: working tree has uncommitted changes — commit or stash first")
        (System/exit 1)))
    (let [result @(p/process ["git" "diff" "--cached" "--quiet"] {:continue true})]
      (when (not= 0 (:exit result))
        (println "error: working tree has staged uncommitted changes — commit or stash first")
        (System/exit 1)))

    (when (not explicit-version)
      (println (str "Latest version: " latest-version))
      (println (str "Bumping " bump-type " version to: " version)))

    (println (str "Tagging " tag " and pushing to origin..."))
    @(p/process ["git" "tag" "-a" tag "-m" (str "Release " tag)] {:inherit true})
    @(p/process ["git" "push" "origin" tag] {:inherit true})

    (println "")
    (println (str "Done. Tag " tag " pushed."))
    (println "Watch the release workflow at:")
    (println "  https://github.com/sortersocial/slug/actions")
    (println "")
    (println "Once complete, users will get the new binary via: npx slugsocial")))
