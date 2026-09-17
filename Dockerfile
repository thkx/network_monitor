# 多阶段构建：完整工具链编译（libsqlite3-sys bundled 需要cc）→ 精简运行时
FROM rust:1-bookworm AS builder
WORKDIR /build

# 先只拷贝清单并预编译依赖：源码改动不会使依赖层缓存失效
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src

# 拷贝真实源码；embed_migrations! 编译期内嵌迁移，migrations 必须参与构建
COPY src ./src
COPY migrations ./migrations
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
# 运行时仅需要CA证书（HTTPS探测）；SQLite已bundled进二进制，无需额外运行库
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/network_monitor /usr/local/bin/network_monitor

# 数据库与日志写到卷目录；BIND_ADDR=0.0.0.0 使容器外可访问
# （默认127.0.0.1仅容器内可达，切不可在容器环境回退到默认值）
ENV DATABASE_URL=/data/monitor.db \
    RESULT_RETENTION_DAYS=30 \
    RUST_LOG=info \
    BIND_ADDR=0.0.0.0
VOLUME /data

# 监控配置不进镜像（避免自动导入示例目标），按需挂载：
#   -v ./monitor_list.json:/app/monitor_list.json  并将 WORKDIR 数据卷对齐
WORKDIR /app
EXPOSE 8080
ENTRYPOINT ["network_monitor", "server", "--port", "8080"]
