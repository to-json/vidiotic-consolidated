#!/bin/sh
# Generate a throwaway certificate, then hand over to nginx.
#
# TLS is here because the real deployment is https and `listen 443 ssl` is a
# different code path in the config than `listen 80` — a config that loads on
# one can fail on the other. It is not here to test the *certificate*: this one
# is self-signed and freshly minted every container start, so a browser will
# refuse it without --ignore-certificate-errors.
#
# Which is why http is the default. localhost is a secure context regardless of
# scheme, so WebGPU works over plain http and the smoke test needs no flags to
# tell Chrome to trust anything.
set -e

CERT=/etc/nginx/play.crt
KEY=/etc/nginx/play.key

if [ ! -f "$CERT" ]; then
  openssl req -x509 -newkey rsa:2048 -nodes -days 30 \
    -keyout "$KEY" -out "$CERT" \
    -subj '/CN=localhost' \
    -addext 'subjectAltName=DNS:localhost,IP:127.0.0.1' \
    >/dev/null 2>&1
fi

# Fail loudly and immediately on a bad config rather than after the port is
# bound — this container exists to catch exactly that.
nginx -t

exec nginx -g 'daemon off;'
