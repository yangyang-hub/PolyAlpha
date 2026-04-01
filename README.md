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
9. **止损安全网** — 扫描所有持仓，跌破相对止损阈值时强制退出
10. **自动赎回** — 已解决市场的 winning tokens 通过 GnosisSafe 自动赎回
11. **仓位同步** — 每 5 分钟与 Data API 对账，处理外部变化
12. **监控** — 22 个 Prometheus 指标 + Grafana 18 面板仪表盘

## 方向性策略

通过外部数据源构建概率模型，当模型概率与市场价格存在显著偏差（edge）时买入。

### 手续费模型

Polymarket 采用封顶手续费模型：`fee = min(fee_rate × price, price × (1 - price))`

### 1. Weather Alpha（天气 Alpha）

利用 provider-aware 天气数据源为 Polymarket 上的天气相关市场定价：
- 美国可交易城市走 NOAA
- 伦敦审计路径走 Open-Meteo
- 首尔审计路径已升级为 KMA

**二元市场模式** — 单一阈值问题（如 "温度会超过 100°F 吗？"）：
- 关键词匹配识别天气市场（temperature, rainfall, snowfall, wind）
- 解析目标日期 → 获取城市对应天气源预报 → 分布 CDF 概率模型
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

1. **触发条件**: best_bid < avg_cost × relative_stop_loss_ratio
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
│   ├── bin/crypto_calibrate.rs      # 加密校准 CLI：JSONL 样本 → calibration_overrides 草案
│   ├── bin/crypto_export_diagnostics.rs # 导出 monitor 里的 crypto 决策/退出 JSONL
│   ├── bin/crypto_prepare_calibration.rs # 诊断 JSONL + 结果标签 → crypto_calibrate 样本
│   ├── bin/crypto_seed_labels.rs    # 从 diagnostics 生成待填结果标签 skeleton
│   ├── bin/crypto_autolabel_resolved.rs # 按 condition_id 自动回填已结算标签
│   └── bin/crypto_pipeline_report.rs # 汇总 seed/autolabel/prepare 三步 summary
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

### 加密校准草案生成

当你已经有历史加密市场样本，并且每条样本包含 `modeled_prob` 与实际结果时，可以直接生成
`crypto_alpha.calibration_overrides` 草案：

```bash
cargo run --bin crypto_calibrate -- \
  --input tmp/crypto_samples.jsonl \
  --min-samples 20 \
  --short-horizon-max-days 1 \
  --medium-horizon-max-days 7 \
  --group-by-asset-class \
  --group-by-event-subtype \
  --override-output tmp/crypto_calibration_overrides.toml
```

输入为 JSONL，每行至少需要：

- `modeled_prob`
- `resolved_yes` 或 `resolved_value`
- 以及一组可推断分段的字段：
  - `asset + market_type + days_to_resolution`
  - 可选 `asset_class + event_subtype`
  - 或 `question + days_to_resolution`
  - 或 `question + observed_at + resolution_at`

生成结果会直接输出带注释的 `[[crypto_alpha.calibration_overrides]]` TOML 片段。
如果传了 `--override-output`，同样的内容也会直接写成一份 merge-ready TOML 文件，
更适合后续 review 和合并到运行时配置。

如果你已经有一份现有 override 配置，也可以让它按 selector 精确合并：

```bash
cargo run --bin crypto_calibrate -- \
  --input tmp/crypto_samples.jsonl \
  --group-by-asset-class \
  --group-by-event-subtype \
  --existing-overrides-input config/default.toml \
  --merge-mode probability-only \
  --override-output tmp/crypto_calibration_merged.toml
```

`--merge-mode` 支持三档：
- `probability-only`：只更新命中行的 `probability_calibration`，其余字段保留不动
- `replace-row`：命中行整条替换成 calibrate 生成的最小行
- `append-only`：不覆盖现有行，直接追加新建议行

默认仍然是最保守的 `probability-only`，所以不会改动现有的
`sigma_multiplier / size_multiplier / min_edge_multiplier / max_spread_multiplier`。
merge 输出文件头部现在还会附一段 diff summary，直接列出：
- `new_rows`
- `updated_rows`
- `unchanged_rows`

这样 review 时不需要手动对比整份 TOML，先看摘要就知道这次校准到底改了哪些 selector。
如果同时传了 `--summary-output`，同样的 merge diff 也会进入
`crypto_calibrate_summary.json` 的 `merge_diff_summary` 字段，方便后续 report 或自动化工具直接消费。
更完整说明见 [docs/crypto-calibration-workflow.md](docs/crypto-calibration-workflow.md)。

如果你想先把运行中的 crypto 决策/退出诊断导出来做人工回看或离线研究，也可以先导出 monitor API 的最近缓冲：

```bash
cargo run --bin crypto_export_diagnostics -- \
  --base-url http://127.0.0.1:8080 \
  --output tmp/crypto_diagnostics.jsonl
```

输出是按 `recorded_at` 排序的 JSONL，包含两类记录：
- `candidate_decision`
- `exit_decision`

这份文件更适合做运行时诊断留档；如果后面要生成 `calibration_overrides`，通常还需要再补上最终结算标签或研究侧加工。

如果你已经另外准备好了按 `question` 对齐的结果标签，也可以把导出的 candidate diagnostics 进一步整理成 `crypto_calibrate` 可直接消费的样本：

如果你还没有标签文件，可以先从 diagnostics 里生成一份待填写 skeleton：

```bash
cargo run --bin crypto_seed_labels -- \
  --diagnostics tmp/crypto_diagnostics.jsonl \
  --output tmp/crypto_labels.jsonl \
  --summary-output tmp/crypto_seed_summary.json
```

生成结果会去重到 `question` 粒度，并预填：
- `question`
- `asset`
- `asset_class`
- `market_type`
- `event_subtype`

如果传了 `--summary-output`，还会写一份 JSON summary，包含：
- `question_count`
- `replace_only`
- `by_asset`
- `by_asset_class`
- `by_market_type`
- `by_event_subtype`

你只需要继续补：
- `resolved_yes` 或 `resolved_value`
- 可选 `resolution_at`

如果其中一部分市场已经结算，也可以先自动回填：

```bash
cargo run --bin crypto_autolabel_resolved -- \
  --labels tmp/crypto_labels.jsonl \
  --output tmp/crypto_labels_filled.jsonl \
  --unresolved-output tmp/crypto_labels_unresolved.jsonl \
  --summary-output tmp/crypto_autolabel_summary.json
```

这个工具会按 `condition_id` 查询 CLOB 单市场详情；如果市场已关闭且 winner 明确，就自动补上：
- `resolved_yes`
- `resolution_at`

如果市场还没结算、winner 不明确或请求失败，原因会进入 `tmp/crypto_labels_unresolved.jsonl`，同时命令本身会打印一条原因汇总；如果传了 `--summary-output`，同一份统计也会落成 JSON。

```bash
cargo run --bin crypto_prepare_calibration -- \
  --diagnostics tmp/crypto_diagnostics.jsonl \
  --labels tmp/crypto_labels_filled.jsonl \
  --output tmp/crypto_samples.jsonl \
  --summary-output tmp/crypto_prepare_summary.json
```

其中 `tmp/crypto_labels.jsonl` 每行至少需要：
- `question`
- `resolved_yes` 或 `resolved_value`
- 可选 `resolution_at`

命令结束时还会打印一条样本汇总，包含：
- `total_candidates`
- `matched_labels`
- `emitted_samples`
- `missing_labels`
- `invalid_labels`
- `by_asset`
- `by_market_type`

如果传了 `--summary-output`，同一份汇总也会落成 JSON，方便 notebook 或后续前端读取。

接着可以直接生成 calibration override 草案和 calibrate summary：

```bash
cargo run --bin crypto_calibrate -- \
  --input tmp/crypto_samples.jsonl \
  --min-samples 20 \
  --short-horizon-max-days 1 \
  --medium-horizon-max-days 7 \
  --group-by-asset-class \
  --group-by-event-subtype \
  --summary-output tmp/crypto_calibrate_summary.json
```

除了 TOML-ready `[[crypto_alpha.calibration_overrides]]` 片段之外，如果传了
`--summary-output`，还会写一份 JSON summary，包含：
- `input_rows`
- `emitted_segment_count`
- `skipped_segment_count`
- emitted / skipped segments
- `underfilled_buckets`，按 `asset_class × horizon × event_subtype` 聚合 underfilled 桶
- `gap_to_min_samples`，直接表示每个 underfilled bucket 还差多少样本才能补到 `min_samples`
- `threshold_band`，把 underfilled bucket 分成 `near-threshold` 或 `far-from-threshold`
- `merge_diff_summary`，在 merge 模式下记录 `new/updated/unchanged` override rows 的计数和 selector 列表

离线 pipeline report 会进一步把最接近可校准的 3 个 `near-threshold`
buckets 提到 headline，并附上 `top-up-now` / `ready-soon` / `defer`
这类建议动作，还会汇总这 3 个桶的 action counts；如果存在
`top-up-now` 桶，静态 viewer 的 headline 状态也会直接变成更醒目的提醒；
如果 `top-up-now >= 2`，headline 和 near-threshold 状态都会升成 `Urgent`。
viewer 现在还会在 headline 下直接补一句解释文案，说明为什么当前批次被标成紧急。
同样的解释现在也会出现在 `crypto_pipeline_report` 生成的 markdown/JSON 里。
静态 viewer 的 hero 顶部状态区域也会同步显示这条解释，不用滚动到 headline 区才看得到。
这条 hero explain line 还会按当前状态联动样式，`Urgent/Action Needed` 用提醒色，`Complete` 用成功色。
现在这条顶部说明里的 `top-up-now` 数量会单独渲染成一个 pill，不再只是文本前缀。
headline 里的 action counts 也会复用同一套 action chip 样式，和 hero 顶部保持一致。
其中 `ready-soon` 现在也有单独的次提醒色，和 `top-up-now` / `defer` 更容易区分。
near-threshold 表里的 `Action` 列也会用同一套 chip，而不是再显示纯文本。
calibrate breakdown 里的 `Skip Reason` 现在也会用轻量 chip，`insufficient_samples` 会单独高亮。
calibrate breakdown 现在还会显示 merge diff 摘要，直接列出本次校准准备新增、更新或保持不变的 override rows。
top underfilled bucket 里的 `threshold_band` 也会拆成独立 chip，不再埋在 `Need` 文本里。
现在 `gap_to_min_samples` 也会显示成独立的 `gap N` badge，和 `threshold_band` 并排展示。
headline 区里每个 near-threshold bucket 的 `gap/action` 也会用同一套 badge + chip，而不是纯文本。

如果你想把这三步 summary 合并成一份可读报告，也可以直接生成 markdown：

```bash
cargo run --bin crypto_pipeline_report -- \
  --input-dir tmp \
  --output-dir tmp/report \
  --title "March 2026 Crypto Calibration Batch" \
  --subtitle "BTC/ETH replace-only run" \
  --notes-file tmp/crypto_pipeline_notes.txt \
  --tag replace-only \
  --tag majors \
```

不传 `--output` 时会直接打印到 stdout。

如果你更想直接在浏览器里看离线汇总，也可以打开
[`docs/crypto-pipeline-report.html`](docs/crypto-pipeline-report.html)，然后手动加载：

- `tmp/crypto_pipeline_report.html`
- `tmp/crypto_pipeline_report.json`
- 或者：
- `tmp/crypto_seed_summary.json`
- `tmp/crypto_autolabel_summary.json`
- `tmp/crypto_prepare_summary.json`
- `tmp/crypto_calibrate_summary.json`

`crypto_pipeline_report --json-output` 生成的 aggregate JSON 现在还会带一个
`ui_priority_summary`，把 headline / near-threshold 状态、hero badge 文案和 explainer
直接算好；同时还会带结构化的 `priority_source`、`headline_status_reason`、
`top_up_now_labels` 和 `near_threshold_bucket_labels`。
静态 viewer 读到它时会优先使用这份摘要，而不是再在客户端重复推导。
viewer 现在还会把这些标签直接渲染成一行紧凑的 `Triggered by` trigger chips，不再只藏在 tooltip 里；超过 3 个桶时会压成 `+N more`。
markdown 版 `crypto_pipeline_report` 现在也会把 `ui_priority_summary` 写出来，和静态页/aggregate JSON 保持同一套优先级语义。

这个页面完全在本地渲染，不依赖 monitor API 或前端服务。
导出的 markdown / JSON / HTML 也会自动带上 `generated_at_utc`，并支持通过 `--notes` / `--notes-file` / repeatable `--tag` 嵌入批次上下文。
如果传了 `--input-dir tmp`，会自动尝试读取其中的 `crypto_seed_summary.json`、`crypto_autolabel_summary.json`、`crypto_prepare_summary.json`、`crypto_calibrate_summary.json`。
如果同时传了 `--output-dir tmp/report`，会默认生成 `crypto_pipeline_report.md`、`crypto_pipeline_report.json`、`crypto_pipeline_report.html`。
如果四份 summary 全部缺失，`crypto_pipeline_report` 现在会直接报错，而不是静默生成空报告。

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
max_slippage_bps = 50
min_profit_retention_ratio = 0.50
min_size_retention_ratio = 0.50
execution_quality_profit_weight = 1.0
execution_quality_size_weight = 1.0
execution_quality_slippage_weight = 1.0
max_exposure_per_strategy = 5000.0
max_markets_per_strategy = 50

[monitor]
health_port = 18381

[market_filter]
ws_max_instruments = 350
market_refresh_interval_secs = 1800

[weather]
min_edge_bps = 450
max_spread_bps = 1700
max_position_usdc = 4.0
kelly_fraction = 0.25
dynamic_sigma = true
forecast_change_detection = false
forecast_change_threshold = 0.35
max_entry_price = 0.38
exit_buffer_bps = 50
capital_efficiency_threshold = 0.98
kma_api_key = ""
met_office_api_key = ""
met_office_obs_api_key = ""
target_cities = ["Atlanta", "Miami", "New York", "Dallas", "Seattle"]

[weather.forecast_error]
temperature_sigma_f = 3.0
precipitation_sigma_in = 0.3
snowfall_sigma_in = 2.0
wind_sigma_mph = 5.0

# Settlement-aware sigma:
# The weather strategy widens sigma by city risk tier to reflect
# Polymarket settlement sources (often airport/Wunderground observation)
# not exactly matching NOAA grid forecasts.
# Verified cities currently use Medium risk; unverified NOAA cities use High risk.

[convergence]
min_price_threshold = 0.93
max_days_to_resolution = 7
max_position_usdc = 100.0
kelly_fraction = 0.25
exit_buffer_bps = 50
capital_efficiency_threshold = 0.98

[smart_money]
wallets = []
blocked_wallets = []
degraded_wallets = []
leader_routes = []
follow_ratio = 0.10
max_position_usdc = 100.0
poll_interval_secs = 30
signal_ttl_secs = 300
exit_buffer_bps = 50
capital_efficiency_threshold = 0.98
onchain_enabled = false
onchain_poll_secs = 4
auto_discover_enabled = false
auto_discover_candidates = []
auto_discover_interval_secs = 3600
min_wallet_score = 0.05
min_wallet_volume_usdc = 250.0
max_wallets = 20
wallet_profile_blend = 0.50
wallet_signal_bonus_per_event = 0.05
wallet_signal_bonus_cap = 0.25
wallet_underperform_decay_step = 0.10
wallet_min_effective_weight = 0.25
wallet_max_effective_weight = 1.50
wallet_signal_lookback_secs = 86400
min_signal_notional_usdc = 25.0
min_signal_delta_shares = 20.0
min_wallet_weight = 0.5
min_consensus_wallets = 1
max_signal_age_secs = 90
max_entry_price = 0.85
max_spread_bps = 300
min_top_level_depth_usdc = 50.0
min_market_liquidity = 500.0
confirm_onchain_with_data_api = true
dedup_window_secs = 45
consensus_bonus_per_wallet = 0.10
consensus_bonus_cap = 0.30
freshness_half_life_secs = 45
leader_delta_ratio_floor = 0.20
position_concentration_soft_cap_usdc = 60.0
position_concentration_min_multiplier = 0.25
leader_exit_min_delta_ratio = 0.25
max_hold_secs = 21600
profit_protect_min_gain_bps = 800
profit_protect_drawdown_bps = 500
max_drawdown_bps = 1200

Use `smart_money_discover_leaders` to build `auto_discover_candidates` or `[[smart_money.wallets]]`
from public Polymarket leaderboard plus active-market holder/position data:

```bash
cargo run --bin smart_money_discover_leaders -- \
  --leaderboard-limit 200 \
  --market-limit 50 \
  --candidate-limit 100 \
  --summary-output smart_money_discovery_summary.json \
  --emit-auto-discover-candidates smart_money_candidates.toml \
  --emit-wallets-toml smart_money_wallets.toml
```

If `PA_DATABASE__URL` or `--database-url` is set, discovered candidates are also upserted into the
`smart_money_leader_candidates` table for later review. Once populated, the monitor exposes:

- `/api/smart-money/leaders` for the full recent candidate list
- `/api/status.smart_money_leader_discovery_summary` for a compact top-candidate snapshot

The SmartMoney page renders this candidate pool directly so you can inspect discovery score, source
mix, leaderboard rank, realized PnL, and recent chain activity without leaving the monitor UI.
The page also supports promoting a candidate, degrading it with a runtime multiplier, or blocking
it entirely inside the repository. Promotion still returns ready-to-copy `[[smart_money.wallets]]`
/ `auto_discover_candidates` snippets, while block/degrade actions update
`smart_money.blocked_wallets` / `smart_money.degraded_wallets`. These changes are persisted into
`app_config` / `config_history`, and running smart-money workers consume the shared `smart_money`
config on later scan/poll cycles for wallet lists, thresholds, and tracker scheduling intervals, so
promoted leaders, blocked wallets, degraded multipliers, and updated polling settings become
actionable without a process restart. The same UI can also restore a leader by clearing any block
or degrade override.
If you need leader-by-theme routing, `leader_routes` can restrict a wallet to specific
`market.category`, question keywords, or parent-event-title keywords; entry-like smart-money
signals that miss their configured route are recorded as `route_mismatch` and skipped without
affecting exit handling.
The SmartMoney page also surfaces each candidate's current route constraints plus a compact route
summary so you can see whether route mismatches are becoming a meaningful source of rejected
signals. Common route templates (`crypto`, `politics`, `sports`, `weather`, `all`) can now be
applied directly from the candidate table without editing `leader_routes` JSON by hand.
The SmartMoney page also includes an estimated leader PnL attribution table. This ledger is based
on accepted smart-money entry/exit opportunities and proportional leader exposure weights, so it is
useful for ranking leaders and debugging copy-trade quality, but it is not yet a fill-confirmed
execution ledger.
Those same estimated leader slices are also persisted into smart-money `opportunities.details` and
flow through `/api/trades` plus `/api/crypto/trades` as `smart_money_attribution`, so historical
trade views can be joined back to the leader mix that produced a copied opportunity. Newer trade
rows also carry `smart_money_trade_attribution`, which scales that leader mix onto each persisted
fill using the realized `filled_size`, fee, and sell-side realized profit recorded for the trade.
The SmartMoney page now shows both layers side-by-side: a fill-confirmed leader PnL table derived
from persisted trade attribution, and the older opportunity-level estimated ledger that is still
useful when a copied opportunity was generated but only partially filled or not yet exited.
On top of those ledgers, the status API now derives a compact leader-health table that combines
accept rate, estimated leader PnL, and fill-confirmed realized PnL into simple `keep_or_promote`,
`observe`, `degrade`, or `block_candidate` suggestions for operator review.
The monitor also derives a pending smart-money review queue from those suggestions plus the current
candidate/config state, so only leaders that still need an operator action are highlighted; the
SmartMoney page can execute the suggested promote/degrade/block/restore action directly from that
queue without manually cross-referencing the candidate table.
Recent smart-money operator changes are also exposed through `/api/smart-money/audit`, which reads
the smart-money section history from `config_history` and renders a lightweight audit table on the
SmartMoney page so you can see when web actions changed wallet counts, degraded/blocked sets, or
route coverage.
The SmartMoney page also includes a leader signal-attribution table derived from recent smart-money
decision records, so you can see which leaders are mostly producing accepted versus rejected follow
signals without trying to overstate exact realized-PnL attribution.

To supplement leaderboard/API discovery with recent Conditional Tokens transfer activity on Polygon,
add a recent-block scan:

```bash
cargo run --bin smart_money_discover_leaders -- \
  --leaderboard-limit 200 \
  --market-limit 50 \
  --candidate-limit 100 \
  --onchain-lookback-blocks 5000 \
  --onchain-max-logs 5000 \
  --rpc-url https://polygon-rpc.com \
  --summary-output smart_money_discovery_summary.json
```

This first chain supplement is a recent-block address seed, not a full historical chain backfill.
It helps catch active wallets that are currently moving size on Conditional Tokens but have not yet
surfaced strongly in the public leaderboard slice.

[crypto_alpha]
min_edge_bps = 100
max_position_pct = 0.50
kelly_fraction = 0.25
refresh_interval_secs = 300
spot_refresh_interval_secs = 30
history_refresh_interval_secs = 1800
iv_refresh_interval_secs = 300
coingecko_api_key = ""
min_entry_depth_ratio = 1.25       # 入场盘口可成交深度至少为目标下单量的 1.25x
gate_scale_feedback_lookback = 24    # 最近 24 条 gate_scale 用于自适应预缩量反馈
gate_scale_feedback_trigger_count = 3
gate_scale_feedback_step_multiplier = 0.90
gate_scale_feedback_max_steps = 2
discovery_search_terms = []          # 额外 Gamma 搜索词，在共享默认 crypto 词表基础上追加
exit_buffer_bps = 50
capital_efficiency_threshold = 0.98
drift_decay = 0.0
max_spread_bps = 1500
relative_stop_loss_ratio = 0.85
max_exposure_per_asset_pct = 0.75
max_exposure_per_asset_direction_pct = 0.45
low_event_min_edge_multiplier = 1.20
medium_event_min_edge_multiplier = 1.50
high_event_min_edge_multiplier = 2.00
low_event_max_spread_multiplier = 0.90
medium_event_max_spread_multiplier = 0.80
high_event_max_spread_multiplier = 0.65
low_event_sigma_multiplier = 1.05
medium_event_sigma_multiplier = 1.15
high_event_sigma_multiplier = 1.30
macro_event_sigma_multiplier = 1.10
crypto_event_sigma_multiplier = 1.20
low_event_size_multiplier = 0.90
medium_event_size_multiplier = 0.75
high_event_size_multiplier = 0.50
macro_event_size_multiplier = 0.85
crypto_event_size_multiplier = 0.75
btc_probability_calibration = 0.95
eth_probability_calibration = 0.93
alt_probability_calibration = 0.88
binary_probability_calibration = 0.97
range_probability_calibration = 0.90
override_probability_blend = 0.50
override_probability_max_delta_bps = 1000
override_multiplier_blend = 1.00
override_multiplier_max_delta_bps = 2500
short_horizon_max_days = 1
medium_horizon_max_days = 7
max_entry_days = 1
same_day_alt_probability_multiplier = 0.95
same_day_range_bad_exit_cooldown_trigger_count = 2
same_day_range_bad_exit_cooldown_secs = 1800
same_day_alt_bad_exit_cooldown_trigger_count = 2
same_day_alt_bad_exit_cooldown_secs = 1800
next_day_alt_range_bad_exit_cooldown_trigger_count = 2
next_day_alt_range_bad_exit_cooldown_secs = 3600
auto_apply_cooldown_priority_patch = true
auto_apply_cooldown_priority_patch_interval_secs = 300
auto_apply_cooldown_priority_patch_tighten_only = true
auto_apply_cooldown_priority_patch_max_rows = 4
auto_apply_cooldown_priority_patch_min_reapply_secs = 1800
same_day_probability_calibration = 0.80
same_day_range_probability_multiplier = 0.90
same_day_major_range_probability_multiplier = 0.95
short_horizon_probability_calibration = 0.85
medium_horizon_probability_calibration = 0.92
same_day_execution_quality_profit_weight_multiplier = 0.70
same_day_alt_execution_quality_profit_weight_multiplier = 0.90
same_day_range_execution_quality_profit_weight_multiplier = 0.85
same_day_execution_quality_size_weight_multiplier = 1.30
same_day_alt_execution_quality_size_weight_multiplier = 1.10
same_day_range_execution_quality_size_weight_multiplier = 1.10
same_day_execution_quality_slippage_weight_multiplier = 1.50
same_day_alt_execution_quality_slippage_weight_multiplier = 1.15
same_day_range_execution_quality_slippage_weight_multiplier = 1.20
short_execution_quality_profit_weight_multiplier = 1.15
short_execution_quality_size_weight_multiplier = 0.95
short_execution_quality_slippage_weight_multiplier = 0.90
same_day_size_multiplier = 0.35
same_day_alt_size_multiplier = 0.80
same_day_range_size_multiplier = 0.75
same_day_major_range_size_multiplier = 0.90
same_day_eth_range_size_multiplier = 0.85
short_horizon_size_multiplier = 0.60
medium_horizon_size_multiplier = 0.80
same_day_min_edge_multiplier = 1.90
same_day_alt_min_edge_multiplier = 1.10
same_day_range_min_edge_multiplier = 1.15
same_day_major_range_min_edge_multiplier = 1.10
short_horizon_min_edge_multiplier = 1.50
same_day_max_spread_multiplier = 0.55
same_day_alt_max_spread_multiplier = 0.85
same_day_major_generic_max_spread_multiplier = 1.08
same_day_alt_generic_max_spread_multiplier = 1.15
same_day_major_generic_range_max_spread_multiplier = 1.08
same_day_alt_generic_range_max_spread_multiplier = 1.15
same_day_alt_range_max_spread_multiplier = 1.10
same_day_range_max_spread_multiplier = 0.85
same_day_major_range_max_spread_multiplier = 0.90
next_day_alt_range_max_spread_multiplier = 1.10
same_day_alt_capital_efficiency_multiplier = 0.98
same_day_major_range_capital_efficiency_multiplier = 0.97
same_day_eth_range_capital_efficiency_multiplier = 0.93
same_day_eth_range_exit_buffer_multiplier = 1.05
same_day_exit_buffer_multiplier = 0.30
same_day_alt_exit_buffer_multiplier = 0.90
same_day_range_exit_buffer_multiplier = 0.85
same_day_hold_edge_multiplier = 1.90
same_day_alt_hold_edge_multiplier = 1.10
same_day_range_hold_edge_multiplier = 1.10
same_day_major_range_hold_edge_multiplier = 1.10
same_day_eth_range_hold_edge_multiplier = 1.15

### Smart Money Replay CLI

The discovery CLI above is the fastest way to bootstrap a wider smart-money candidate set from
official API-visible wallets, with an optional recent-block chain supplement. Full historical
chain backfill and entity clustering are still a separate follow-up problem.

If your raw JSONL omits optional replay fields such as `source`, `fee_rate_bps`, `liquidity`,
or top-of-book sizes, normalize it first:

```bash
cargo run --bin smart_money_prepare_replay -- \
  --input raw_smart_money_samples.jsonl \
  --output smart_money_samples.jsonl \
  --summary-output smart_money_prepare_summary.json
```

Use `smart_money_replay` to replay newline-delimited JSON market snapshots through the current
smart-money strategy logic, including entry gates, dynamic sizing, and multi-route exits.

```bash
cargo run --bin smart_money_replay -- \
  --input smart_money_samples.jsonl \
  --initial-balance 1000 \
  --output json \
  --summary-output smart_money_replay_summary.json \
  --trace-output smart_money_replay_trace.json
```

Each JSONL row should provide a market snapshot and may optionally include one smart-money signal:

```json
{
  "timestamp": "2026-03-25T09:30:00Z",
  "token_id": "42",
  "condition_id": "0x0000000000000000000000000000000000000000000000000000000000000042",
  "question": "Will BTC finish above $110k today?",
  "signal_type": "entry",
  "wallet_address": "0xabc...",
  "wallet_label": "leader_1",
  "wallet_weight": "1.0",
  "wallet_size": "500",
  "delta": "500",
  "source": "data_api",
  "best_bid": "0.59",
  "best_bid_size": "500",
  "best_ask": "0.60",
  "best_ask_size": "500",
  "fee_rate_bps": 200,
  "liquidity": "1500"
}
```

Rows without `signal_type` still update the order book and let the replay trigger strategy-managed exits
such as `stale_follow`, `profit_protect`, `drawdown`, and `capital_efficiency`.

`--summary-output` writes aggregate replay stats plus recent decision/exit snippets, while
`--trace-output` writes a per-row execution trace that includes generated opportunities, simulated
buys/sells, ending cash, realized PnL, and open-position count after each snapshot.
medium_horizon_min_edge_multiplier = 1.20
same_day_max_spread_multiplier = 0.55
short_horizon_max_spread_multiplier = 0.75
next_day_alt_range_max_spread_multiplier = 1.10
medium_horizon_max_spread_multiplier = 0.90
same_day_capital_efficiency_threshold = 0.90
short_horizon_capital_efficiency_threshold = 0.92
medium_horizon_capital_efficiency_threshold = 0.95
same_day_exit_buffer_multiplier = 0.30
short_horizon_exit_buffer_multiplier = 0.50
medium_horizon_exit_buffer_multiplier = 0.80
hold_min_edge_bps = 100
same_day_hold_edge_multiplier = 1.90
short_horizon_hold_edge_multiplier = 1.50
medium_horizon_hold_edge_multiplier = 1.20
edge_decay_exit_fraction = 0.25
edge_decay_exit_fraction_step = 0.10
edge_decay_moderate_gap_bps = 50
edge_decay_severe_gap_bps = 150
edge_decay_moderate_exit_multiplier = 1.25
edge_decay_severe_exit_multiplier = 1.50
edge_decay_moderate_cooldown_multiplier = 0.75
edge_decay_severe_cooldown_multiplier = 0.50
same_day_edge_decay_exit_multiplier = 1.80
short_horizon_edge_decay_exit_multiplier = 1.50
same_day_edge_decay_confirmation_scans = 1
same_day_edge_decay_confirmation_window_multiplier = 0.40
same_day_edge_decay_cooldown_multiplier = 0.40

其中 `override_probability_blend` 和 `override_probability_max_delta_bps`
是 runtime 护栏：命中 `calibration_overrides.probability_calibration` 时，系统会先把
override 因子和默认 baseline 做混合，再限制它相对 baseline 的最大偏移，避免新校准
一上线就把概率收缩拉得过头。

当 `crypto_alpha.max_entry_days = 1` 时，crypto 策略现在会进一步把短盘拆成两档：
- `same_day = 0`：当天结算
- `short = 1`：次日结算

也就是说，当前默认已经不是把所有 `<= 1d` 市场视为同一档，而是会对当天盘使用更保守的
entry / sizing / hold / edge-decay 参数。
`override_multiplier_blend` 和 `override_multiplier_max_delta_bps` 则对其他
multiplier 类 override 做同样的运行时护栏，统一限制 sigma/size/entry/exit/execution
乘数相对 `1.0` 的偏移幅度。
`max_entry_days` 则是 crypto 新开仓的硬过滤窗口：超过这个到期天数的市场不再生成新机会，
但已持有仓位仍然会继续进入退出扫描。
`gate_scale_feedback_*` 则是 runtime sizing 反馈护栏：当某个 `asset_class × event_subtype`
桶最近持续出现 `gate_scale`（也就是候选能做，但总被 depth/retention 预缩量）时，
策略会先把目标下单量轻微压低，减少“先生成、再裁掉”的重复摩擦。
`risk.execution_quality_*_weight` 则控制执行质量分数里 `profit retention / size retention /
slippage quality` 三项的相对重要性；默认三者等权，此时排序语义与原先保持一致。
`same_day_execution_quality_*_multiplier` 和 `short_execution_quality_*_multiplier`
则分别让当天盘和次日盘在这三项之间做一次桶内重加权。
max_slippage_bps = 50
min_profit_retention_ratio = 0.50
min_size_retention_ratio = 0.50
execution_quality_profit_weight = 1.0
execution_quality_size_weight = 1.0
execution_quality_slippage_weight = 1.0
medium_horizon_edge_decay_exit_multiplier = 1.20
edge_decay_cooldown_secs = 1800
edge_decay_confirmation_scans = 2
short_horizon_edge_decay_confirmation_scans = 1
medium_horizon_edge_decay_confirmation_scans = 2
edge_decay_moderate_confirmation_scan_multiplier = 0.75
edge_decay_severe_confirmation_scan_multiplier = 0.50
edge_decay_confirmation_window_secs = 900
short_horizon_edge_decay_confirmation_window_multiplier = 0.50
medium_horizon_edge_decay_confirmation_window_multiplier = 0.75
edge_decay_moderate_confirmation_window_multiplier = 0.75
edge_decay_severe_confirmation_window_multiplier = 0.50
short_horizon_edge_decay_cooldown_multiplier = 0.50
medium_horizon_edge_decay_cooldown_multiplier = 0.75

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "major"
horizon = "any"
resolution_bucket = "same_day"
market_type = "any"
event_subtype = "unlock"
sigma_multiplier = 1.10
size_multiplier = 0.85
depth_ratio_multiplier = 1.10
min_edge_multiplier = 1.08
max_spread_multiplier = 0.95
hold_edge_multiplier = 1.10
edge_decay_exit_multiplier = 1.15
edge_decay_confirmation_scan_multiplier = 0.90
edge_decay_confirmation_window_multiplier = 0.90
edge_decay_cooldown_multiplier = 0.90
capital_efficiency_multiplier = 0.98
model_reversal_buffer_multiplier = 0.90
profit_retention_multiplier = 1.10
slippage_multiplier = 0.90
size_retention_multiplier = 1.08

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "major"
horizon = "short"
market_type = "binary"
event_subtype = "unlock"
sigma_multiplier = 1.04
size_multiplier = 0.92
depth_ratio_multiplier = 1.20
min_edge_multiplier = 1.02
max_spread_multiplier = 0.99
profit_retention_multiplier = 1.15
slippage_multiplier = 0.85
size_retention_multiplier = 1.15

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "major"
horizon = "any"
market_type = "any"
event_subtype = "upgrade"
sigma_multiplier = 1.05
size_multiplier = 0.90
depth_ratio_multiplier = 1.02
min_edge_multiplier = 1.04
max_spread_multiplier = 0.98
hold_edge_multiplier = 1.03
edge_decay_exit_multiplier = 1.08
edge_decay_confirmation_scan_multiplier = 0.97
edge_decay_confirmation_window_multiplier = 0.95
edge_decay_cooldown_multiplier = 0.95
capital_efficiency_multiplier = 0.99
model_reversal_buffer_multiplier = 0.96
profit_retention_multiplier = 1.03
slippage_multiplier = 0.97
size_retention_multiplier = 1.02

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "major"
horizon = "any"
market_type = "any"
event_subtype = "regulatory"
sigma_multiplier = 1.08
size_multiplier = 0.88
depth_ratio_multiplier = 1.12
min_edge_multiplier = 1.12
max_spread_multiplier = 0.92
hold_edge_multiplier = 1.12
edge_decay_exit_multiplier = 1.18
edge_decay_confirmation_scan_multiplier = 0.85
edge_decay_confirmation_window_multiplier = 0.85
edge_decay_cooldown_multiplier = 0.90
capital_efficiency_multiplier = 0.97
model_reversal_buffer_multiplier = 0.88
profit_retention_multiplier = 1.10
slippage_multiplier = 0.88
size_retention_multiplier = 1.10

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "any"
market_type = "any"
event_subtype = "unlock"
sigma_multiplier = 1.07
size_multiplier = 0.90
depth_ratio_multiplier = 1.08
min_edge_multiplier = 1.05
max_spread_multiplier = 0.98
hold_edge_multiplier = 1.08
edge_decay_exit_multiplier = 1.12
edge_decay_confirmation_scan_multiplier = 0.92
edge_decay_confirmation_window_multiplier = 0.92
edge_decay_cooldown_multiplier = 0.92
capital_efficiency_multiplier = 0.99
model_reversal_buffer_multiplier = 0.94
profit_retention_multiplier = 1.08
slippage_multiplier = 0.92
size_retention_multiplier = 1.06

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "short"
market_type = "binary"
event_subtype = "unlock"
sigma_multiplier = 1.10
size_multiplier = 0.82
depth_ratio_multiplier = 1.15
min_edge_multiplier = 1.08
max_spread_multiplier = 0.95
profit_retention_multiplier = 1.12
slippage_multiplier = 0.88
size_retention_multiplier = 1.12

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "any"
market_type = "any"
event_subtype = "upgrade"
sigma_multiplier = 1.05
size_multiplier = 0.90
depth_ratio_multiplier = 1.01
min_edge_multiplier = 1.04
max_spread_multiplier = 0.98
hold_edge_multiplier = 1.03
edge_decay_exit_multiplier = 1.06
edge_decay_confirmation_scan_multiplier = 0.97
edge_decay_confirmation_window_multiplier = 0.95
edge_decay_cooldown_multiplier = 0.97
capital_efficiency_multiplier = 0.995
model_reversal_buffer_multiplier = 0.97
profit_retention_multiplier = 1.02
slippage_multiplier = 0.98
size_retention_multiplier = 1.01

[[crypto_alpha.calibration_overrides]]
asset = "*"
asset_class = "alt"
horizon = "any"
market_type = "any"
event_subtype = "regulatory"
sigma_multiplier = 1.05
size_multiplier = 0.92
depth_ratio_multiplier = 1.06
min_edge_multiplier = 1.08
max_spread_multiplier = 0.95
hold_edge_multiplier = 1.08
edge_decay_exit_multiplier = 1.12
edge_decay_confirmation_scan_multiplier = 0.90
edge_decay_confirmation_window_multiplier = 0.90
edge_decay_cooldown_multiplier = 0.92
capital_efficiency_multiplier = 0.98
model_reversal_buffer_multiplier = 0.92
profit_retention_multiplier = 1.05
slippage_multiplier = 0.93
size_retention_multiplier = 1.06

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
| `PA_DATABASE__URL` | PostgreSQL 连接字符串；留空则禁用 ConfigStore/历史配置 | 可选 |
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
