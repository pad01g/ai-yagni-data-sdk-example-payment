# 模擬の決済サービスのイメージ（compose から使う）。
FROM rust:1.98.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p payment

FROM debian:bookworm-slim
COPY --from=build /src/target/release/payment /usr/local/bin/payment
USER nobody
EXPOSE 8080
CMD ["payment"]
