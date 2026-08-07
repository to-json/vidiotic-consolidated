#!/bin/sh
# Generate the throwaway crypto the real config expects, then hand over to nginx.
#
# The production nginx.conf references `ssl_dhparam /etc/nginx/dhparam.pem` at
# *http* level, which means nginx refuses to start without it whether or not a
# TLS listener is ever hit. Generating it is therefore not optional, and 2048
# bits takes long enough that it is done once into a named volume rather than on
# every start.
set -e

# All three live in /etc/nginx/tls, which the stage script backs with a named
# volume — dhparam at 2048 bits is slow enough that regenerating it on every
# container start would be the slowest thing about the rig.
mkdir -p /etc/nginx/tls
CERT=/etc/nginx/tls/fubar.crt
KEY=/etc/nginx/tls/fubar.key
DH=/etc/nginx/tls/dhparam.pem

if [ ! -f "$DH" ]; then
  echo "generating dhparam (once, ~10s) ..."
  openssl dhparam -out "$DH" 2048 >/dev/null 2>&1
fi

if [ ! -f "$CERT" ]; then
  openssl req -x509 -newkey rsa:2048 -nodes -days 30 \
    -keyout "$KEY" -out "$CERT" \
    -subj '/CN=localhost' \
    -addext 'subjectAltName=DNS:localhost,DNS:fubarchitect.local,IP:127.0.0.1' \
    >/dev/null 2>&1
fi

# Fail on a bad config before the port is bound. This container exists to catch
# exactly that class of thing, so it must never come up half-working.
nginx -t

exec nginx -g 'daemon off;'
