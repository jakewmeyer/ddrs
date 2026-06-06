FROM rust:1.96.0-alpine AS base
WORKDIR /app
RUN apk add --no-cache musl-dev musl-utils ca-certificates
RUN update-ca-certificates
RUN cargo install cargo-chef
RUN rustup target add x86_64-unknown-linux-musl

FROM base AS chef
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base AS builder
COPY --from=chef /app/recipe.json recipe.json
RUN cargo chef cook --locked --target x86_64-unknown-linux-musl --release --bin ddrs --recipe-path recipe.json
COPY . .
RUN cargo build --locked --target x86_64-unknown-linux-musl --release --bin ddrs

FROM scratch AS runtime
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/ddrs /bin/
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
CMD ["/bin/ddrs"]
