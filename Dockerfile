FROM node:22-alpine AS frontend-builder

WORKDIR /app/admin-ui
# pnpm 固定 v9: 避免 v10+ 把 ignored build scripts(@swc/core,esbuild) 当硬错误退出
RUN corepack enable && corepack prepare pnpm@9 --activate
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY admin-ui ./
RUN pnpm build

FROM rust:1.92-alpine AS builder

RUN apk add --no-cache musl-dev perl make

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY --from=frontend-builder /app/admin-ui/dist /app/admin-ui/dist

RUN cargo build --release --no-default-features

FROM alpine:3.21

RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /app/target/release/xkiro-rs /app/xkiro-rs

VOLUME ["/app/config"]

EXPOSE 8990

CMD ["./xkiro-rs", "-c", "/app/config/config.json", "--credentials", "/app/config/credentials.json"]
