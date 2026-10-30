# 模擬の決済サービスのイメージ（compose から使う）。
FROM rust:1.98.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p payment

FROM debian:bookworm-slim
COPY --from=build /src/target/release/payment /usr/local/bin/payment
# 決済の記録（JOURNAL）を置く場所。compose ではボリュームを当てる
RUN mkdir /data && chown nobody /data
VOLUME /data
ENV JOURNAL=/data/charges.jsonl
USER nobody
EXPOSE 8080
CMD ["payment"]
