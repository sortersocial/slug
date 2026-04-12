(ns test.runner
  "JVM entrypoint: run the same flows as Kaocha (for scripts / quick runs)."
  (:require [test.integration :as integration]
            [test.auth :as auth]
            [test.grants :as grants]
            [test.invites :as invites]
            [test.room-list :as room-list]
            [test.browser-sse :as browser-sse]
            [test.browser-post-redact :as browser-post-redact]
            [test.browser-ui-morph :as browser-ui-morph]))

(defn run-core! []
  (integration/integration-flow!)
  (auth/auth-flow!)
  (grants/grants-flow!)
  (invites/invites-flow!)
  (room-list/room-list-flow!))

(defn -main [& args]
  (case (vec args)
    ["browser-sse"] (browser-sse/sse-browser-flow!)
    ["browser-post-redact"] (browser-post-redact/post-redact-flow!)
    ["browser-ui-morph"] (browser-ui-morph/ui-morph-flow!)
    (run-core!)))
