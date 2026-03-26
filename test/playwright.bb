(ns test.playwright
  "Babashka rig for Playwright end-to-end tests.
   Installs pip dependencies, starts the slugsocial-server on a free port,
   seeds it with sample data via the CLI, runs pytest with --base-url, then kills the server."
  (:require [babashka.process :as p]
            [babashka.fs :as fs]
            [clojure.string :as str]
            [test.common :as common]))

;; ---------------------------------------------------------------------------
;; seed data — enough tree nodes for interaction tests to run
;; ---------------------------------------------------------------------------

(def ^:private seed-actor "@00000000-0000-0000-0000-000000000001:playwright:local/seed")

(def ^:private seed-doc
  (str/join "\n"
            [seed-actor
             "#playwright-seed"
             ""
             "~/languages         { Programming languages }"
             "~/languages/python  { General-purpose, dynamically typed }"
             "~/languages/rust    { Systems language with ownership model }"
             "~/languages/go      { Compiled, garbage-collected, simple concurrency }"
             ""
             "~/languages/rust 3:1 ~/languages/python { Rust has stronger type safety }"
             "~/languages/rust 2:1 ~/languages/go     { Ownership beats GC for systems work }"
             "~/languages/python 2:1 ~/languages/go   { Python's ecosystem is broader }"]))

(defn playwright-test [& _args]
  (println "\n━━━ playwright e2e tests ━━━\n")

  ;; 1. Install pip requirements
  (println "installing pip requirements…")
  (let [pip-result @(p/process ["pip" "install" "-q" "-r" "test/playwright/requirements.txt"]
                               {:inherit true})]
    (when-not (zero? (:exit pip-result))
      (println "pip install failed")
      (System/exit (:exit pip-result))))

  ;; 2. Install playwright browser
  (println "installing playwright chromium…")
  (let [pw-result @(p/process ["playwright" "install" "chromium"]
                              {:inherit true})]
    (when-not (zero? (:exit pw-result))
      (println "playwright install failed")
      (System/exit (:exit pw-result))))

  ;; 3. Build binaries
  (println "building binaries…")
  (let [cargo (common/cargo-bin)
        build-result @(p/process [cargo "build" "--release"
                                  "--package" "slugsocial-server"
                                  "--package" "slugsocial"]
                                 {:inherit true :env common/base-env})]
    (when-not (zero? (:exit build-result))
      (println "cargo build failed")
      (System/exit (:exit build-result))))

  ;; 4. Find binaries
  (let [server-bin (cond
                     (fs/exists? "target/release/slugsocial-server") "target/release/slugsocial-server"
                     (fs/exists? "target/debug/slugsocial-server")   "target/debug/slugsocial-server"
                     :else (do (println "slugsocial-server binary not found. Run `cargo build -p slugsocial-server` first.")
                               (System/exit 1)))
        cli-bin    (cond
                     (fs/exists? "target/release/slugsocial") "target/release/slugsocial"
                     (fs/exists? "target/debug/slugsocial")   "target/debug/slugsocial"
                     :else (do (println "slugsocial CLI binary not found. Run `cargo build -p slugsocial` first.")
                               (System/exit 1)))]
    (println (str "using server: " server-bin))
    (println (str "using cli:    " cli-bin))

    ;; 4. Start server on a free port with a temp data dir
    (let [port       (common/pick-port)
          base-url   (str "http://127.0.0.1:" port)
          tmp-dir    (str (fs/create-temp-dir {:prefix "slug-playwright-"}))
          server-env (merge common/base-env
                            {"SLUG_DATA_DIR" tmp-dir
                             "SLUG_KEYS"     "test:test"
                             "PORT"          (str port)
                             "RUST_LOG"      "warn"})
          !server    (atom nil)]
      (try
        (println (str "\nstarting server on :" port " (data: " tmp-dir ")"))
        (let [server (common/start-server server-bin server-env)]
          (reset! !server server)
          (when-not (common/wait-for-server base-url 15000)
            (println "server did not become healthy within 15s")
            (common/kill-server server)
            (fs/delete-tree tmp-dir)
            (System/exit 1))
          (println (str "server healthy at " base-url)))

        ;; 5. Seed data via CLI
        (println "\nseeding tree data…")
        (let [seed-result (common/run-cli cli-bin base-url ["ingest"] :input seed-doc)]
          (when-not (zero? (:exit seed-result))
            (println (str "seed ingest failed:\n" (:err seed-result)))
            (System/exit 1))
          (println "  seed ok"))

        ;; 6. Run pytest
        (println (str "\nrunning pytest test/playwright/ --base-url " base-url "\n"))
        (let [pytest-result @(p/process ["pytest" "test/playwright/" "-v"
                                         (str "--base-url=" base-url)]
                                        {:inherit true})]

          ;; 7. Kill server
          (println "\nkilling server…")
          (when-some [s @!server]
            (common/kill-server s)
            (reset! !server nil))
          (fs/delete-tree tmp-dir)

          ;; 8. Exit with pytest exit code
          (System/exit (:exit pytest-result)))

        (finally
          (when-some [s @!server]
            (println "\nkilling server…")
            (common/kill-server s))
          (fs/delete-tree tmp-dir))))))
