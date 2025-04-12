FROM rust:1.86.0-alpine AS base
RUN apk add musl-dev musl-utils
RUN cargo install cargo-chef

FROM base AS chef
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=chef /recipe.json recipe.json
RUN cargo chef cook --target x86_64-unknown-linux-musl --release  --bin ddrs --recipe-path recipe.json
COPY . .
RUN cargo build --target x86_64-unknown-linux-musl --release --bin ddrs

FROM scratch AS runtime
COPY --from=builder /target/x86_64-unknown-linux-musl/release/ddrs /bin/
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
CMD ["/bin/ddrs"]
