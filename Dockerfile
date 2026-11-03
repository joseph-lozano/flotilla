# Flotilla's server: `flotilla serve`, with its data (the database, uploads, and the secrets
# `flotilla setup` keeps) in the /data volume. See docs/server.md.
#
#   docker build -t flotilla .
#   docker build -t flotilla --build-arg CLAUDE=1 .   # with the claude CLI, for titles and previews
#
# Nothing secret is in any layer: set up in a running container (`flotilla setup`), and the
# boat key, gh's sign-in and the Claude token are kept in /data.

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev build-base cmake perl
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p flotilla-cli \
    && cp target/release/flotilla /flotilla \
    && strip /flotilla

FROM alpine:3
ARG CLAUDE=0
RUN apk add --no-cache ca-certificates tini github-cli \
    && adduser -D -h /home/flotilla -u 10001 flotilla \
    && mkdir -p /data \
    && chown flotilla:flotilla /data \
    && chmod 700 /data
RUN if [ "$CLAUDE" = 1 ]; then \
        apk add --no-cache bash curl libgcc libstdc++ ripgrep \
        && su flotilla -c 'curl -fsSL https://claude.ai/install.sh | bash'; \
    fi
COPY --from=build /flotilla /usr/local/bin/flotilla
USER flotilla
# Flotilla's data is /data/Flotilla, where every `flotilla` command looks by default; gh keeps
# its sign-in in the volume too. claude (with CLAUDE=1) is on PATH; its own ripgrep doesn't
# run on Alpine.
ENV XDG_DATA_HOME=/data \
    GH_CONFIG_DIR=/data/gh \
    PATH=/home/flotilla/.local/bin:$PATH \
    USE_BUILTIN_RIPGREP=0
VOLUME /data
EXPOSE 7777
HEALTHCHECK --interval=30s --timeout=5s CMD wget -qO /dev/null http://127.0.0.1:7777/v1/health || exit 1
ENTRYPOINT ["/sbin/tini", "--", "flotilla"]
# In a container the server listens on every address (Docker forwards to it); publish it on
# the host's loopback (-p 127.0.0.1:7777:7777) and put Tailscale or Caddy in front.
CMD ["serve", "--host", "0.0.0.0", "--insecure"]
