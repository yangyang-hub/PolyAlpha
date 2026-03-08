# Stage 1: Build frontend
FROM node:22-slim AS frontend-builder
WORKDIR /frontend
COPY frontend/package.json frontend/package-lock.json* ./
RUN npm ci
COPY frontend/ .
RUN npm run build

# Stage 2: Final image
FROM ubuntu:24.04

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy pre-built binary from repo
COPY bin/polyalpha /app/polyalpha
RUN chmod +x /app/polyalpha

COPY config/ /app/config/
COPY migrations/ /app/migrations/

# Copy frontend build artifacts
COPY --from=frontend-builder /frontend/dist /app/frontend/dist

ENV RUST_LOG=info,polymarket_client_sdk::serde_helpers=error
ENV RUN_MODE=production

EXPOSE 18381

CMD ["/app/polyalpha"]
