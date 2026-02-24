FROM ubuntu:24.04

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy pre-built binary from repo
COPY bin/polyalpha /app/polyalpha
RUN chmod +x /app/polyalpha

COPY config/ /app/config/

ENV RUST_LOG=info,polymarket_client_sdk::serde_helpers=error
ENV RUN_MODE=production

EXPOSE 18381

CMD ["/app/polyalpha"]
