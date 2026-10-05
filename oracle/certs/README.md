# Test certificates

These files are test values, and the keys are not secret. Do not use them for anything else.

`root.crt` is a certificate authority, and `server.crt` is a certificate for `localhost`, `127.0.0.1` and `::1` that it signs. Both are valid for 100 years. Both servers present `server.crt`, so a client that trusts `root.crt` sees the same chain from each. `root.key` is here so that a later change can sign a client certificate.
