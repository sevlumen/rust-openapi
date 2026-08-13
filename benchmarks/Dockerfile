FROM rust:1.88.0-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches
RUN cargo build --release --locked --bin oas-bench-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 oas
COPY --from=build /src/target/release/oas-bench-server /usr/local/bin/oas-bench-server
USER oas
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/oas-bench-server"]
