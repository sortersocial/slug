(ns test.runner
  "JVM entrypoint for the integration test suite (mirrors `bb test` in bb.edn)."
  (:require [test.integration :as integration]
            [test.auth :as auth]
            [test.grants :as grants]
            [test.invites :as invites]))

(defn -main [& _args]
  (integration/integration)
  (auth/auth-test)
  (grants/grants-test)
  (invites/invites-test))
