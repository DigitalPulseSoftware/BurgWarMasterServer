FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev

RUN cargo install cargo-build-deps

WORKDIR /app

RUN cargo new --bin bw_masterserver
WORKDIR /app/bw_masterserver

COPY Cargo.toml ./
RUN cargo build-deps --release

COPY src ./src
RUN cargo build --release

FROM alpine:3 AS runtime

EXPOSE 8080

WORKDIR /app

COPY --from=builder /app/bw_masterserver/target/release ./

CMD ["/app/bw_master_server"]
