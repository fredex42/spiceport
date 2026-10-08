#!/bin/sh
# Generate a throwaway CA and server certificate for TLS testing:
#   scripts/gen-test-certs.sh <dir> [server-cn]
# Writes ca-cert.pem, server-cert.pem and server-key.pem (the names QEMU's
# -spice x509-dir= expects). The server cert has SANs for localhost and
# 127.0.0.1 and the subject "C=GB,O=spiceport test,CN=<server-cn>".
set -eu
dir=$1
cn=${2:-localhost}
mkdir -p "$dir"
cd "$dir"
openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/C=GB/O=spiceport test CA/CN=spiceport test CA" \
    -keyout ca-key.pem -out ca-cert.pem 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/C=GB/O=spiceport test/CN=$cn" \
    -keyout server-key.pem -out server.csr 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\n' > san.ext
openssl x509 -req -in server.csr -CA ca-cert.pem -CAkey ca-key.pem -CAcreateserial -days 365 \
    -extfile san.ext -out server-cert.pem 2>/dev/null
rm -f server.csr san.ext ca-cert.srl
echo "certificates written to $dir"
