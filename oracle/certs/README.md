# Test certificates

These files are test values, and the keys are not secret. Do not use them for anything else.

`root.crt` is a certificate authority, and `server.crt` is a certificate for `localhost`, `127.0.0.1` and `::1` that it signs. Both are valid for 100 years. Both servers present `server.crt`, so a client that trusts `root.crt` sees the same chain from each. `root.key` is here so that a later change can sign a client certificate.

The root has the key usage `keyCertSign` and `cRLSign`, and `server.crt` has a subject key identifier and an authority key identifier. Python 3.13 and later check a chain with `VERIFY_X509_STRICT` by default, and they refuse a chain without these extensions. To make the certificates again with the same keys, sign a request for `/CN=rudb-postgres test root` with `root.key` and these extensions, then sign a request for `/CN=localhost` with the new root:

```
# root
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash

# server
subjectAltName = DNS:localhost, IP:127.0.0.1, IP:::1
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid
```
