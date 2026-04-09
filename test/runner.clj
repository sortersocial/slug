(ns test.runner
  "JVM entrypoint for the integration test suite."
  (:require [test.integration :as integration]
            [test.auth :as auth]
            [test.grants :as grants]
            [test.invites :as invites]
            [test.browser-sse :as browser-sse]))

(defn run-core! []
  (integration/integration)
  (auth/auth-test)
  (grants/grants-test)
  (invites/invites-test))

(defn -main [& args]
  (if (= ["browser-sse"] (vec args))
    (browser-sse/private-thread-sse-browser-test)
    (run-core!)))
