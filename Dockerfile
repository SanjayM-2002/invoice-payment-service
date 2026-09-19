# One image, three binaries. Each compose service runs a different one, so the
# build and all the dependency layers are shared.

FROM rust:1.96 AS builder
WORKDIR /app

COPY . .

ENV SQLX_OFFLINE=true

RUN cargo build --release --workspace

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/invoice-service  /usr/local/bin/invoice-service
COPY --from=builder /app/target/release/mock-psp         /usr/local/bin/mock-psp
COPY --from=builder /app/target/release/webhook-receiver /usr/local/bin/webhook-receiver

CMD ["invoice-service"]
