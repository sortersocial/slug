(ns test.runner
  "JVM entrypoint for the integration test suite."
  (:require [test.integration :as integration]
            [test.auth :as auth]
            [test.grants :as grants]
            [test.invites :as invites]
            [test.room-list :as room-list]
            [test.browser-sse :as browser-sse]
            [test.browser-post-redact :as browser-post-redact]))

(defn run-core! []
  (integration/integration)
  (auth/auth-test)
  (grants/grants-test)
  (invites/invites-test)
  (room-list/room-list-test))

(defn -main [& args]
  (case (vec args)
    ["browser-sse"] (browser-sse/private-thread-sse-browser-test)
    ["browser-post-redact"] (browser-post-redact/post-redact-browser-test)
    (run-core!)))
