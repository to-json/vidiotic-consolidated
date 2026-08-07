# A local stand-in for fubarchitect.com, close enough to argue with.
#
# The existing `docker/play/` container rehearses /play as its own site at the
# document root, served by the nginx.conf `release-play.sh` generates. That is
# the deployment /play was designed for, and it is not the one happening: on
# fubarchitect a warez is copied verbatim into `_site/` by a Ruby SSG and served
# by a vhost written for a text-and-CSS personal site. The generated config is
# never read. Nothing about the earlier rehearsal covers that.
#
# So this image is Debian + Debian's nginx, because that is what the DigitalOcean
# box is, and the two things under test are both properties of that exact build:
#
#   - whether `mime.types` maps .wasm (it must, or instantiateStreaming refuses
#     the module);
#   - what the stock `gzip_types` list does and does not include, which decides
#     whether a 6.6 MB download is 6.6 MB.
#
# Alpine would have been smaller and would have answered a different question.
FROM debian:trixie-slim

# nginx from Debian's own archive, not nginx.org's — the ansible `nginx` role
# does `apt install nginx`, and the build flags that come with it are part of
# what is being rehearsed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends nginx openssl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /etc/nginx/sites-enabled/default \
    && mkdir -p /srv/root /etc/nginx/fubar /run/nginx

# The main config replaces Debian's. Both are transcriptions of the templates in
# webb/ansible/roles/nginx/templates — see the header of each for the deviations.
COPY nginx.conf /etc/nginx/nginx.conf
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# vhost.conf, site.conf and warez-vidiotic.conf are bind-mounted by
# scripts/stage-fubar.sh rather than copied: the whole point is to edit the
# vidiotic block and see what changes without rebuilding an image, and an
# operator reading docker/fubar/*.conf should be reading the files that ran.
EXPOSE 80 443
ENTRYPOINT ["/entrypoint.sh"]
