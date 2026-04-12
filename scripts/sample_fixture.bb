(ns scripts.sample-fixture
  "Find process(es) listening on the fixture port (default 8080) and run macOS `sample`."
  (:require [babashka.fs :as fs]
            [babashka.process :as p]
            [clojure.string :as str]))

(defn- usage []
  (println "Usage: bb sample-fixture [PORT] [DURATION_SEC] [OUT_DIR]")
  (println "")
  (println "  Finds PIDs bound to TCP LISTEN on PORT (default 8080), then runs")
  (println "  `sample` for each PID. OUT_DIR defaults to the current directory.")
  (println "")
  (println "  Example:  bb sample-fixture")
  (println "            bb sample-fixture 8080 10")
  (println "            bb sample-fixture 8080 5 /tmp")
  (println "")
  (println "  Requires macOS (the `sample` tool)."))

(defn- parse-long* [s]
  (try (Long/parseLong s)
       (catch NumberFormatException _ nil)))

(defn- listen-pids [port]
  (let [spec (str "TCP:" port)
        {:keys [out exit]}
        @(p/process ["lsof" "-nP" (str "-i" spec) "-sTCP:LISTEN" "-t"]
                    {:out :string :err :string})]
    (when (zero? exit)
      (->> (str/split-lines out)
           (map str/trim)
           (remove str/blank?)
           (distinct)
           vec))))

(defn- sample-bin []
  (or (fs/which "sample")
      (throw (ex-info "macOS `sample` not found on PATH" {}))))

(defn- run-sample! [sample duration-sec pid out-file]
  (println (str "sampling PID " pid " for " duration-sec "s → " out-file))
  (let [{:keys [exit err]} @(p/process [sample (str pid) (str duration-sec) "-file" out-file]
                                      {:out :inherit :err :inherit})]
    (when-not (zero? exit)
      (binding [*out* *err*]
        (println "sample failed:" err))
      (System/exit exit))))

(defn -main [& args]
  (when (some #{"-h" "--help" "help"} args)
    (usage)
    (System/exit 0))
  (let [port (or (some-> (first args) parse-long*) 8080)
        duration-sec (or (some-> (second args) parse-long*) 5)
        out-dir (or (nth args 2 nil) ".")
        pids (listen-pids port)]
    (when (or (nil? pids) (empty? pids))
      (binding [*out* *err*]
        (println (str "No process listening on TCP " port " (LISTEN). Is `bb fixture` running?")))
      (System/exit 1))
    (when-not (fs/exists? out-dir)
      (binding [*out* *err*]
        (println "Output directory does not exist:" out-dir))
      (System/exit 1))
    (let [sample (sample-bin)
          ts (str (System/currentTimeMillis))]
      (println (str "port " port " → PIDs " (str/join ", " pids)))
      (doseq [pid pids]
        (let [out-file (str (fs/path out-dir) "/slug-sample-" port "-" pid "-" ts ".txt")]
          (run-sample! sample duration-sec pid (str out-file))))
      (println "done."))))
