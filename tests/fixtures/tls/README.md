This self-signed localhost certificate and its public test key are fixtures only.
They are deliberately untrusted and must never be used for a real service.
The integration test verifies that Hydra rejects the certificate as `:tls`.
