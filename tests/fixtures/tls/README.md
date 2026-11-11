# Test TLS certificate

`cert.pem` and `key.pem` are a **throwaway, self-signed** certificate and its
private key, committed so the web server's HTTPS tests (`tests/web_server.rs`,
`mod worker`) and the worker's certificate-lifecycle unit tests have a real
certificate to load. They protect nothing: never use them anywhere else.

- Key: ECDSA P-256 (PKCS#8, unencrypted).
- Subject `CN=localhost`; SAN `DNS:localhost`, `IP:127.0.0.1`.
- `basicConstraints CA:FALSE`, `keyUsage digitalSignature`,
  `extendedKeyUsage serverAuth`, so a test client can trust the certificate
  itself as its root and verify the server as `localhost` or `127.0.0.1`.
- Valid for 100 years: notAfter `2126-10-18 09:43:05 UTC` (unix
  `4947990185`), past 2049, so it is encoded as a `GeneralizedTime` and the
  expiry parser's four-digit-year path is exercised. Tests pin that number.

Generated with:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -keyout key.pem -out cert.pem -days 36500 -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=serverAuth"
```

Regenerating changes the notAfter: update the pinned value in the tests.
