# syntax=docker/dockerfile:1.7
# ─── 前端 ─────────────────────────────────────────────────────────────────────
FROM node:22-alpine AS frontend-builder

WORKDIR /app/admin-ui
# pnpm 固定 v9: 避免 v10+ 把 ignored build scripts(@swc/core,esbuild) 当硬错误退出
RUN corepack enable && corepack prepare pnpm@9 --activate

# 先只拷 lockfile → pnpm install 层独立缓存，lockfile 不变则跳过
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml ./
RUN --mount=type=cache,id=pnpm-store,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile

COPY admin-ui ./
RUN pnpm build

# ─── Rust 编译 ────────────────────────────────────────────────────────────────
FROM rust:1.92-alpine AS builder

RUN apk add --no-cache musl-dev perl make

WORKDIR /app

# 先只拷 Cargo 清单 → 构造空 src/main.rs 预热依赖编译
# 依赖层独立缓存：只有 Cargo.lock 变化才重编依赖
COPY Cargo.toml Cargo.lock* ./
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=cargo-target,target=/app/target \
    mkdir -p src && echo 'fn main(){}' > src/main.rs && \
    cargo build --release --no-default-features 2>&1 | tail -5; \
    rm src/main.rs

# 拷真实源码 + 前端产物 → 只重编业务代码，依赖已缓存
COPY src ./src
COPY --from=frontend-builder /app/admin-ui/dist /app/admin-ui/dist
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=cargo-target,target=/app/target \
    # touch 让 cargo 识别源码变化
    find src -name '*.rs' | xargs touch && \
    cargo build --release --no-default-features && \
    cp target/release/xkiro-rs /app/xkiro-rs-bin

# ─── 运行镜像 ─────────────────────────────────────────────────────────────────
FROM alpine:3.21

RUN apk add --no-cache ca-certificates

WORKDIR /app
COPY --from=builder /app/xkiro-rs-bin /app/xkiro-rs

VOLUME ["/app/config"]
EXPOSE 8991
CMD ["./xkiro-rs", "-c", "/app/config/config.json", "--credentials", "/app/config/credentials.json"]
