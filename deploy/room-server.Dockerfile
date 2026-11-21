# 常駐房間（crates/room-server）：只轉發、沒有音訊，邀請票存在 volume，重開後不變。
# 自架 relay 權杖用 BuildKit secret 傳入（不會留在映像的建置紀錄裡），沒給就只能用 n0 公共 relay。
# 由 deploy/docker-compose.yml 的 room-server 服務建置（context 是儲存庫根目錄）
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    --mount=type=secret,id=multivoice_relay_token,env=MULTIVOICE_RELAY_TOKEN \
    cargo build --release -p room-server && cp target/release/multivoice-room-server /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/multivoice-room-server /usr/local/bin/
VOLUME /data
ENTRYPOINT ["multivoice-room-server", "--state", "/data/room.txt"]
