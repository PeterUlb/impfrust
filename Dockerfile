FROM rust:1.52 as builder
WORKDIR app
COPY . .
RUN cargo build --release --bin impfrust

FROM rust:1.52 as runtime
WORKDIR app
COPY --from=builder /app/target/release/impfrust /usr/local/bin/impfrust
ENTRYPOINT ["impfrust", "--lat", "49.39875", "--long", "8.672434", "--radius", "150"]