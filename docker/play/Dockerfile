# A local stand-in for the server /play is deployed to.
#
# The point is not "a web server" — python3 -m http.server was already that, and
# WebGPU is happy on localhost either way. The point is *this* web server: the
# generated dist/play/nginx.conf has to be a config nginx will actually load,
# the cache headers have to be the ones that come back, the .wasm has to arrive
# as application/wasm, the precompressed copies have to be the ones served, and
# the server's own security headers have to survive the locations that set a
# Cache-Control. None of that is observable from a static file server, and all
# of it is the kind of thing that is discovered on the real box at the worst
# moment.
#
# alpine + apk rather than the official nginx image, for one reason: the brotli
# module. `nginx-mod-http-brotli` is built against Alpine's nginx, so the pair
# match; dropping the same module into the Docker Hub nginx gives a version
# mismatch that refuses to load. Brotli is worth the trouble — 2141 KiB against
# gzip's 2721 on a module a visitor downloads before anything happens.
FROM alpine:3.20

# nginx 1.26 here. Two things about this build are load-bearing and are asserted
# by scripts/serve-play.sh rather than assumed:
#   - mime.types already maps wasm (since at least 1.21.0), so the site config
#     must NOT declare its own types block at server level — that replaces the
#     whole map rather than adding to it, and everything else becomes
#     octet-stream;
#   - --with-http_gzip_static_module is in the stock build, ngx_brotli is not
#     and comes from the package below.
RUN apk add --no-cache nginx nginx-mod-http-brotli openssl \
    && mkdir -p /run/nginx /srv/root /srv/clips /etc/nginx/vidiotic-headers

# The shared header snippet the generated config includes by glob. Kept as a
# real file rather than baked into the server block, because that is the shape
# an operator has to reproduce for their own headers to survive.
COPY vidiotic-headers.conf /etc/nginx/vidiotic-headers/00-site.conf
COPY entrypoint.sh /entrypoint.sh
RUN rm -f /etc/nginx/http.d/default.conf && chmod +x /entrypoint.sh

# The server block is bind-mounted rather than copied: scripts/serve-play.sh
# rewrites SITE_DIR in it to rehearse a subdirectory deploy, and an operator
# reading docker/play/nginx.conf should be reading the file that ran.
EXPOSE 80 443
ENTRYPOINT ["/entrypoint.sh"]
