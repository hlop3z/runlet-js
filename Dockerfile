# ── Planner (cargo-chef) ─────────────────────────────────
FROM rust:1.92-alpine AS planner

RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef --locked

WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Builder (cached deps + final build) ──────────────────
FROM rust:1.92-alpine AS builder

RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef --locked

WORKDIR /app

# Cook deps from recipe (cached as long as deps don't change)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --target x86_64-unknown-linux-musl --recipe-path recipe.json

# Build the real app (only recompiles our code)
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl \
    && strip target/x86_64-unknown-linux-musl/release/runlet

# ── Runtime (distroless static — no glibc needed) ────────
FROM gcr.io/distroless/static-debian12:nonroot

WORKDIR /app

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/runlet .
COPY config.example.json config.example.json

# Default config binds 0.0.0.0 so the published port is reachable. /execute runs
# caller-supplied code, so the box FAILS CLOSED on an exposed bind with no auth gate —
# a bare `docker run` refuses to start by design. Supply the gate at run time via env
# (no secret is ever baked into the image):
#
#   docker run -e RUNLET_ACCESS_TOKEN=<secret>       -p 3000:3000 <image>   # authenticated
#   docker run -e RUNLET_ALLOW_UNAUTHENTICATED=1     -p 3000:3000 <image>   # auth terminated upstream / quickstart
#
# For anything beyond a quickstart, mount a full config over /app/config.json.
COPY <<EOF /app/config.json
{"server":{"host":"0.0.0.0","port":3000}}
EOF

EXPOSE 3000

ENTRYPOINT ["./runlet"]
