FROM rust:1.52 as builder
WORKDIR /usr/src/impfrust
COPY . .
RUN cargo install --path .

FROM debian:buster-slim as runtime
RUN apt-get update && apt-get install -y ca-certificates
COPY --from=builder /usr/local/cargo/bin/impfrust /usr/local/bin/impfrust
ENTRYPOINT ["impfrust", "--lat", "49.488888", "--long", "8.469167", "--radius", "150"]