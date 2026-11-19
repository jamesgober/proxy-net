#!/usr/bin/env bash
# Regenerates the test certificates and keys in this directory.
# Test material only: none of these keys protects anything.
# Needs OpenSSL 3.4 or later (for -not_before and -not_after).
set -euo pipefail
cd "$(dirname "$0")"
FROM=20260101000000Z
TO=20991231235959Z

# A root CA (ECDSA P-256).
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ca.key
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n' > ca.ext
openssl req -new -key ca.key -subj "/CN=proxy-net test root" -out ca.csr
openssl x509 -req -in ca.csr -signkey ca.key -sha256 -not_before $FROM -not_after $TO -extfile ca.ext -out ca.pem

# The upstream's certificate, for localhost and 127.0.0.1.
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out upstream.key
openssl req -new -key upstream.key -subj "/CN=upstream" -out upstream.csr
printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n' > upstream.ext
openssl x509 -req -in upstream.csr -CA ca.pem -CAkey ca.key -sha256 -not_before $FROM -not_after $TO \
  -extfile upstream.ext -set_serial "0x$(openssl rand -hex 8)" -out upstream.crt
cat upstream.crt ca.pem > upstream.pem

rm -f ca.csr ca.ext ca.key upstream.csr upstream.ext upstream.crt
