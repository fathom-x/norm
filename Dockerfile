# Multi-stage build: compile a static musl `owallet` binary, then drop it
# into a distroless image. The Python implementation needed Python 3.12 +
# conda + a long list of native deps; here it's one binary, no runtime.

FROM rust:1-bookworm AS builder

# musl-tools gives us the static toolchain for x86_64-unknown-linux-musl.
RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rustup target add x86_64-unknown-linux-musl \
 && rm -rf /var/lib/apt/lists/*

# Point the `cc` crate at musl-gcc so the bundled C deps (libsqlite3-sys's
# sqlite3.c, secp256k1-sys's libsecp256k1) compile against musl. We do NOT
# override CARGO_TARGET_..._LINKER: the musl target links self-contained
# and static by default (rust-lld + musl crt), producing a no-interpreter
# static-pie binary. Forcing musl-gcc as the *linker* instead yields a
# dynamically-linked musl binary that needs /lib/ld-musl-x86_64.so.1 at
# runtime — which the distroless `static` image does not ship, so the
# container would fail to exec with "no such file or directory".
ENV CC_x86_64_unknown_linux_musl=musl-gcc

WORKDIR /src
COPY . /src

# Build only the binary crate to skip irrelevant workspace members in
# Docker builds.
RUN cargo build --profile dist \
    --target x86_64-unknown-linux-musl \
    -p owallet

FROM gcr.io/distroless/static-debian12:nonroot AS runtime

COPY --from=builder \
    /src/target/x86_64-unknown-linux-musl/dist/owallet \
    /usr/local/bin/owallet

ENV OWALLET_HOST=0.0.0.0 \
    OWALLET_PORT=8765 \
    OWALLET_DB_PATH=/data/.owallet.db

# The dashboard + MCP both share this port.
EXPOSE 8765

# /data is the only writable mount the container needs — point a volume
# at it to persist the encrypted DB across restarts.
VOLUME ["/data"]

USER nonroot:nonroot

ENTRYPOINT ["/usr/local/bin/owallet"]
CMD ["serve"]
