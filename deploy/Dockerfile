# P0-6: fusion-memory 部署制品。多阶段构建。
#
# 产物: 单镜像含 fm-server + fm(CLI) 二进制。默认 stub 离线模式起服务;
# 真实 bge-m3 模式需 fusion-mlx 对端 (同网络可达 127.0.0.1:11434 或 FUSION_MLX_URL 指向)。
#
# 构建:
#   docker build -t fusion-memory:0.2.0 -f deploy/Dockerfile .
# 运行 (stub 离线, 一键起 HTTP + UDS):
#   docker run -d --name fm \
#     -p 11435:11435 \
#     -e FUSION_MEMORY_API_KEY=change-me \
#     -e FUSION_MEMORY_STUB=1 \
#     -v fm-data:/data \
#     fusion-memory:0.2.0
# 健康检查:
#   curl -sf http://127.0.0.1:11435/healthz
# 指标:
#   curl -sf http://127.0.0.1:11435/metrics
#
# 镜像 tag 版本化: fusion-memory:<workspace.version> (见 Cargo.toml [workspace.package])。
# 当前 0.2.0。

# ---------- stage 1: builder ----------
FROM rust:1.87-bookworm AS builder

# 静态依赖少 (rusqlite bundled), 系统工具最小化。
WORKDIR /build

# 先拷 manifest 利用依赖缓存层。
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# release 构建 fm-server + fm-cli (mlx-live off = stub 离线编译, 无 MLX 依赖)。
# features 空: MlxEmbedder 经 HTTP 调 fusion-mlx, 镜像内不需要 mlx 源码编译路径。
RUN cargo build --release -p fm-server -p fm-cli && \
    strip target/release/fm-server target/release/fm

# ---------- stage 2: runtime ----------
FROM debian:bookworm-slim AS runtime

# 最小运行时: ca-certificates (HTTPS 不需要, 纯 127.0.0.1, 但 curl 健康检查用) + curl。
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

# 非 root 运行 (uid 1000)。
RUN useradd --uid 1000 --create-home fm
WORKDIR /home/fm

# 拷二进制。
COPY --from=builder /build/target/release/fm-server /usr/local/bin/fm-server
COPY --from=builder /build/target/release/fm /usr/local/bin/fm

# 数据卷: FM_HOME 挂此。
RUN mkdir -p /data && chown -R fm:fm /data
VOLUME ["/data"]

USER fm

# 默认 env: stub 离线 + HTTP 11435 + 数据 /data。
# FUSION_MEMORY_API_KEY 运行时注入 (HTTP 必配, B5)。
ENV FM_HOME=/data \
    FUSION_MEMORY_STUB=1 \
    FUSION_MEMORY_HTTP_PORT=11435 \
    FUSION_MEMORY_DIM=1024 \
    RUST_LOG=info

EXPOSE 11435

# healthcheck: HTTP healthz 200 (无需鉴权)。
HEALTHCHECK --interval=15s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -sf http://127.0.0.1:11435/healthz || exit 1

CMD ["fm-server"]
