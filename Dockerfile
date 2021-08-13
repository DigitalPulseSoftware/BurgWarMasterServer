FROM rust:1-alpine
EXPOSE 8080

RUN apk add --no-cache musl-dev

RUN cargo install cargo-build-deps

WORKDIR /app

RUN cargo new --bin bw_masterserver
WORKDIR /app/bw_masterserver

COPY Cargo.toml ./
RUN cargo build-deps --release

COPY src ./src
RUN cargo build --release

CMD cargo run --release
