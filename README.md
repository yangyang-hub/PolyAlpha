# PolyAlpha

Polymarket 量化套利交易机器人，基于 Rust 构建。通过实时监控订单簿价差，自动发现并执行 YES/NO 二元市场、NegRisk 多结果事件及跨市场相关性套利，结合 CLOB API 下单与链上 CTF（Conditional Token Framework）拆分/合并操作实现混合执行。

## 目录

- [架构概览](#架构概览)
- [套利策略](#套利策略)
- [项目结构](#项目结构)
- [快速开始](#快速开始)
- [配置说明](#配置说明)
- [Docker 部署](#docker-部署)
- [回测系统](#回测系统)
- [监控与告警](#监控与告警)
- [数据库](#数据库)
- [API 与合约](#api-与合约)
- [开发指南](#开发指南)

## 架构概览

```
┌──────────────────────────────────────────────────────────────────┐
│                         PolyAlpha Bot                            │
│                                                                  │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────────────┐  │
│  │ Market Data  │  │  Strategies   │  │     Execution Layer    │  │
│  │             │  │              │  │                        │  │
│  │ Gamma API   │──▶│ YesNo Arb    │──▶│ CLOB Executor (API)   │  │
│  │ (discovery) │  │ NegRisk Arb  │  │ CTF Executor (on-chain)│  │
│  │             │  │ CrossMkt Arb │  │ Hybrid Orchestrator    │  │
│  │ WebSocket   │  │              │  │                        │  │
│  │ (orderbook) │  │ Strategy     │  │  ┌─────┐  ┌────────┐  │  │
│  │             │  │ Engine       │  │  │ FOK │  │ Split/ │  │  │
│  │ OB Cache    │  │              │  │  │Order│  │ Merge  │  │  │
│  └──────┬──────┘  └──────┬───────┘  │  └─────┘  └────────┘  │  │
│         │                │          └────────────────────────┘  │
│         │                │                                      │
│  ┌──────▼──────┐  ┌──────▼───────┐  ┌────────────────────────┐  │
│  │  Storage     │  │ Risk Manager │  │      Monitoring        │  │
│  │             │  │              │  │                        │  │
│  │ PostgreSQL  │  │ Position Lim │  │ Prometheus Metrics     │  │
│  │ Snapshots   │  │ Daily Loss   │  │ Health Checks          │  │
│  │ Trades      │  │ Circuit Brkr │  │ Grafana Dashboard      │  │
│  └─────────────┘  └──────────────┘  └────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

### 核心工作流

1. **市场发现** — 通过 Gamma API 获取所有活跃的 Polymarket 二元市场
2. **实时数据** — WebSocket 订阅订单簿更新（最多 500 instruments/连接），缓存至内存
3. **策略扫描** — 事件驱动 + 定时轮询双模式，检测所有已注册策略的套利机会
4. **风控检查** — 仓位限制、日损失限制、滑点保护、熔断机制
5. **混合执行** — CLOB API（FOK 订单）+ Polygon 链上 CTF 合约（split/merge）
6. **持续记录** — 交易记录入库，订单簿快照每分钟持久化，全链路 Prometheus 指标

## 套利策略

### 1. YesNo Merge/Split（二元市场套利）

Polymarket 的每个二元市场都有 YES 和 NO 两个 token，理论价格之和 = $1.00。

- **Merge（买入套利）**: 当 `ask(YES) + ask(NO) < $1.00` 时，买入双方 token 后合并为 USDC
- **Split（卖出套利）**: 当 `bid(YES) + bid(NO) > $1.00` 时，拆分 USDC 为双方 token 后卖出

最小利润空间约 2.5-3%（Polymarket 2% 手续费 + Polygon gas）。

### 2. NegRisk 多结果套利

NegRisk 事件（如"谁将赢得选举？"）包含 N 个互斥结果市场，所有 YES 价格之和理论上 = $1.00。

- 当 `sum(ask[YES_i]) < $1.00` 时，买入所有 YES token 并通过 NegRiskAdapter 合并
- 结果数越多，定价偏离越频繁

### 3. CrossMarket 跨市场套利

检测不同二元市场间的相关性（通过问题文本的词汇相似度自动发现）。

- 例如："X 在6月前发生吗？" vs "X 在12月前发生吗？"
- 两个市场 YES 价格之和偏离理论值时进行套利
- 支持 ComplementaryYes 和 InverseYesNo 两种相关性模式
- Gas 成本翻倍（两笔独立链上交易）

### 手续费模型

Polymarket 采用封顶手续费模型：

```
fee = min(fee_rate × price, price × (1 - price))
```

这意味着极端价格（接近 0 或 1）的手续费会被封顶，对套利有利。

## 项目结构

```
PolyAlpha/
├── src/
│   ├── main.rs                     # 主入口：初始化、连接、交易循环
│   └── bin/
│       └── backtest.rs             # 回测 CLI 工具
├── crates/
│   ├── pa-core/                    # 核心类型、traits、配置、错误定义
│   │   └── src/
│   │       ├── config.rs           # Settings 配置结构（TOML + 环境变量）
│   │       ├── error.rs            # 统一错误类型
│   │       ├── traits.rs           # MarketDataFeed, Strategy, Executor, RiskManager
│   │       └── types.rs            # MarketInfo, OrderBook, ArbitrageOpportunity, ...
│   ├── pa-market-data/             # 市场数据采集层
│   │   └── src/
│   │       ├── gamma_feed.rs       # Gamma API 市场发现 + NegRisk 事件分组
│   │       ├── ws_feed.rs          # WebSocket 订单簿流（含断线重连 + 指数退避）
│   │       ├── cache.rs            # DashMap 并发订单簿缓存
│   │       ├── orderbook.rs        # 订单簿构建与排序
│   │       └── service.rs          # MarketDataService（组合 Gamma + WS + Cache）
│   ├── pa-strategy/                # 套利策略实现
│   │   └── src/
│   │       ├── yes_no.rs           # YesNo Merge/Split 策略
│   │       ├── neg_risk.rs         # NegRisk 多结果策略
│   │       ├── cross_market.rs     # 跨市场相关性策略 + 自动配对检测
│   │       ├── profitability.rs    # 利润计算器（含封顶手续费模型）
│   │       ├── engine.rs           # StrategyEngine（事件驱动 + 定时扫描）
│   │       └── detector.rs         # 通用机会检测辅助
│   ├── pa-execution/               # 执行层
│   │   └── src/
│   │       ├── clob_executor.rs    # Polymarket CLOB API 下单（FOK）
│   │       ├── ctf_executor.rs     # 链上 CTF 合约调用（split/merge/NegRisk）
│   │       ├── orchestrator.rs     # HybridOrchestrator（自动选择执行路径）
│   │       ├── gas_oracle.rs       # Polygon gas 估算
│   │       └── nonce_manager.rs    # Nonce 并发管理
│   ├── pa-risk/                    # 风险管理
│   │   └── src/
│   │       ├── manager.rs          # RiskManagerImpl（线程安全）
│   │       ├── limits.rs           # 仓位/敞口限制
│   │       ├── circuit_breaker.rs  # 熔断器（连续亏损 / 日损失上限）
│   │       ├── position.rs         # 持仓追踪
│   │       └── pnl.rs             # PnL 追踪器
│   ├── pa-storage/                 # 数据持久化
│   │   └── src/
│   │       ├── repository.rs       # PostgreSQL CRUD（sqlx）
│   │       └── models.rs           # 数据库模型（MarketRow, TradeRow, ...）
│   ├── pa-backtest/                # 回测引擎
│   │   └── src/
│   │       ├── engine.rs           # BacktestEngine（快照回放 + 策略管线）
│   │       ├── data_loader.rs      # 从 DB 加载历史数据 → SnapshotFrame
│   │       ├── simulator.rs        # TradeSimulator（滑点 + 手续费模拟）
│   │       └── report.rs           # BacktestResult（PnL、Sharpe、最大回撤、胜率）
│   └── pa-monitor/                 # 监控
│       └── src/
│           ├── metrics.rs          # 13 个 Prometheus 指标
│           ├── health.rs           # /health + /ready + /metrics HTTP 端点
│           └── alerts.rs           # 告警（Webhook）
├── config/
│   └── default.toml                # 默认配置文件
├── docker/
│   ├── Dockerfile                  # 多阶段构建（依赖缓存优化）
│   ├── docker-compose.yml          # bot + postgres + prometheus + grafana
│   ├── init.sql                    # 数据库初始化 DDL
│   ├── prometheus.yml              # Prometheus 采集配置
│   └── grafana/                    # Grafana 自动配置
│       ├── provisioning/
│       │   ├── datasources/datasource.yml
│       │   └── dashboards/dashboard.yml
│       └── dashboards/
│           └── polyalpha-overview.json   # 11 面板总览仪表盘
├── migrations/                     # sqlx 数据库迁移
│   ├── 001_create_markets.sql
│   ├── 002_create_orderbooks.sql
│   ├── 003_create_trades.sql
│   ├── 004_create_opportunities.sql
│   └── 005_create_positions.sql
└── .env.example                    # 环境变量模板
```

## 快速开始

### 前置要求

- **Rust** >= 1.88.0（Edition 2024）
- **PostgreSQL** >= 14
- **Docker** + Docker Compose（可选，用于一键部署）
- Polygon 钱包私钥（持有 MATIC 用于 gas，USDC 用于交易）

### 本地开发

```bash
# 1. 克隆项目
git clone <repo-url> && cd PolyAlpha

# 2. 复制并编辑环境变量
cp .env.example .env
# 编辑 .env，填入你的私钥和数据库连接

# 3. 启动 PostgreSQL（如果没有使用 Docker）
# 确保数据库已创建且可连接

# 4. 编译检查
cargo check --workspace

# 5. 运行测试（27 个测试）
cargo test --workspace

# 6. 启动机器人
cargo run --release
```

### Docker 部署

> **注意**: docker-compose 仅管理 PolyAlpha bot 容器。PostgreSQL、Prometheus、Grafana 需独立部署（参考下方「独立安装 Grafana + Prometheus」章节）。

```bash
cd docker

# 启动 bot（使用 host 网络，直接访问宿主机上的 PostgreSQL 等服务）
docker compose up -d

# 查看日志
docker compose logs -f polyalpha

# 健康检查
curl http://localhost:8080/health
```

## 配置说明

配置采用分层加载机制（优先级从高到低）：

1. 环境变量（`PA_` 前缀，`__` 分隔，如 `PA_CHAIN__RPC_URL`）
2. `config/{RUN_MODE}.toml`（默认 `RUN_MODE=default`）
3. `config/default.toml`

### 完整配置参考

```toml
[chain]
chain_id = 137                              # Polygon Mainnet
rpc_url = "https://polygon-rpc.com"         # 主 RPC 节点
rpc_fallbacks = ["https://rpc.ankr.com/polygon"]  # 备用节点

[clob]
host = "https://clob.polymarket.com"                # CLOB REST API
ws_host = "wss://ws-subscriptions-clob.polymarket.com"  # CLOB WebSocket

[gamma]
host = "https://gamma-api.polymarket.com"   # Gamma 市场发现 API

[strategy]
enabled = ["yes_no"]                        # 启用的策略列表
scan_interval_ms = 100                      # 定时扫描间隔（毫秒）
min_spread_bps = 300                        # 最小价差（基点，300 = 3%）
min_profit_usdc = 0.50                      # 最小利润阈值（USDC）
max_trade_size_usdc = 500.0                 # 单笔最大交易量（USDC）
order_type = "FOK"                          # 订单类型（Fill-or-Kill）

[risk]
max_position_per_market = 2000.0            # 单市场最大仓位（USDC）
max_total_exposure = 10000.0                # 总敞口上限（USDC）
max_daily_loss = 500.0                      # 日最大亏损（USDC）
circuit_breaker_loss = 1000.0               # 熔断器触发亏损额
circuit_breaker_consecutive_losses = 5      # 连续亏损触发次数
max_slippage_bps = 50                       # 最大允许滑点（基点）

[database]
url = "postgresql://polyalpha:polyalpha@localhost:5432/polyalpha"
max_connections = 10                        # 连接池大小

[monitor]
prometheus_port = 9090                      # 已废弃，metrics 通过 health_port 暴露
health_port = 8080                          # 健康检查 + Metrics 端口
alert_webhook = ""                          # Webhook 告警地址（可选）

[market_filter]
min_liquidity = 1000.0                      # 最小流动性筛选
min_volume_24h = 5000.0                     # 24h 最小交易量
max_markets = 200                           # 最大监控市场数
ws_max_instruments = 450                    # WS 最大订阅数（上限 500）
```

### 环境变量

| 变量 | 说明 | 必填 |
|------|------|------|
| `POLYMARKET_PRIVATE_KEY` | Polygon 钱包私钥（hex 格式） | 是 |
| `PA_DATABASE__URL` | PostgreSQL 连接字符串 | 是 |
| `PA_CHAIN__RPC_URL` | Polygon RPC 节点 URL | 否（有默认值） |
| `RUN_MODE` | 配置模式（default/production/testnet） | 否 |
| `RUST_LOG` | 日志级别（info/debug/trace） | 否 |

## Docker 部署

docker-compose 仅管理 PolyAlpha bot 容器，使用 `network_mode: host` 直接访问宿主机上的 PostgreSQL、Prometheus 等服务。

```bash
cd docker && docker compose up -d
```

确保 `.env` 中的 `PA_DATABASE__URL` 指向正确的 PostgreSQL 地址。

### Grafana 仪表盘

包含 11 个面板：

| 面板 | 类型 | 指标 |
|------|------|------|
| Realized PnL | 时序图 | `realized_pnl_usd` |
| Total Exposure | 仪表盘 | `total_exposure_usd` |
| Circuit Breaker | 状态 | `circuit_breaker_active` |
| Monitored Markets | 数值 | `monitored_markets` |
| Active Subscriptions | 数值 | `active_ws_subscriptions` |
| Snapshots Recorded | 数值 | `rate(snapshots_recorded_total[5m])` |
| Opportunities Rate | 时序图 | `rate(opportunities_detected_total[5m])` |
| Execution Rate & Errors | 时序图 | `rate(executions_total[5m])` |
| Execution Latency | 时序图 | `histogram_quantile(0.5/0.95, execution_latency_seconds)` |
| Scan Latency | 热力图 | `scan_latency_seconds_bucket` |
| WS Reconnections | 柱状图 | `rate(ws_reconnect_total[5m])` |

## 独立安装 Grafana + Prometheus

如果不使用 Docker 全栈部署，可以单独安装 Prometheus 和 Grafana 接入机器人的监控指标。

### 数据流

```
PolyAlpha (:8080/metrics)  →  Prometheus (:9090)  →  Grafana (:3000)
       暴露指标                    采集+存储               可视化
```

### 第一步：安装 Prometheus

```bash
# Ubuntu/Debian
sudo apt install prometheus

# 或手动下载: https://prometheus.io/download/
```

编辑 Prometheus 配置，添加 PolyAlpha 采集目标：

```yaml
# /etc/prometheus/prometheus.yml — 在 scrape_configs 下追加
scrape_configs:
  - job_name: 'polyalpha'
    scrape_interval: 15s
    metrics_path: '/metrics'
    static_configs:
      - targets: ['localhost:8080']  # 机器人的 health_port
```

> 如果机器人运行在远程机器（如 `192.168.31.8`），将 `localhost:8080` 替换为 `192.168.31.8:8080`。

启动并验证：

```bash
sudo systemctl restart prometheus
# 访问 http://localhost:9090/targets — polyalpha 状态应为 UP
```

### 第二步：安装 Grafana

```bash
# Ubuntu/Debian
sudo apt install -y adduser libfontconfig1 musl
wget https://dl.grafana.com/oss/release/grafana_11.5.2_amd64.deb
sudo dpkg -i grafana_11.5.2_amd64.deb
sudo systemctl enable grafana-server
sudo systemctl start grafana-server
```

### 第三步：配置数据源

1. 浏览器打开 `http://localhost:3000`（默认账号 `admin` / `admin`）
2. 左侧菜单 → **Connections** → **Data sources** → **Add data source**
3. 选择 **Prometheus**
4. URL 填写 `http://localhost:9090`
5. 点击 **Save & Test**，显示绿色 ✓ 即成功

### 第四步：导入仪表盘

1. 左侧菜单 → **Dashboards** → **Import**
2. 点击 **Upload JSON file**，选择项目中的：
   ```
   docker/grafana/dashboards/polyalpha-overview.json
   ```
3. 在 **DS_PROMETHEUS** 下拉框中选择上一步添加的 Prometheus 数据源
4. 点击 **Import**

完成后即可看到 11 个面板的实时监控仪表盘。

### 验证数据链路

```bash
# 1. 确认机器人在暴露指标
curl http://localhost:8080/metrics

# 应看到类似输出:
# opportunities_detected_total 0
# realized_pnl_usd 0
# monitored_markets 200

# 2. 确认 Prometheus 能采集
# 访问 http://localhost:9090/targets — polyalpha 应显示 UP

# 3. 在 Grafana 中查看仪表盘
# 访问 http://localhost:3000 → Dashboards → PolyAlpha Overview
```

## 回测系统

回测引擎从 PostgreSQL 加载历史订单簿快照，按时间顺序回放每一帧数据到策略管线中，模拟真实交易执行（含滑点、手续费、gas）。

### CLI 使用

```bash
# 文本报告
cargo run --bin backtest -- \
  --from "2025-01-01T00:00:00" \
  --to "2025-01-31T23:59:59" \
  --balance 10000 \
  --slippage-bps 10 \
  --output text

# JSON 输出（供程序处理）
cargo run --bin backtest -- \
  --from "2025-01-01T00:00:00" \
  --to "2025-01-31T23:59:59" \
  --output json

# 指定数据库
cargo run --bin backtest -- \
  --from "2025-06-01T00:00:00" \
  --to "2025-06-30T23:59:59" \
  --database-url "postgresql://user:pass@host:5432/db"
```

### CLI 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--from` | 必填 | 开始时间（格式：`2025-01-01T00:00:00`） |
| `--to` | 必填 | 结束时间 |
| `--balance` | 10000 | 初始模拟余额（USDC） |
| `--slippage-bps` | 10 | 模拟滑点（基点，10 = 0.1%） |
| `--fill-ratio` | 1.0 | FOK 订单成交率（1.0 = 100%） |
| `--gas-cost` | 0.01 | 单笔链上交易 gas（USD） |
| `--fee-rate-bps` | 200 | Taker 手续费率（基点，200 = 2%） |
| `--output` | text | 输出格式（text / json） |
| `--database-url` | 配置文件 | 数据库连接字符串 |

### 报告指标

- **Total PnL** — 净盈亏（扣除手续费和 gas）
- **Win Rate** — 盈利交易占比
- **Sharpe Ratio** — 风险调整后收益
- **Max Drawdown** — 最大回撤
- **ROI** — 投资回报率
- **Per-Strategy Breakdown** — 按策略类型分组的统计

### 回测流程

```
DB (snapshots) → DataLoader → SnapshotFrame[]
                                    │
     ┌──────────────────────────────┘
     ▼
 For each frame:
   1. 更新共享订单簿状态
   2. 运行所有策略 scan()
   3. 风控检查 (RiskManager)
   4. 模拟执行 (TradeSimulator)
   5. 记录结果
                    │
                    ▼
            BacktestResult
   (PnL curve, Sharpe, drawdown, win rate)
```

## 监控与告警

### HTTP 端点

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /health` | 200 | 健康状态 JSON（含各项检查状态、运行时间） |
| `GET /ready` | 200/503 | K8s 就绪探针（所有检查通过返回 200） |
| `GET /metrics` | 200 | Prometheus 文本格式指标 |

### 健康检查

```json
{
  "status": "healthy",
  "service": "polyalpha",
  "uptime_seconds": 3600,
  "checks": {
    "websocket": "ok"
  }
}
```

`status` 取值：`healthy`（全部通过）、`degraded`（部分失败）。

### Prometheus 指标（13 个）

| 指标 | 类型 | 说明 |
|------|------|------|
| `opportunities_detected_total` | Counter | 检测到的套利机会总数 |
| `opportunities_rejected_total` | Counter | 被风控拒绝的机会数 |
| `executions_total` | Counter | 执行尝试总数 |
| `execution_errors_total` | Counter | 执行错误总数 |
| `execution_latency_seconds` | Histogram | 执行延迟（P50/P95/P99） |
| `realized_pnl_usd` | Gauge | 已实现盈亏（USD） |
| `active_ws_subscriptions` | Gauge | 活跃 WebSocket 订阅数 |
| `monitored_markets` | Gauge | 监控中的市场数 |
| `ws_reconnect_total` | Counter | WebSocket 重连次数 |
| `snapshots_recorded_total` | Counter | 已记录的订单簿快照数 |
| `circuit_breaker_active` | Gauge | 熔断器状态（1=触发, 0=正常） |
| `total_exposure_usd` | Gauge | 当前总敞口（USD） |
| `scan_latency_seconds` | Histogram | 策略扫描周期延迟 |

## 数据库

### Schema

```
markets              # 市场元数据
  ├── condition_id (PK)
  ├── question_id, question, neg_risk
  ├── tick_size, fee_rate_bps, active
  └── created_at, updated_at

tokens               # YES/NO Token 信息
  ├── token_id (PK)
  ├── condition_id (FK → markets)
  ├── outcome (Yes/No)
  └── complement_id

orderbook_snapshots  # 订单簿快照（回测用）
  ├── id (PK)
  ├── token_id, timestamp
  ├── bids (JSONB), asks (JSONB)
  └── best_bid, best_ask, midpoint

opportunities        # 套利机会记录
  ├── id (UUID PK)
  ├── strategy_type, condition_id (FK)
  ├── spread, estimated_profit, actual_profit
  ├── status, detected_at, executed_at
  └── details (JSONB)

trades               # 交易记录
  ├── id (UUID PK)
  ├── opportunity_id (FK → opportunities)
  ├── token_id, side, price, size
  ├── filled_size, fee, tx_type, tx_hash
  └── status, created_at

positions            # 当前持仓
  ├── token_id (PK)
  ├── condition_id (FK)
  ├── size, avg_cost
  └── updated_at

pnl_log              # PnL 日志
  ├── id (PK)
  ├── timestamp
  ├── realized_pnl, unrealized_pnl
  └── total_exposure, usdc_balance
```

### 快照录制

机器人运行时，后台任务每 **60 秒**从内存订单簿缓存读取所有 token 的快照并写入 `orderbook_snapshots` 表，为回测积累历史数据。

## API 与合约

### Polymarket API

| 服务 | URL | 用途 |
|------|-----|------|
| Gamma API | `https://gamma-api.polymarket.com` | 市场发现、元数据 |
| CLOB API | `https://clob.polymarket.com` | 下单、撤单、认证 |
| CLOB WebSocket | `wss://ws-subscriptions-clob.polymarket.com` | 实时订单簿推送 |

### Polygon 合约

| 合约 | 地址 | 用途 |
|------|------|------|
| CTFExchange | `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E` | 条件代币交易 |
| ConditionalTokens | `0x4D97DCd97eC945f40cF65F87097ACe5EA0476045` | Split/Merge 操作 |
| NegRiskExchange | `0xC5d563A36AE78145C45a50134d48A1215220f80a` | NegRisk 适配器 |
| USDC (PoS) | `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` | 抵押代币 |

### 链参数

- **Chain ID**: 137 (Polygon PoS)
- **出块时间**: ~2 秒
- **Gas 成本**: ~$0.01/tx
- **需要**: ERC-1155 approval（CTF split/merge 前）

## 开发指南

### 编译与测试

```bash
# 编译检查（不生成二进制）
cargo check --workspace

# 运行所有测试
cargo test --workspace

# 编译 release 版本
cargo build --release

# 编译回测 CLI
cargo build --bin backtest --release
```

### Crate 依赖关系

```
pa-core (核心类型/traits)
  ├── pa-market-data (依赖 pa-core, pa-monitor)
  ├── pa-strategy (依赖 pa-core, pa-monitor, pa-market-data)
  ├── pa-execution (依赖 pa-core)
  ├── pa-risk (依赖 pa-core)
  ├── pa-storage (依赖 pa-core)
  ├── pa-backtest (依赖 pa-core, pa-market-data, pa-strategy, pa-risk, pa-storage)
  └── pa-monitor (依赖 pa-core)
```

### 关键 Trait

```rust
// 市场数据源
trait MarketDataFeed {
    async fn subscribe(&self, token_ids: &[U256]) -> Result<()>;
    async fn get_orderbook(&self, token_id: U256) -> Result<OrderBook>;
    async fn discover_markets(&self) -> Result<Vec<MarketInfo>>;
}

// 套利策略
trait Strategy {
    fn name(&self) -> &str;
    fn strategy_type(&self) -> StrategyType;
    async fn scan(&self, markets: &[MarketInfo]) -> Result<Vec<ArbitrageOpportunity>>;
}

// 执行器
trait Executor {
    async fn execute(&self, opp: &ArbitrageOpportunity) -> Result<ExecutionResult>;
    async fn cancel_all(&self) -> Result<()>;
}

// 风控
trait RiskManager {
    fn check_pre_trade(&self, opp: &ArbitrageOpportunity) -> RiskDecision;
    fn update_position(&self, result: &ExecutionResult);
    fn is_circuit_broken(&self) -> bool;
}
```

### 添加新策略

1. 在 `crates/pa-strategy/src/` 下创建新模块（如 `my_strategy.rs`）
2. 实现 `Strategy` trait
3. 在 `crates/pa-strategy/src/lib.rs` 中注册 `pub mod my_strategy;`
4. 在 `pa-core/src/types.rs` 中的 `StrategyType` 枚举添加变体
5. 在 `pa-core/src/types.rs` 中的 `ExecutionPlan` 枚举添加变体（如需新执行路径）
6. 在 `pa-execution/src/orchestrator.rs` 和 `pa-backtest/src/simulator.rs` 中添加 match arm
7. 在 `src/main.rs` 和 `crates/pa-backtest/src/engine.rs` 中注册策略实例

### 技术栈

| 组件 | 技术 |
|------|------|
| 语言 | Rust (Edition 2024) |
| 异步运行时 | Tokio |
| 区块链交互 | Alloy (非 ethers-rs) |
| Polymarket SDK | polymarket-client-sdk v0.4 |
| 数据库 | PostgreSQL + sqlx |
| 并发缓存 | DashMap |
| 监控 | Prometheus + Grafana |
| Web 框架 | Axum |
| CLI | Clap v4 |
| 容器化 | Docker + Docker Compose |

## 风险提示

本项目仅供学习和研究目的。在使用前请注意：

- 套利交易存在资金损失风险，包括但不限于滑点、网络延迟、合约风险
- 请确保充分理解 Polymarket 的交易规则和手续费结构
- 建议先使用回测系统验证策略表现，再投入真实资金
- 请勿将超过承受范围的资金用于自动化交易
- 确保私钥安全，切勿将 `.env` 文件提交到版本控制

## License

Private / All Rights Reserved
