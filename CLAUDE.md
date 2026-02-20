# CLAUDE.md — PolyAlpha 项目指南

> 本文件为 Claude Code 提供项目上下文。每次代码变更后同步更新。

## 项目概要

Polymarket 量化套利交易机器人（Rust）。通过实时订单簿监控，在 YES/NO 二元市场、NegRisk 多结果事件、跨市场相关性之间发现并执行套利。此外支持基于天气预报的方向性 Alpha 策略。混合执行层：CLOB API（下单）+ Polygon 链上 CTF（split/merge）。

- **语言**: Rust Edition 2024, MSRV 1.88.0
- **工具链**: rustc 1.93.0, cargo 1.93.0
- **代码量**: ~9700 行 Rust
- **测试**: 125 个（全部通过）

## 常用命令

```bash
cargo check --workspace          # 编译检查
cargo test --workspace           # 运行全部 86 个测试
cargo build --release            # 构建 release
cargo run --release              # 运行机器人
cargo run --bin backtest -- --from "2025-01-01T00:00:00" --to "2025-01-31T23:59:59"  # 回测
cd docker && docker compose up -d  # Docker 全栈部署
```

## Workspace 结构

```
polyalpha (root binary)       # src/main.rs — 主入口, src/bin/backtest.rs — 回测CLI
├── pa-core                   # 核心类型、traits、配置、错误
├── pa-market-data            # Gamma API + WebSocket + OrderBookCache + EventCalendar
├── pa-strategy               # YesNo / NegRisk / CrossMarket / Weather / Convergence / CryptoAlpha 策略 + StrategyEngine
├── pa-execution              # ClobExecutor + CtfExecutor + HybridOrchestrator
├── pa-risk                   # RiskManagerImpl (仓位/损失限制/熔断器)
├── pa-storage                # PostgreSQL Repository (sqlx)
├── pa-backtest               # BacktestEngine + DataLoader + TradeSimulator + Report
└── pa-monitor                # Prometheus 指标(13个) + Health/Ready/Metrics HTTP
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

## 核心 Trait（pa-core/src/traits.rs）

| Trait | 方法 | 实现 |
|-------|------|------|
| `MarketDataFeed` | subscribe, unsubscribe, get_orderbook, discover_markets | `MarketDataService` |
| `Strategy` | name, strategy_type, scan | `YesNoArbitrage`, `NegRiskArbitrage`, `CrossMarketArbitrage`, `WeatherAlphaStrategy`, `ResolutionConvergenceStrategy`, `CryptoAlphaStrategy` |
| `Executor` | execute, cancel_all | `HybridOrchestrator`, `TradeSimulator` |
| `RiskManager` | check_pre_trade, update_position, is_circuit_broken, reset_daily | `RiskManagerImpl` |

所有 trait 使用 `#[async_trait]` 并要求 `Send + Sync`。

## 关键类型（pa-core/src/types.rs）

| 类型 | 说明 |
|------|------|
| `MarketInfo` | 市场元数据（condition_id, tokens, fee_rate_bps, event_title, end_date, category, outcome_prices） |
| `OrderBook` | 订单簿快照（bids 降序, asks 升序） |
| `ArbitrageOpportunity` | 检测到的套利机会（含 ExecutionPlan） |
| `ExecutionPlan` | 枚举: BuyAndMerge / SplitAndSell / NegRiskConvert / CrossMarket / DirectionalBuy |
| `ExecutionResult` | 执行结果（profit, fees, gas, status） |
| `StrategyType` | 枚举: YesNoMerge / YesNoSplit / NegRiskConvert / CrossMarket / Weather / ResolutionConvergence / CryptoAlpha |
| `NegRiskEvent` | NegRisk 事件（title + 多个 MarketInfo） |
| `BinaryEventGroup` | 二元事件分组（title + 多个独立 MarketInfo，非 NegRisk） |
| `EventCategory` | 枚举: Macro / Crypto / Political / Sports |
| `EventImpact` | 枚举: Low / Medium / High |
| `CrossMarketPair` | 跨市场配对（market_a, market_b, expected_sum, correlation） |
| `RiskDecision` | 枚举: Approve / Reject(reason) |

## 错误处理

- **pa-core**: `pa_core::Error`（thiserror 枚举）+ `pa_core::Result<T>`
- **跨 crate 边界**: 使用 `anyhow::Result` 进行错误传播
- **错误变体**: Config, Strategy, Execution, RiskCheck, MarketData, InsufficientLiquidity, OrderFailed, OnChainTxFailed, Database, CircuitBroken

## 配置系统

分层加载（优先级高→低）：
1. 环境变量 `PA_` 前缀（`PA_CHAIN__RPC_URL`）
2. `config/{RUN_MODE}.toml`
3. `config/default.toml`

关键结构: `Settings { chain, clob, gamma, strategy, risk, database, monitor, market_filter, weather, convergence, crypto_alpha, event_calendar, market_making }`

`WeatherConfig` fields: `min_edge_bps`, `max_position_usdc`, `kelly_fraction`, `forecast_error: ForecastErrorConfig`, `refresh_interval_secs`

`ForecastErrorConfig` — 每指标预报误差σ: `temperature_sigma_f(3.0°F)`, `precipitation_sigma_in(0.3in)`, `snowfall_sigma_in(2.0in)`, `wind_sigma_mph(5.0mph)`

`ConvergenceConfig` fields: `min_price_threshold(0.93)`, `max_days_to_resolution(7)`, `max_position_usdc(100)`, `kelly_fraction(0.25)`, `time_decay_boost(true)`, `time_decay_rate(0.03)`

`CryptoAlphaConfig` fields: `min_edge_bps(500, config default 100)`, `max_position_usdc(100)`, `kelly_fraction(0.25)`, `refresh_interval_secs(300)`, `coingecko_api_key("")`

`EventCalendarConfig` fields: `enabled(false)`, `finnhub_api_key("")`, `coinmarketcal_api_key("")`, `refresh_interval_secs(3600)`, `pre_event_hours(4)`, `post_event_hours(2)`, `high_impact_multiplier(0.25)`, `medium_impact_multiplier(0.50)`, `low_impact_multiplier(0.75)`, `static_events([])`

`MarketMakingConfig` fields: `enabled(false)`, `target_spread_bps(300)`, `max_position_per_market(50)`, `max_markets(5)`, `quote_refresh_secs(30)`, `inventory_skew_factor(0.50)`

## 策略模式

所有策略遵循相同模式：
1. 构造函数接收配置参数 + `Box<dyn Fn(U256) -> Option<OrderBook> + Send + Sync>` 闭包
2. 内部 `detect_*()` 方法检测单个市场/事件/配对的套利机会
3. `Strategy::scan()` 遍历市场并调用 detect
4. 利润计算通过 `ProfitCalculator`（含 Polymarket 封顶手续费模型）

**手续费**: `fee = min(fee_rate × price, price × (1 - price))`

### Weather Alpha 策略（方向性）

与套利策略不同，Weather Alpha 是有方向性风险的。支持两种模式：

**二元市场模式** — 单一阈值问题（如 "Will temperature exceed 100°F?"）：
1. 通过关键词匹配识别天气相关市场（temperature, rainfall, snowfall, wind）
2. 解析目标日期（"on Feb 14"、"today"、"tomorrow"、"2/14"）→ 单日预报
3. 检测降水单位（"mm" vs "inch"，默认 inch）
4. 调用 Open-Meteo API（免费，无需 API Key，10s超时+指数退避重试）获取天气预报
5. 使用分布CDF模型将预报转换为事件概率（温度→正态, 降水→对数正态, 风速→Weibull）
6. 比较模型概率与市场价格，检查YES和NO两侧，取更大edge的一方买入

**NegRisk 多结果模式** — 区间分布问题（如 "Highest temperature in NYC?"）：
1. 通过 `NegRiskEvent.title` 识别天气事件
2. 对每个结果市场解析数值区间（"35°F or below", "36-37°F", "50°F or higher"）
3. 使用 CDF 区间概率 `P(a ≤ X ≤ b) = CDF(b) - CDF(a)` 为每个结果建模
4. 检查每个结果的YES和NO两侧edge，选择最大edge的结果买入

**Forecast Error Model**:
- 使用绝对值 sigma 替代百分比不确定性: `ForecastErrorConfig` per metric
- 日期特定: `sigma = forecast_error_sigma`（仅预报误差）
- 多日模式: `sigma = sqrt(std_dev² + forecast_error_sigma²)`（组合方差）

**分布模型（CDF）**:
- 温度: 正态分布 `normal_cdf(z)`
- 降水/降雪: 对数正态分布 `lognormal_cdf(t, mean, sigma)`
- 风速: Weibull分布 `weibull_cdf(t, mean, sigma)` (k=2, Rayleigh)

**共通逻辑**：
- 仓位控制: Kelly criterion（quarter Kelly）+ max_position_usdc 上限 + position-aware sizing（减去已有仓位）
- 执行: CLOB FOK 单边买入（`DirectionalBuy`），无链上操作
- API: HTTP 10s timeout + 指数退避重试（500ms, 1s, 2s）
- 缓存: 带TTL驱逐的forecast cache
- 启用: 在 `strategy.enabled` 中添加 `"weather"`

### Resolution Convergence 策略（方向性）

买入接近到期的市场中价格已收敛至0或1附近的token。到期越近，结果越确定，token价格趋向最终赔付值。

**逻辑**:
1. 过滤: `end_date` 存在且在 `max_days_to_resolution` 内（默认7天），未过期
2. 过滤: 二元市场（2个token），非NegRisk，活跃
3. 获取YES/NO order book，提取best ask price
4. 选择高于 `min_price_threshold`（默认0.93）的一方（两边都符合则选更高价）
5. 模型概率: `time_decay_boost=true` 时 `model_prob = 1.0 - (days_remaining / max_days) * 0.03`
6. Edge = model_prob - ask_price，Kelly sizing + position-aware cap
7. `ProfitCalculator::directional_buy_profit()` 盈利检查
8. 执行: CLOB FOK 单边买入（`DirectionalBuy`），无链上操作
9. 启用: 在 `strategy.enabled` 中添加 `"convergence"`

### Crypto Alpha 策略（方向性）

利用实时加密货币价格数据 + GBM（几何布朗运动）模型，为 Polymarket 上的 crypto 预测市场定价，发现 mispriced tokens。支持二元市场和 NegRisk 多结果市场。

**支持资产**: BTC, ETH, SOL, BNB, XRP, DOGE, ADA, AVAX, DOT, POL（硬编码映射）

**二元市场模式**:
1. `parse_crypto_question()`: 解析市场问题 → 识别资产 + 价格阈值 + 方向（Above/Below）+ 目标日期
2. 价格数据: Binance 主 + CoinGecko 备, 当前价格 + 30日K线
3. `calculate_volatility()`: 30日 log-returns → 年化 μ（momentum drift）+ σ（volatility）
4. `gbm_probability()`: Black-Scholes 式概率 `P(S_T > K) = Φ(d)`, `d = (ln(S/K) + (μ - σ²/2)t) / (σ√t)`
5. Below 方向: `P = 1 - gbm_probability()`
6. 双侧 YES/NO edge 检测，取更大 edge
7. `edge_bps ≥ min_edge_bps` 过滤
8. Kelly sizing + position-aware cap
9. `ProfitCalculator::directional_buy_profit()` 盈利检查
10. 执行: CLOB FOK 单边买入（`DirectionalBuy`），无链上操作

**NegRisk 多结果模式** — 价格区间分布问题（如 "Bitcoin price on March 1?" → "$90k-$95k", "$95k-$100k", "$100k+"）:
1. `parse_crypto_event_title()`: 从 NegRisk event title 识别加密资产 + 目标日期
2. `parse_crypto_outcome_range()`: 解析 `CryptoPriceRange` — `AtOrBelow`, `Range`, `AtOrAbove`
3. `gbm_range_probability()`: 区间概率（AtOrBelow: 1-P(S>K), Range: P(S>lo)-P(S>hi), AtOrAbove: P(S>K)）
4. `detect_crypto_neg_risk()`: 遍历所有 outcome，双侧 YES/NO edge 检测，选最大 edge
5. Kelly sizing + position-aware cap + profitability check

**二元事件分组模式** — 分组的独立二元市场（如 "What price will Bitcoin hit in 2026?" → "Will Bitcoin reach $200,000?", "Will Bitcoin reach $150,000?"）:
1. `group_binary_events()`: 按 `event_title` 将非 NegRisk 市场分组为 `BinaryEventGroup`
2. `detect_crypto_group()`: 从组标题/问题中识别资产，一次获取价格数据
3. 遍历组内所有市场：解析问题 → GBM 概率 → 双侧 YES/NO edge 检测
4. 选择组内最大 edge 的市场，Kelly sizing + profitability check
5. 与单独二元市场去重：已分组的市场跳过个别扫描

**启用**: 在 `strategy.enabled` 中添加 `"crypto"`

**价格获取**:
- Binance: `/api/v3/ticker/price` + `/api/v3/klines?interval=1d&limit=30`（无需 API key）
- CoinGecko: `/api/v3/simple/price` + `/api/v3/coins/{id}/market_chart`（需 Demo API key）
- 带 TTL 缓存 + `with_retry(2, ...)` 指数退避重试

### Event Calendar Filter

在重大事件（FOMC、CPI、Token Unlock 等）前后，方向性策略的模型预测不可靠。事件日历过滤器在事件窗口期自动降低仓位上限。

**集成点**: `StrategyEngine::scan_and_execute()` — 策略产出机会后、执行前，乘以事件系数缩减 `size`。集中式处理，无需修改任何策略代码。

**事件来源**:
1. **Finnhub** (`finnhub_api_key`): 美国宏观经济日历（FOMC, CPI, NFP, GDP）
2. **CoinMarketCal** (`coinmarketcal_api_key`): 加密货币事件（Token Unlock, Fork, ETF）
3. **Static** (`static_events`): 手动配置事件（TOML 中定义，支持 4 类事件）

**事件分类**: `EventCategory` — Macro / Crypto / Political / Sports

**仓位乘数**: 基于 `EventImpact`:
- High → `high_impact_multiplier` (默认 0.25)
- Medium → `medium_impact_multiplier` (默认 0.50)
- Low → `low_impact_multiplier` (默认 0.75)
- 多事件重叠取最小值

**事件窗口**: `[event_time - pre_event_hours, event_time + post_event_hours]`

**关键词匹配** (`event_matches_market`):
1. 直接匹配: event.keywords 子串匹配 market question
2. 扩展匹配: event title 触发类别关键词映射（如 "fomc" → "interest rate, federal reserve, fed, monetary policy"）

**启用**: 在 `config/default.toml` 设置 `[event_calendar] enabled = true` + API keys

## 执行层

`HybridOrchestrator` 根据 `ExecutionPlan` 分发:
- `BuyAndMerge` → CLOB 买入 + CTF merge
- `SplitAndSell` → CTF split + CLOB 卖出
- `NegRiskConvert` → 多笔 CLOB 买入 + NegRiskAdapter merge
- `CrossMarket` → `tokio::join!` 并发执行两条腿
- `DirectionalBuy` → 单笔 CLOB FOK 买入（Weather 策略，无链上操作）

### Market Making 后台任务

被动做市赚取 bid-ask 价差。作为**后台任务**运行（非 Strategy trait — MM 需要持续管理订单，非一次性检测执行）。

**设计决策**:
- 独立 CLOB 连接: 避免与策略执行的请求竞争
- Cancel-then-replace: 每周期取消全部市场订单再重新报价
- Buy-only start: 只有持仓时才挂 ask 卖单（避免裸空头）
- Inventory skew: 持仓偏重时加宽该侧价差，鼓励再平衡

**流程** (每 `quote_refresh_secs`):
1. 取消该市场上一周期的 bid/ask 订单
2. 从 OrderBookCache 获取 midpoint
3. 计算 bid/ask = midpoint ± half_spread, 含 inventory skew
4. 挂 GTC buy limit（bid 价）
5. 如有持仓，挂 GTC sell limit（ask 价）
6. 跟踪 order_ids 用于下周期取消

**市场选择**: 非 NegRisk, outcome_prices 在 0.20-0.80 范围, 按接近 0.50 排序, 取前 N 个

**启用**: 在 `config/default.toml` 设置 `[market_making] enabled = true`

### Smart WS 订阅

基于 Gamma API `outcome_prices` 智能排序 WebSocket 订阅:
1. 策略相关市场（weather/crypto 关键词）: 最高优先
2. 过滤极端市场: YES price < 0.05 或 > 0.95
3. 按 "mid-ness" 排序: 越接近 0.50 优先级越高
4. NegRisk tokens 追加在二元市场之后
5. 截断至 `ws_max_instruments` 限制

## WebSocket 断线重连

`ws_feed.rs` 中的 subscribe 使用指数退避重连:
- 退避: 1s → 2s → 4s → ... → 60s max
- `Arc<AtomicBool>` ws_connected 状态标志
- 每次重连递增 `WS_RECONNECT_COUNT` metric

## 数据库（PostgreSQL）

7 张表: markets, tokens, orderbook_snapshots, opportunities, trades, positions, pnl_log

- 迁移: `migrations/001..005_*.sql` 通过 `sqlx::migrate!` 执行
- `Repository` 已 derive `Clone`（PgPool 内部 Arc）
- 快照录制: 后台任务每 60s 从 OrderBookCache 读取 → insert_orderbook_snapshot

## 回测系统

- **DataLoader**: DB → `Vec<SnapshotFrame>`（按时间分组）
- **BacktestEngine**: 回放每帧，更新共享 `Arc<RwLock<HashMap<U256, OrderBook>>>`，运行策略+风控+模拟执行
- **TradeSimulator**: 实现 `Executor` trait，模拟滑点+手续费+gas
- **BacktestResult**: PnL curve, Sharpe ratio, max drawdown, win rate, per-strategy breakdown
- **CLI**: `src/bin/backtest.rs`（clap），支持 --output text/json

## 监控（pa-monitor）

17 个 Prometheus 指标（`LazyLock` + 全局 `REGISTRY`）:
- Counters: opportunities_detected, opportunities_rejected, executions, execution_errors, ws_reconnect, snapshots_recorded, event_filter_applied, mm_orders_placed, mm_orders_cancelled
- Gauges: realized_pnl_usd, active_ws_subscriptions, monitored_markets, circuit_breaker_active, total_exposure_usd, mm_active_markets
- Histograms: execution_latency_seconds, scan_latency_seconds

HTTP 端点（Axum, health_port 18381）:
- `GET /health` → JSON 含 status + checks + uptime
- `GET /ready` → 200 或 503（K8s readiness probe）
- `GET /metrics` → Prometheus text format

HealthState 使用回调模式: `Vec<(&'static str, Box<dyn Fn() -> bool + Send + Sync>)>`

## Docker

```
docker/
├── Dockerfile              # 多阶段构建（依赖缓存层）
├── docker-compose.yml      # bot + postgres + prometheus + grafana
├── init.sql                # DDL
├── prometheus.yml          # scrape localhost:18381/metrics
└── grafana/
    ├── provisioning/       # 自动配置 datasource + dashboard provider
    └── dashboards/         # polyalpha-overview.json (11 面板)
```

Grafana 仪表盘: PnL, Exposure gauge, Circuit breaker, Market stats, Opportunity/Execution rates, Latency P50/P95, Scan latency heatmap, WS reconnections

## 编码规范

- 使用 `alloy`（非 ethers-rs）进行链上交互
- `DashMap` 用于并发订单簿缓存
- `broadcast channel` 用于事件分发
- 私钥: `PrivateKeySigner::from_str(hex).with_chain_id(Some(chain_id))`
- Provider: `ProviderBuilder::new().connect(url).await`
- 必须 `use pa_core::traits::MarketDataFeed` 才能在 MarketDataService 上调用 trait 方法
- `StrategyType` 需要 `Hash` derive（用作 HashMap key）
- Backtest 共享订单簿: `Arc<RwLock<HashMap<U256, OrderBook>>>`
- 新增 Prometheus 指标: 使用 `LazyLock` + `REGISTRY.register()` 模式

## 添加新策略 Checklist

1. `crates/pa-strategy/src/new_strategy.rs` — 实现 Strategy trait
2. `crates/pa-strategy/src/lib.rs` — `pub mod new_strategy;`
3. `crates/pa-core/src/types.rs` — StrategyType 枚举 + ExecutionPlan 枚举
4. `crates/pa-execution/src/orchestrator.rs` — ExecutionPlan match arm
5. `crates/pa-backtest/src/simulator.rs` — ExecutionPlan match arm
6. `src/main.rs` — 策略实例化 + 注册到 strategies vec
7. `crates/pa-backtest/src/engine.rs` — build_strategies() 中添加
8. 测试: 至少覆盖 detect 逻辑 + profitability 计算

## 测试分布（125 个）

| Crate | 数量 | 覆盖 |
|-------|------|------|
| pa-backtest | 11 | DataLoader 解析, Report 构建/统计, Simulator 执行模拟 |
| pa-strategy | 96 | ProfitCalculator(12), CrossMarket(4), Weather(45: binary parser, NegRisk outcome range parser(5), event title parser(2), date parser(6), precipitation unit(3), CDF models(7: normal, lognormal, weibull, dispatcher), forecast error sigma(2), probability model(3), position sizing(3), cache eviction, NegRisk NO-side, edge detection), Convergence(10: filters(4), detection(3), time decay, position sizing, neg_risk skip), CryptoAlpha(24: question parser(6), volatility, GBM probability(4), asset mapping, NegRisk event title parser(3), NegRisk outcome range parser(3), GBM range probability(2), binary group(4: asset from title, reach/dip questions, group type)), YesNo(内含于profitability) |
| pa-execution | 1 | Gas 估算 |
| pa-market-data | 17 | OrderBook 排序(1), EventCalendar(12: Finnhub/CoinMarketCal parsing, static loading, window tests(3), impact multipliers(3), overlapping events, keyword matching, no-match different category), GammaFeed(4: binary group basic, excludes neg_risk, no title, multiple groups) |

## Polygon 合约地址

| 合约 | 地址 |
|------|------|
| CTFExchange | `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E` |
| ConditionalTokens | `0x4D97DCd97eC945f40cF65F87097ACe5EA0476045` |
| NegRiskExchange | `0xC5d563A36AE78145C45a50134d48A1215220f80a` |
| USDC (PoS) | `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174` |

Chain ID: 137, ~2s blocks, ~$0.01 gas, ERC-1155 approval required for CTF ops.

## 文件索引

| 文件 | 职责 |
|------|------|
| `src/main.rs` | 入口: 配置→签名→DB→市场发现→Smart WS订阅→快照录制→执行层→策略引擎→做市任务→交易循环 |
| `src/bin/backtest.rs` | 回测CLI: clap参数→DB连接→BacktestEngine→Report输出 |
| `crates/pa-core/src/types.rs` | 所有领域类型定义 |
| `crates/pa-core/src/traits.rs` | 4 个核心 trait |
| `crates/pa-core/src/config.rs` | Settings 分层加载 |
| `crates/pa-core/src/error.rs` | Error 枚举 (10 变体) |
| `crates/pa-market-data/src/service.rs` | MarketDataService (Gamma + WS + Cache 组合) |
| `crates/pa-market-data/src/ws_feed.rs` | WebSocket 订单簿流 + 断线重连 |
| `crates/pa-market-data/src/cache.rs` | DashMap OrderBookCache |
| `crates/pa-market-data/src/gamma_feed.rs` | Gamma API 市场发现 + NegRisk 分组 |
| `crates/pa-market-data/src/event_calendar.rs` | EventCalendarService: Finnhub/CoinMarketCal/Static providers + 关键词匹配 + 仓位乘数 |
| `crates/pa-strategy/src/engine.rs` | StrategyEngine 事件驱动+定时扫描双模 |
| `crates/pa-strategy/src/yes_no.rs` | YesNo 二元市场套利 |
| `crates/pa-strategy/src/neg_risk.rs` | NegRisk 多结果套利 |
| `crates/pa-strategy/src/cross_market.rs` | 跨市场套利 + detect_cross_market_pairs() |
| `crates/pa-strategy/src/weather.rs` | Weather Alpha: 问题解析 + Open-Meteo客户端 + 概率模型 + Strategy impl |
| `crates/pa-strategy/src/convergence.rs` | Resolution Convergence: 到期收敛策略 + detect_convergence() + Strategy impl |
| `crates/pa-strategy/src/crypto_alpha.rs` | Crypto Alpha: 资产映射 + 问题解析 + Binance/CoinGecko客户端 + GBM模型 + Strategy impl |
| `crates/pa-strategy/src/profitability.rs` | ProfitCalculator (4种策略利润计算，含directional_buy) |
| `crates/pa-execution/src/orchestrator.rs` | HybridOrchestrator (CLOB + CTF 路由) |
| `crates/pa-execution/src/clob_executor.rs` | CLOB API FOK 下单 |
| `crates/pa-execution/src/ctf_executor.rs` | 链上 CTF split/merge/NegRisk |
| `crates/pa-risk/src/manager.rs` | RiskManagerImpl (线程安全) |
| `crates/pa-storage/src/repository.rs` | PostgreSQL CRUD (Clone, sqlx) |
| `crates/pa-storage/src/models.rs` | DB row 模型 |
| `crates/pa-backtest/src/engine.rs` | BacktestEngine 回放循环 |
| `crates/pa-backtest/src/simulator.rs` | TradeSimulator (滑点+手续费模拟) |
| `crates/pa-backtest/src/report.rs` | BacktestResult + Display |
| `crates/pa-backtest/src/data_loader.rs` | DB → SnapshotFrame 加载 |
| `crates/pa-monitor/src/metrics.rs` | 17 个 Prometheus 指标 |
| `crates/pa-monitor/src/health.rs` | Health/Ready/Metrics HTTP 服务 |
| `config/default.toml` | 默认配置 |
| `docker/docker-compose.yml` | 全栈 Docker 部署 |
