# PolyAlpha

Polymarket 量化方向性 Alpha 交易机器人，基于 Rust 构建。

**方向性策略**：基于外部数据源（天气预报、加密货币实时价格、市场到期时间、智能钱包跟单）构建概率模型，识别 mispriced tokens 并方向性买入。支持模型反转退出和万能止损安全网。

**流动性奖励做市**：自动在有奖励的市场挂单做市，赚取 Polymarket 流动性奖励。支持余额感知动态仓位、失败冷却、重复检测、市场级配置覆盖。

**风控增强**：事件日历过滤器在 FOMC、CPI、Token Unlock 等重大事件窗口期自动降低仓位上限。三层仓位积累检查 + 熔断机制。

- **语言**: Rust Edition 2024, MSRV 1.88.0
- **代码量**: ~19500 行
- **测试**: 231 个（全部通过）
- **实盘模式无需 PostgreSQL** — 仓位从 Polymarket Data API 加载

## 目录

- [架构概览](#架构概览)
- [方向性策略](#方向性策略)
- [流动性奖励做市](#流动性奖励做市)
- [Strategy Engine](#strategy-engine)
- [仓位管理](#仓位管理)
- [事件日历过滤器](#事件日历过滤器)
- [流动性奖励做市](#流动性奖励做市)
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
┌───────────────────────────────────────────────────────────────────────────┐
│                            PolyAlpha Bot                                  │
│                                                                           │
│  ┌──────────────────┐  ┌───────────────────┐  ┌───────────────────────┐  │
│  │   Market Data     │  │    Strategies      │  │   Execution Layer     │  │
│  │                   │  │                    │  │                       │  │
│  │ Gamma API         │─▶│ Weather Alpha      │─▶│ CLOB Executor (API)   │  │
│  │ (discovery)       │  │ CryptoAlpha        │  │ Hybrid Orchestrator   │  │
│  │                   │  │ Convergence        │  │ SafeRedeemer (Gnosis) │  │
│  │ WebSocket         │  │ SmartMoney         │  │                       │  │
│  │ (orderbook, sort) │  │ LiquidityRewards   │  │  ┌─────┐             │  │
│  │                   │  │                    │  │  │ FOK │             │  │
│  │ OB Cache (DashMap)│  │ Strategy Engine     │  │  │Order│             │  │
│  │                   │  │ ├─ Depth Validate  │  │  └─────┘             │  │
│  │ Data API          │  │ ├─ Budget Track    │  └───────────────────────┘  │
│  │                   │  │ ├─ Model Exit      │                             │
│  │ Event Calendar    │  │ └─ Stop-Loss Net   │                             │
│  └────────┬─────────┘  └────────┬──────────┘                              │
│           │                      │                                         │
│  ┌────────▼─────────┐  ┌────────▼──────────┐  ┌───────────────────────┐  │
│  │    Storage        │  │  Risk Manager      │  │     Monitoring        │  │
│  │                   │  │                    │  │                       │  │
│  │ PostgreSQL        │  │ 3-layer Position   │  │ 22 Prometheus Metrics │  │
│  │ (backtest only)   │  │ Per-Market Accum   │  │ Health/Ready/Metrics  │  │
│  │                   │  │ Per-Strategy Limit  │  │ Grafana 18-panel     │  │
│  │ Data API          │  │ Circuit Breaker    │  │ Dashboard             │  │
│  │ (live positions)  │  │                    │  │                       │  │
│  └───────────────────┘  └────────────────────┘  └───────────────────────┘  │
└───────────────────────────────────────────────────────────────────────────┘
```

### 核心工作流

1. **市场发现** — Gamma API 获取活跃市场（NegRisk 自动分组 + 二元事件分组）
2. **实时数据** — Smart WS 订阅（持仓优先 > 策略相关 > 一般，最多 500 instruments）
3. **策略扫描** — 事件驱动 + 定时轮询双模式，检测方向性 Alpha
4. **事件过滤** — 事件日历在 FOMC/CPI 等窗口期自动缩减仓位
5. **风控检查** — 三层仓位积累 + 日损失限制 + 熔断机制
6. **深度验证** — 按比例缩小或拒绝深度不足的机会
7. **执行** — CLOB API FOK/GTC 订单
8. **模型退出** — 方向性策略检测模型反转或资金效率信号时自动卖出
9. **止损安全网** — 扫描所有持仓，亏损 >= 50% 时强制退出
10. **自动赎回** — 已解决市场的 winning tokens 通过 GnosisSafe 自动赎回
11. **仓位同步** — 每 5 分钟与 Data API 对账，处理外部变化
12. **监控** — 22 个 Prometheus 指标 + Grafana 18 面板仪表盘

## 方向性策略

通过外部数据源构建概率模型，当模型概率与市场价格存在显著偏差（edge）时买入。

### 手续费模型

Polymarket 采用封顶手续费模型：`fee = min(fee_rate × price, price × (1 - price))`

### 1. Weather Alpha（天气 Alpha）

利用 Open-Meteo 免费天气预报 API 为 Polymarket 上的天气相关市场定价。

**二元市场模式** — 单一阈值问题（如 "温度会超过 100°F 吗？"）：
- 关键词匹配识别天气市场（temperature, rainfall, snowfall, wind）
- 解析目标日期 → 获取 Open-Meteo 预报 → 分布 CDF 概率模型
- 温度用正态分布，降水用对数正态分布，风速用 Weibull 分布
- 包含预报误差模型（`ForecastErrorConfig` 每指标独立 sigma）

**NegRisk 多结果模式** — 区间分布问题（如 "NYC 最高温度？"）：
- 解析每个结果的数值区间（"35°F or below", "36-37°F", "50°F or higher"）
- 区间概率 `P(a <= X <= b) = CDF(b) - CDF(a)`
- 双侧 YES/NO edge 检测，选最大 edge 结果买入

**增强功能**：
- 动态 sigma：`sigma = base × sqrt(max(1, days_to_event))`
- 多模型 ensemble：并行查询 GFS/ECMWF/ICON，取均值 + 模型分歧
- 预报变化检测：仅在 `|new - old| > threshold × sigma` 时交易

**启用**: `strategy.enabled` 中添加 `"weather"`

### 2. Resolution Convergence（到期收敛）

买入接近到期且价格已收敛至 0 或 1 附近的 token。市场越接近到期，结果越确定。

- 过滤: `end_date` 在 7 天内，token price > 0.93
- 时间衰减模型: 离到期越近，模型概率越高
- Kelly criterion 仓位控制 + position-aware sizing

**启用**: `strategy.enabled` 中添加 `"convergence"`

### 3. Crypto Alpha（加密货币 Alpha）

利用实时 crypto 价格 + GBM（几何布朗运动）模型为 crypto 预测市场定价。

- **支持资产**: BTC, ETH, SOL, BNB, XRP, DOGE, ADA, AVAX, DOT, POL
- 价格源: Binance（主）+ CoinGecko（备），30 日 K 线计算年化波动率
- Black-Scholes 概率: `P(S_T > K) = Phi(d)`
- 支持三种模式：二元市场、NegRisk 多结果、二元事件分组（Polymarket 特有）

**启用**: `strategy.enabled` 中添加 `"crypto"`

### 通用机制

所有方向性策略共享：
- **仓位控制**: Kelly criterion（quarter Kelly）+ max_position_usdc + position-aware sizing
- **执行**: CLOB FOK 单边买入（`DirectionalBuy`），无链上操作
- **模型退出**: 模型概率 < best_bid - buffer 时自动卖出（锁定利润或止损）
- **资金效率退出**: best_bid >= 0.98 时卖出（接近 $1 的 token 不如兑现再投资）
- **盈利检查**: `ProfitCalculator::directional_buy_profit()`

## Strategy Engine

`StrategyEngine` 是核心交易循环，事件驱动 + 定时扫描双模。

### 市场过滤

- `max_market_end_days`: 只扫描 end_date 在 N 天内的市场
- 止损扫描使用**完整**市场列表（确保所有持仓都能检查）
- 无 end_date 的市场始终包含

### 冷却机制

- `HashMap<(condition_id, StrategyType), Instant>` 追踪冷却
- 成功 10s, NoFill/Failed 120s, 深度不足 60s, 预算耗尽 120s
- 止损冷却: 300s, 过期市场 3600s, 数据可疑 3600s
- 自动剪枝: 超 500 条目时清理过期条目

### 执行暂停

- 余额/授权错误后全局暂停 5 分钟
- 批量处理中某个机会触发暂停 → 跳过剩余机会

### 预算追踪

- 每周期查询一次可用资金，逐个扣减
- 退出订单绕过预算检查

### 深度验证

- 从 ExecutionPlan 提取流动性需求，检查 orderbook 深度
- 深度不足时按比例缩小，缩小后低于 min_order_usdc 则拒绝
- 退出订单绕过深度验证

### 万能止损安全网

在策略扫描之后运行，检查**所有**持仓：

1. **触发条件**: best_bid < avg_cost × 50%
2. **安全检查**:
   - best_ask >= avg_cost 且价差合理 → 不卖（市场仍看好）
   - 过期市场 bid >= $0.10 → 跳过（让自动赎回处理）
   - 双侧 bid 都低 → 跳过（数据可能陈旧）
   - 卖出金额 < $0.05 → 太小无法成交
3. **执行**: FOK 卖出量上限为 bid 侧可用深度

## 仓位管理

### Data API 加载（无需 PostgreSQL）

- 启动时从 Polymarket Data API 加载持仓
- 策略标签推断: weather → crypto → convergence
- 未发现的市场（过期/关闭）通过 `fetch_position_markets()` 补充
- 持仓 token 加入 WS 订阅最高优先级

### 定期同步（每 5 分钟）

与 Data API 对账，处理 4 种情况：
1. 新仓位（本地无）→ 添加
2. 大小变化（部分成交/外部交易）→ 更新
3. 仓位清零（赎回等）→ 清除
4. 本地有但 API 无 → 清除

### 自动赎回（SafeRedeemer）

- 每 5 分钟扫描可赎回仓位
- 通过 GnosisSafe `execTransaction()` 赎回（代币在 Safe 代理钱包中）
- 支持普通市场和 NegRisk 市场赎回

## 事件日历过滤器

在 FOMC 利率决议、CPI 发布、Token Unlock 等重大事件前后，方向性策略的模型预测不可靠。事件日历过滤器在事件窗口期自动降低仓位上限。

### 集成方式

集中式处理，在 `StrategyEngine::scan_and_execute()` 中，策略产出机会后、执行前，乘以事件系数缩减仓位。无需修改任何策略代码。

### 事件来源

| 来源 | 类别 | 数据 |
|------|------|------|
| **Finnhub** | Macro | 美国经济日历（FOMC, CPI, NFP, GDP） |
| **CoinMarketCal** | Crypto | 加密货币事件（Token Unlock, Fork, ETF） |
| **Static (TOML)** | 全部 | 手动配置事件，支持 Macro/Crypto/Political/Sports |

### 仓位乘数

| 事件影响 | 乘数 | 说明 |
|----------|------|------|
| High | 0.25 | FOMC, CPI 等 |
| Medium | 0.50 | |
| Low | 0.75 | |

多事件重叠取最小值。事件窗口: `[event_time - 4h, event_time + 2h]`（可配置）。

**启用**: `[event_calendar] enabled = true` + API keys

## 流动性奖励做市

赚取 Polymarket CLOB 流动性奖励。作为后台任务运行（非 Strategy trait）。

- 自动发现有奖励的市场（按 reward density 排序）
- 支持三种市场模式: auto（自动发现）、manual（仅手动列表）、hybrid（两者结合）
- 余额感知动态仓位: 根据可用余额自动计算可负担仓位
- 失败订单冷却: 60s 内不重复挂同一价位的失败订单
- 重复订单检测: 对比期望订单与现有订单，只增删差异
- 市场级配置覆盖: 每个市场可独立设置 max_position、spread、quote_yes/no
- Fill 检测: 10s CLOB 轮询，部分成交跟踪，全部成交立即 re-quote
- 订单得分验证: 可选检查订单是否满足奖励计分要求

**启用**: `[liquidity_rewards] enabled = true`

## 项目结构

```
PolyAlpha/
├── src/
│   ├── main.rs                      # 主入口：初始化→WS订阅→执行→策略→做市→同步→赎回
│   └── bin/backtest.rs              # 回测 CLI (clap)
├── crates/
│   ├── pa-core/                     # 核心类型、traits、配置、错误
│   ├── pa-market-data/              # Gamma API + WS (排序) + Cache + DataAPI + EventCalendar
│   ├── pa-strategy/                 # 5 策略 + ProfitCalculator + StrategyEngine
│   ├── pa-execution/                # CLOB + CTF + Orchestrator + SafeRedeemer
│   ├── pa-risk/                     # RiskManager + PositionTracker (3 层积累检查)
│   ├── pa-storage/                  # PostgreSQL Repository (sqlx)
│   ├── pa-backtest/                 # BacktestEngine + DataLoader + Simulator + Report
│   └── pa-monitor/                  # 22 Prometheus 指标 + Health/Ready/Metrics HTTP
├── config/default.toml              # 默认配置
├── docker/                          # Docker 全栈部署 + Grafana 仪表盘
└── migrations/                      # sqlx DB 迁移 (001-006)
```

### Crate 依赖方向

```
pa-core ← pa-monitor ← pa-market-data ← pa-strategy
pa-core ← pa-execution
pa-core ← pa-risk
pa-core ← pa-storage
pa-core + pa-market-data + pa-strategy + pa-risk + pa-storage ← pa-backtest
```

**禁止循环依赖**。pa-core 不依赖任何其他内部 crate。

## 快速开始

### 前置要求

- **Rust** >= 1.88.0（Edition 2024, rustc 1.93.0 推荐）
- **Docker** + Docker Compose（可选，用于一键部署）
- Polygon 钱包私钥（持有 MATIC 用于 gas，USDC 用于交易）
- **PostgreSQL** >= 14（仅回测模式需要，实盘不需要）

### 本地开发

```bash
# 1. 克隆项目
git clone <repo-url> && cd PolyAlpha

# 2. 复制并编辑环境变量
cp .env.example .env
# 编辑 .env，填入你的私钥

# 3. 编译检查
cargo check --workspace

# 4. 运行测试（199 个测试）
cargo test --workspace

# 5. 启动机器人
cargo run --release
```

### Docker 部署

```bash
cd docker && docker compose up -d

# 查看日志
docker compose logs -f polyalpha

# 健康检查
curl http://localhost:18381/health
```

## 配置说明

配置采用分层加载机制（优先级从高到低）：

1. 环境变量（`PA_` 前缀，`__` 分隔，如 `PA_CHAIN__RPC_URL`）
2. `config/{RUN_MODE}.toml`
3. `config/default.toml`

### 核心配置参考

```toml
[chain]
chain_id = 137
rpc_url = "https://polygon-rpc.com"

[clob]
host = "https://clob.polymarket.com"
ws_host = "wss://ws-subscriptions-clob.polymarket.com"
signature_type = 2                     # 0=EOA, 1=Proxy, 2=GnosisSafe
proxy_wallet = ""                      # GnosisSafe/Proxy 钱包地址

[gamma]
host = "https://gamma-api.polymarket.com"

[strategy]
enabled = ["weather"]                  # 可选: weather, convergence, crypto, smart_money
scan_interval_ms = 100
min_spread_bps = 300
min_profit_usdc = 0.50
max_trade_size_usdc = 500.0

[risk]
max_position_per_market = 2000.0
max_total_exposure = 10000.0
max_daily_loss = 500.0
circuit_breaker_loss = 1000.0
max_exposure_per_strategy = 5000.0
max_markets_per_strategy = 50

[monitor]
health_port = 18381

[market_filter]
ws_max_instruments = 500
market_refresh_interval_secs = 1800

[weather]
min_edge_bps = 700
max_spread_bps = 1800
max_position_usdc = 5.0
kelly_fraction = 0.25
dynamic_sigma = true
forecast_change_detection = false
forecast_change_threshold = 0.35
max_entry_price = 0.30
exit_buffer_bps = 50
capital_efficiency_threshold = 0.98
target_cities = []

[weather.forecast_error]
temperature_sigma_f = 3.0
precipitation_sigma_in = 0.3
snowfall_sigma_in = 2.0
wind_sigma_mph = 5.0

[convergence]
min_price_threshold = 0.93
max_days_to_resolution = 7
max_position_usdc = 100.0
kelly_fraction = 0.25
exit_buffer_bps = 50
capital_efficiency_threshold = 0.98

[crypto_alpha]
min_edge_bps = 100
max_position_usdc = 100.0
kelly_fraction = 0.25
coingecko_api_key = ""
exit_buffer_bps = 50
capital_efficiency_threshold = 0.98

[event_calendar]
enabled = false
finnhub_api_key = ""
coinmarketcal_api_key = ""
pre_event_hours = 4
post_event_hours = 2

[liquidity_rewards]
enabled = true
max_markets = 10
max_position_per_market = 100.0
max_total_exposure = 500.0
spread_fraction = 0.80
min_order_size = 5.0
min_daily_rate = 1.0
market_mode = "auto"                  # auto / manual / hybrid
failed_cooldown_secs = 60
```

### 环境变量

| 变量 | 说明 | 必填 |
|------|------|------|
| `POLYMARKET_PRIVATE_KEY` | 账户私钥环境变量示例，供 `[[accounts]]` / `PA_ACCOUNT_<N>_PRIVATE_KEY_ENV` 引用 | 按账户配置 |
| `PA_ACCOUNT_1_NAME` | 第一个交易账户名；设置后启用 env 多账户配置 | 否 |
| `PA_ACCOUNT_1_PRIVATE_KEY_ENV` | 第一个交易账户引用的私钥环境变量名，例如 `POLYMARKET_PRIVATE_KEY` | 按账户配置 |
| `PA_DATABASE__URL` | PostgreSQL 连接字符串 | 仅回测 |
| `PA_CHAIN__RPC_URL` | Polygon RPC 节点 URL | 否 |
| `PA_CLOB__PROXY_WALLET` | GnosisSafe 代理钱包地址 | 推荐 |
| `RUST_LOG` | 日志级别（info/debug/trace） | 否 |

## Docker 部署

docker-compose 管理 bot + postgres + prometheus + grafana，使用 `network_mode: host`。

```bash
cd docker && docker compose up -d
```

### Grafana 仪表盘（18 面板）

| 行 | 面板 |
|------|------|
| **Stat Row** | USDC Balance, PnL, Exposure, Circuit Breaker, Markets |
| **Portfolio** | PnL + Balance 时序, USDC Balance |
| **Pipeline** | Opportunity Funnel (4 series), Exits & Events, MM Orders |
| **Latency** | Execution P50/P95, Scan Heatmap, WS Reconnections |

## 回测系统

回测引擎从 PostgreSQL 加载历史订单簿快照，按时间回放到策略管线中。**需要 PostgreSQL**。

```bash
cargo run --bin backtest -- \
  --from "2025-01-01T00:00:00" \
  --to "2025-01-31T23:59:59" \
  --output text

# JSON 输出
cargo run --bin backtest -- \
  --from "2025-01-01T00:00:00" \
  --to "2025-01-31T23:59:59" \
  --output json
```

### 报告指标

- **Total PnL**, **Win Rate**, **Sharpe Ratio**, **Max Drawdown**, **ROI**
- **Per-Strategy Breakdown** — 按策略类型分组统计

## 监控与告警

### HTTP 端点

| 端点 | 说明 |
|------|------|
| `GET /health` | 健康状态 JSON（含检查状态、运行时间） |
| `GET /ready` | K8s 就绪探针（200/503） |
| `GET /metrics` | Prometheus 文本格式指标 |

### Prometheus 指标（22 个）

| 指标 | 类型 | 说明 |
|------|------|------|
| `opportunities_detected_total` | Counter | 检测到的机会总数 |
| `opportunities_rejected_total` | Counter | 风控拒绝数 |
| `executions_total` | Counter | 执行尝试总数 |
| `execution_errors_total` | Counter | 执行错误总数 |
| `execution_latency_seconds` | Histogram | 执行延迟 |
| `scan_latency_seconds` | Histogram | 扫描延迟 |
| `realized_pnl_usd` | Gauge | 已实现盈亏 |
| `usdc_balance` | Gauge | USDC 余额 |
| `total_exposure_usd` | Gauge | 当前总敞口 |
| `active_ws_subscriptions` | Gauge | 活跃 WS 订阅数 |
| `monitored_markets` | Gauge | 监控中的市场数 |
| `circuit_breaker_active` | Gauge | 熔断器状态 |
| `mm_active_markets` | Gauge | LR 活跃市场数 |
| `ws_reconnect_total` | Counter | WS 重连次数 |
| `snapshots_recorded_total` | Counter | 快照记录数 |
| `event_filter_applied_total` | Counter | 事件日历降仓次数 |
| `exit_trades_total` | Counter | 退出交易次数 |
| `depth_validation_scaled_total` | Counter | 深度缩放次数 |
| `depth_validation_rejected_total` | Counter | 深度拒绝次数 |
| `mm_orders_placed_total` | Counter | LR 下单次数 |
| `mm_orders_cancelled_total` | Counter | LR 撤单次数 |

## 数据库

**实盘模式不需要 PostgreSQL** — 仓位从 Polymarket Data API 加载。

**回测模式需要 PostgreSQL** — 7 张表: markets, tokens, orderbook_snapshots, opportunities, trades, positions, pnl_log。迁移通过 `sqlx::migrate!` 执行。

## API 与合约

### Polymarket API

| 服务 | URL | 用途 |
|------|-----|------|
| Gamma API | `https://gamma-api.polymarket.com` | 市场发现、元数据 |
| CLOB API | `https://clob.polymarket.com` | 下单、撤单、认证、余额 |
| CLOB WebSocket | `wss://ws-subscriptions-clob.polymarket.com` | 实时订单簿推送 |
| Data API | `https://data-api.polymarket.com` | 仓位加载、可赎回查询 |

### Polygon 合约

| 合约 | 地址 |
|------|------|
| CTFExchange | `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E` |
| ConditionalTokens | `0x4D97DCd97eC945f40cF65F87097ACe5EA0476045` |
| NegRiskExchange | `0xC5d563A36AE78145C45a50134d48A1215220f80a` |
| USDC (PoS) | `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` |

Chain ID: 137, ~2s blocks, ~$0.01 gas, ERC-1155 approval required。

## 开发指南

### 编译与测试

```bash
cargo check --workspace          # 编译检查
cargo test --workspace           # 运行 231 个测试
cargo build --release            # 构建 release
cargo run --release              # 运行机器人
```

### 测试分布（231 个）

| Crate | 数量 | 覆盖 |
|-------|------|------|
| pa-strategy | 171 | ProfitCalc(6), Weather(79), Convergence(14), CryptoAlpha(30), LR(23), SmartMoney(5) |
| pa-market-data | 24 | OrderBook(1), EventCalendar(12), GammaFeed(4), WalletTracker(7) |
| pa-backtest | 8 | DataLoader, Report, Simulator |
| pa-execution | 11 | Gas(1), Cost precision(5), GCD(1), min_cost(4) |
| pa-core | 8 | OrderBook depth(3), walk_book(4), liquidity_requirements(1) |
| pa-risk | 9 | PositionTracker(3), RiskManager(5), exit_bypass(1) |

### 关键 Trait

```rust
trait MarketDataFeed {
    async fn subscribe(&self, token_ids: &[U256]) -> Result<()>;
    async fn get_orderbook(&self, token_id: U256) -> Result<OrderBook>;
    async fn discover_markets(&self) -> Result<Vec<MarketInfo>>;
}

trait Strategy {
    fn name(&self) -> &str;
    fn strategy_type(&self) -> StrategyType;
    async fn scan(&self, markets: &[MarketInfo]) -> Result<Vec<TradingOpportunity>>;
}

trait Executor {
    async fn execute(&self, opp: &TradingOpportunity) -> Result<ExecutionResult>;
    async fn cancel_all(&self) -> Result<()>;
}

trait RiskManager {
    fn check_pre_trade(&self, opp: &TradingOpportunity) -> RiskDecision;
    fn update_position(&self, result: &ExecutionResult);
    fn is_circuit_broken(&self) -> bool;
}
```

### 添加新策略

1. `crates/pa-strategy/src/new_strategy.rs` — 实现 Strategy trait
2. `crates/pa-strategy/src/lib.rs` — `pub mod`
3. `crates/pa-core/src/types.rs` — StrategyType + ExecutionPlan 枚举
4. `crates/pa-execution/src/orchestrator.rs` — ExecutionPlan match arm
5. `crates/pa-backtest/src/simulator.rs` — ExecutionPlan match arm
6. `src/main.rs` — 策略实例化 + 注册
7. `crates/pa-backtest/src/engine.rs` — build_strategies()
8. 测试: 至少覆盖 detect 逻辑 + profitability 计算

### 技术栈

| 组件 | 技术 |
|------|------|
| 语言 | Rust (Edition 2024, MSRV 1.88.0) |
| 异步运行时 | Tokio |
| 区块链交互 | Alloy |
| Polymarket SDK | polymarket-client-sdk v0.4 |
| 数据库 | PostgreSQL + sqlx（仅回测） |
| 并发缓存 | DashMap |
| 监控 | Prometheus + Grafana |
| Web 框架 | Axum |
| CLI | Clap v4 |
| 容器化 | Docker + Docker Compose |

## 风险提示

本项目仅供学习和研究目的。在使用前请注意：

- 方向性交易存在资金损失风险
- 请确保充分理解 Polymarket 的交易规则和手续费结构
- 建议先使用回测系统验证策略表现，再投入真实资金
- 确保私钥安全，切勿将 `.env` 文件提交到版本控制

## License

Private / All Rights Reserved
