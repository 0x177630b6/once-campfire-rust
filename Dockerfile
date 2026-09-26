# syntax = docker/dockerfile:1
#
# Production image for the Rust port. A drop-in for the reference image (reference/Dockerfile):
# same user (uid 1000), working directory, storage layout (/rails/storage/{db,files,backups}), env
# vars, ports and ONCE hooks. The app runs behind Thruster, like the reference's Procfile does
# (`thrust bin/start-app`), until TLS termination moves into the binary.
#
#   docker build -t campfire-rust --build-arg APP_VERSION=... --build-arg GIT_REVISION=... .
#
# Media: variants and video posters must be byte-identical to the reference's, so the runtime
# installs the exact Debian trixie packages the reference image ships (see NOTES.md, storage):
# libvips 8.16.1-1+deb13u1 and ffmpeg 7:7.1.5-0+deb13u1, and no PDF previewers (the reference has
# neither poppler nor mupdf, so PDFs aren't previewable there either).

ARG RUST_VERSION=1.98.1
ARG DEBIAN_RELEASE=trixie
ARG LIBVIPS_VERSION=8.16.1-1+deb13u1
ARG FFMPEG_VERSION=7:7.1.5-0+deb13u1
ARG THRUSTER_VERSION=0.1.23
ARG RUBY_VERSION=3.4.10


# Build the binary. libvips-dev (same version as the runtime's libvips) provides the link-time
# libraries for crates/storage's FFI.
FROM docker.io/library/rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS build
ARG LIBVIPS_VERSION
RUN apt-get update -qq && \
    apt-get install --no-install-recommends -y libvips-dev=${LIBVIPS_VERSION} && \
    rm -rf /var/lib/apt/lists /var/cache/apt/archives

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# crates/assets/build.rs digests and embeds the reference's assets and public/ at build time.
COPY reference reference

RUN --mount=type=cache,id=campfire-rust-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=campfire-rust-target,target=/src/target \
    cargo build --release --locked -p campfire && \
    install -D -m 755 target/release/campfire /out/campfire


# Thruster: the same release the reference bundles (Gemfile.lock), from the platform gem.
FROM docker.io/library/ruby:${RUBY_VERSION}-slim-${DEBIAN_RELEASE} AS thruster
ARG THRUSTER_VERSION
RUN gem install thruster -v ${THRUSTER_VERSION} --no-document && \
    install -D -m 755 "$(find "$(gem env gemdir)/gems" -path "*/thruster-${THRUSTER_VERSION}-*/exe/*-linux/thrust" -type f)" /out/thrust


FROM docker.io/library/debian:${DEBIAN_RELEASE}-slim

ARG LIBVIPS_VERSION
ARG FFMPEG_VERSION
# ca-certificates: the system CA store, for webhooks, unfurling and Web Push over TLS.
RUN apt-get update -qq && \
    apt-get install --no-install-recommends -y \
      ca-certificates libvips42t64=${LIBVIPS_VERSION} ffmpeg=${FFMPEG_VERSION} && \
    rm -rf /var/lib/apt/lists /var/cache/apt/archives

# Image metadata
ARG OCI_DESCRIPTION
LABEL org.opencontainers.image.description="${OCI_DESCRIPTION}"
ARG OCI_SOURCE
LABEL org.opencontainers.image.source="${OCI_SOURCE}"
LABEL org.opencontainers.image.licenses="MIT"

# Run and own only the runtime files as a non-root user, as the reference does.
RUN groupadd --system --gid 1000 rails && \
    useradd rails --uid 1000 --gid 1000 --create-home --shell /bin/bash

WORKDIR /rails

COPY --from=thruster /out/thrust /usr/local/bin/thrust
COPY --from=build /out/campfire /usr/local/bin/campfire

# bin/boot, as in the reference: Thruster on HTTP_PORT (80) and, with TLS_DOMAIN, 443, proxying to
# the app on TARGET_PORT (3000), which it passes to the app as PORT.
COPY --chmod=755 <<'EOF' /rails/bin/boot
#!/bin/sh
exec thrust /usr/local/bin/campfire server
EOF

# The storage root is Rails.root.join("storage"): storage/db/<env>.sqlite3, storage/files,
# storage/backups.
RUN mkdir -p /rails/storage/db /rails/storage/files /rails/storage/backups && \
    chown -R 1000:1000 /rails

# ONCE backup/restore hooks. pre-backup is script/admin/prepare-backup (`campfire backup`);
# post-restore is the reference's own script.
COPY --chmod=755 <<'EOF' /hooks/pre-backup
#!/bin/bash
cd /rails
exec /usr/local/bin/campfire backup
EOF
COPY --chmod=755 reference/hooks/post-restore /hooks/post-restore

USER 1000:1000

# Configure environment defaults
ENV RAILS_ENV="production"
ENV HTTP_IDLE_TIMEOUT=60
ENV HTTP_READ_TIMEOUT=300
ENV HTTP_WRITE_TIMEOUT=300

# Set version and revision
ARG APP_VERSION
ENV APP_VERSION=$APP_VERSION
ARG GIT_REVISION
ENV GIT_REVISION=$GIT_REVISION

# Expose ports for HTTP and HTTPS
EXPOSE 80 443

# Start the server by default, this can be overwritten at runtime
CMD ["bin/boot"]
