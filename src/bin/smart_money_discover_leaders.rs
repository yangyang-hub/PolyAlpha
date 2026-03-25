use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use clap::{Parser, ValueEnum};
use pa_core::config::{Settings, SmartMoneyConfig};
use pa_storage::models::SmartMoneyLeaderCandidateRow;
use pa_storage::repository::Repository;
use reqwest::Url;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

const DATA_API_BASE: &str = "https://data-api.polymarket.com";
const GAMMA_API_BASE: &str = "https://gamma-api.polymarket.com";
const PAGE_LIMIT: usize = 200;
const CT_ADDRESS: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";
const TRANSFER_SINGLE_TOPIC: &str =
    "0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62";

/// Discover and score high-quality smart-money leader wallets from public Polymarket endpoints.
#[derive(Debug, Parser)]
#[command(
    name = "smart_money_discover_leaders",
    about = "Discover candidate smart-money leader wallets from Polymarket leaderboard and active-market APIs"
)]
struct Args {
    /// Optional database URL. Falls back to PA_DATABASE__URL, then config file database.url.
    #[arg(long)]
    database_url: Option<String>,

    /// How many leaderboard rows to collect.
    #[arg(long, default_value_t = 200)]
    leaderboard_limit: usize,

    /// How many active markets to sample.
    #[arg(long, default_value_t = 50)]
    market_limit: usize,

    /// Ignore active markets below this liquidity.
    #[arg(long, default_value = "1000")]
    min_market_liquidity: Decimal,

    /// How many top positions to sample per active market.
    #[arg(long, default_value_t = 8)]
    market_position_limit: usize,

    /// How many top holders to sample per active market outcome token.
    #[arg(long, default_value_t = 8)]
    holder_limit: usize,

    /// Maximum number of enriched candidates to score and output.
    #[arg(long, default_value_t = 100)]
    candidate_limit: usize,

    /// Minimum discovery score for a candidate to be included in output/export.
    #[arg(long)]
    min_score: Option<Decimal>,

    /// Optional RPC URL for chain-level candidate discovery. Falls back to config chain.rpc_url.
    #[arg(long)]
    rpc_url: Option<String>,

    /// Number of recent Polygon blocks to scan for Conditional Tokens transfers. 0 disables on-chain supplement.
    #[arg(long, default_value_t = 0)]
    onchain_lookback_blocks: u64,

    /// Maximum number of on-chain logs to consider after sorting by newest first.
    #[arg(long, default_value_t = 5000)]
    onchain_max_logs: usize,

    /// Output format written to stdout.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output: OutputFormat,

    /// Optional JSON file to write the full discovery summary.
    #[arg(long)]
    summary_output: Option<PathBuf>,

    /// Optional TOML snippet file containing `auto_discover_candidates = [...]`.
    #[arg(long)]
    emit_auto_discover_candidates: Option<PathBuf>,

    /// Optional TOML snippet file containing `[[smart_money.wallets]]` entries.
    #[arg(long)]
    emit_wallets_toml: Option<PathBuf>,

    /// How many top-scoring candidates to emit in TOML snippets.
    #[arg(long, default_value_t = 20)]
    emit_top: usize,

    /// Mark the top emitted candidates as promoted in the database.
    #[arg(long, default_value_t = false)]
    mark_promoted: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Deserialize)]
struct LeaderboardEntry {
    rank: String,
    #[serde(rename = "proxyWallet")]
    proxy_wallet: String,
    #[serde(rename = "userName", default)]
    user_name: String,
    #[serde(rename = "verifiedBadge", default)]
    verified_badge: bool,
    #[serde(rename = "vol")]
    volume: Decimal,
    pnl: Decimal,
}

#[derive(Debug, Deserialize)]
struct GammaMarket {
    #[serde(rename = "conditionId")]
    condition_id: String,
    question: String,
    liquidity: String,
}

#[derive(Debug, Deserialize)]
struct MarketPositionsGroup {
    token: String,
    positions: Vec<MarketPositionEntry>,
}

#[derive(Debug, Deserialize)]
struct MarketPositionEntry {
    #[serde(rename = "proxyWallet")]
    proxy_wallet: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    verified: bool,
    #[serde(rename = "totalBought", default)]
    total_bought: Decimal,
    #[serde(rename = "currentValue", default)]
    current_value: Decimal,
    #[serde(rename = "totalPnl", default)]
    total_pnl: Decimal,
}

#[derive(Debug, Deserialize)]
struct HolderGroup {
    token: String,
    holders: Vec<HolderEntry>,
}

#[derive(Debug, Deserialize)]
struct HolderEntry {
    #[serde(rename = "proxyWallet")]
    proxy_wallet: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    pseudonym: String,
    #[serde(default)]
    verified: bool,
}

#[derive(Debug, Deserialize)]
struct PublicProfile {
    #[serde(rename = "proxyWallet")]
    proxy_wallet: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    pseudonym: String,
    #[serde(rename = "verifiedBadge", default)]
    verified_badge: bool,
}

#[derive(Debug, Deserialize)]
struct OpenPosition {
    #[serde(rename = "currentValue", default)]
    current_value: Decimal,
    #[serde(rename = "totalBought", default)]
    total_bought: Decimal,
}

#[derive(Debug, Deserialize)]
struct ClosedPosition {
    #[serde(rename = "realizedPnl", default)]
    realized_pnl: Decimal,
    #[serde(rename = "totalBought", default)]
    total_bought: Decimal,
}

#[derive(Debug, Deserialize)]
struct ActivityEntry {
    timestamp: i64,
    #[serde(rename = "usdcSize", default)]
    usdc_size: Decimal,
}

#[derive(Debug, Clone)]
struct CandidateAccumulator {
    address: String,
    label: String,
    source_tags: BTreeSet<String>,
    first_seen_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    leaderboard_rank: Option<i32>,
    leaderboard_volume: Decimal,
    leaderboard_pnl: Decimal,
    open_positions_count: usize,
    open_notional: Decimal,
    open_total_bought: Decimal,
    closed_positions_count: usize,
    closed_total_bought: Decimal,
    closed_realized_pnl: Decimal,
    sampled_markets: BTreeSet<String>,
    market_position_count: usize,
    holder_position_count: usize,
    activity_volume: Decimal,
    activity_pnl: Decimal,
    onchain_transfer_count: usize,
    onchain_transfer_volume: Decimal,
    verified: bool,
    discovery_score: Decimal,
}

impl CandidateAccumulator {
    fn new(address: &str) -> Self {
        let now = Utc::now();
        Self {
            address: normalize_address(address),
            label: String::new(),
            source_tags: BTreeSet::new(),
            first_seen_at: now,
            last_seen_at: now,
            leaderboard_rank: None,
            leaderboard_volume: Decimal::ZERO,
            leaderboard_pnl: Decimal::ZERO,
            open_positions_count: 0,
            open_notional: Decimal::ZERO,
            open_total_bought: Decimal::ZERO,
            closed_positions_count: 0,
            closed_total_bought: Decimal::ZERO,
            closed_realized_pnl: Decimal::ZERO,
            sampled_markets: BTreeSet::new(),
            market_position_count: 0,
            holder_position_count: 0,
            activity_volume: Decimal::ZERO,
            activity_pnl: Decimal::ZERO,
            onchain_transfer_count: 0,
            onchain_transfer_volume: Decimal::ZERO,
            verified: false,
            discovery_score: Decimal::ZERO,
        }
    }

    fn row(&self, promoted: bool) -> SmartMoneyLeaderCandidateRow {
        SmartMoneyLeaderCandidateRow {
            address: self.address.clone(),
            label: self.label.clone(),
            source_tags: serde_json::to_value(self.source_tags.iter().collect::<Vec<_>>())
                .unwrap_or_else(|_| serde_json::json!([])),
            first_seen_at: self.first_seen_at,
            last_seen_at: self.last_seen_at,
            leaderboard_rank: self.leaderboard_rank,
            leaderboard_volume: self.leaderboard_volume,
            leaderboard_pnl: self.leaderboard_pnl,
            open_positions_count: self.open_positions_count as i32,
            open_notional: self.open_notional,
            closed_positions_count: self.closed_positions_count as i32,
            closed_total_bought: self.closed_total_bought,
            closed_realized_pnl: self.closed_realized_pnl,
            sampled_markets: self.sampled_markets.len() as i32,
            market_position_count: self.market_position_count as i32,
            holder_position_count: self.holder_position_count as i32,
            activity_volume: self.activity_volume,
            activity_pnl: self.activity_pnl,
            verified: self.verified,
            discovery_score: self.discovery_score,
            promoted,
            metadata: Some(serde_json::json!({
                "open_total_bought": self.open_total_bought,
                "source_tags": self.source_tags.iter().collect::<Vec<_>>(),
                "onchain_transfer_count": self.onchain_transfer_count,
                "onchain_transfer_volume": self.onchain_transfer_volume,
            })),
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Serialize)]
struct CandidateView {
    address: String,
    label: String,
    discovery_score: Decimal,
    source_tags: Vec<String>,
    leaderboard_rank: Option<i32>,
    leaderboard_volume: Decimal,
    leaderboard_pnl: Decimal,
    open_positions_count: usize,
    open_notional: Decimal,
    closed_positions_count: usize,
    closed_total_bought: Decimal,
    closed_realized_pnl: Decimal,
    sampled_markets: usize,
    market_position_count: usize,
    holder_position_count: usize,
    activity_volume: Decimal,
    onchain_transfer_count: usize,
    onchain_transfer_volume: Decimal,
    verified: bool,
}

#[derive(Debug, Serialize)]
struct DiscoverySummary {
    discovered_at: DateTime<Utc>,
    leaderboard_limit: usize,
    market_limit: usize,
    candidate_limit: usize,
    min_score: Decimal,
    candidate_count: usize,
    top_candidates: Vec<CandidateView>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let settings = Settings::load().ok();
    let smart_money_config = settings
        .as_ref()
        .map(|settings| settings.smart_money.clone())
        .unwrap_or_else(SmartMoneyConfig::default);
    let min_score = args
        .min_score
        .unwrap_or(smart_money_config.min_wallet_score);
    let database_url = resolved_database_url(&args, settings.as_ref());
    let rpc_url = resolved_rpc_url(&args, settings.as_ref());
    let repository = if let Some(url) = database_url.as_deref().filter(|url| !url.is_empty()) {
        let repo = Repository::connect(url, 4)
            .await
            .with_context(|| format!("failed to connect to database `{url}`"))?;
        repo.migrate()
            .await
            .context("failed to apply migrations for leader discovery")?;
        Some(repo)
    } else {
        None
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("failed to build HTTP client")?;

    let mut candidates = collect_seed_candidates(&client, &args, rpc_url.as_deref()).await?;
    let top_addresses = select_top_addresses(&candidates, args.candidate_limit);
    enrich_candidates(&client, &mut candidates, &top_addresses).await?;
    score_candidates(&mut candidates);

    let mut ranked: Vec<_> = candidates
        .into_values()
        .filter(|candidate| candidate.discovery_score >= min_score)
        .collect();
    ranked.sort_by(|a, b| {
        b.discovery_score
            .cmp(&a.discovery_score)
            .then_with(|| b.leaderboard_volume.cmp(&a.leaderboard_volume))
            .then_with(|| a.address.cmp(&b.address))
    });

    let emit_count = args.emit_top.min(ranked.len());
    let promoted_addresses: BTreeSet<_> = ranked
        .iter()
        .take(emit_count)
        .map(|candidate| candidate.address.clone())
        .collect();

    if let Some(repo) = &repository {
        for candidate in &ranked {
            repo.upsert_smart_money_leader_candidate(
                &candidate
                    .row(args.mark_promoted && promoted_addresses.contains(&candidate.address)),
            )
            .await
            .with_context(|| format!("failed to upsert candidate {}", candidate.address))?;
        }
    }

    if let Some(path) = &args.emit_auto_discover_candidates {
        write_auto_discover_candidates(path, &ranked, emit_count)?;
    }
    if let Some(path) = &args.emit_wallets_toml {
        write_wallets_toml(path, &ranked, emit_count)?;
    }

    let summary = DiscoverySummary {
        discovered_at: Utc::now(),
        leaderboard_limit: args.leaderboard_limit,
        market_limit: args.market_limit,
        candidate_limit: args.candidate_limit,
        min_score,
        candidate_count: ranked.len(),
        top_candidates: ranked
            .iter()
            .take(args.candidate_limit.min(50))
            .map(candidate_view)
            .collect(),
    };
    if let Some(path) = &args.summary_output {
        write_json_file(path, &summary)?;
    }

    match args.output {
        OutputFormat::Text => print_text_summary(&summary),
        OutputFormat::Json => serde_json::to_writer_pretty(std::io::stdout(), &summary)
            .context("failed to write JSON summary to stdout")?,
    }
    if matches!(args.output, OutputFormat::Json) {
        println!();
    }

    Ok(())
}

fn resolved_database_url(args: &Args, settings: Option<&Settings>) -> Option<String> {
    args.database_url
        .clone()
        .or_else(|| std::env::var("PA_DATABASE__URL").ok())
        .or_else(|| settings.map(|settings| settings.database.url.clone()))
}

fn resolved_rpc_url(args: &Args, settings: Option<&Settings>) -> Option<String> {
    args.rpc_url
        .clone()
        .or_else(|| settings.map(|settings| settings.chain.rpc_url.clone()))
        .filter(|url| !url.is_empty())
}

async fn collect_seed_candidates(
    client: &reqwest::Client,
    args: &Args,
    rpc_url: Option<&str>,
) -> Result<HashMap<String, CandidateAccumulator>> {
    let mut candidates = HashMap::new();
    collect_leaderboard_candidates(client, args.leaderboard_limit, &mut candidates).await?;
    collect_active_market_candidates(client, args, &mut candidates).await?;
    if args.onchain_lookback_blocks > 0 {
        if let Some(rpc_url) = rpc_url {
            collect_onchain_transfer_candidates(client, rpc_url, args, &mut candidates).await?;
        } else {
            tracing::warn!(
                "smart_money_discover_leaders: on-chain supplement requested but no RPC URL was configured"
            );
        }
    }
    Ok(candidates)
}

async fn collect_leaderboard_candidates(
    client: &reqwest::Client,
    limit: usize,
    candidates: &mut HashMap<String, CandidateAccumulator>,
) -> Result<()> {
    let mut offset = 0usize;
    while offset < limit {
        let page_size = PAGE_LIMIT.min(limit - offset);
        let url = data_api_url(
            "/v1/leaderboard",
            &[
                ("limit", page_size.to_string()),
                ("offset", offset.to_string()),
            ],
        )?;
        let page: Vec<LeaderboardEntry> = fetch_json(client, url).await?;
        if page.is_empty() {
            break;
        }
        for entry in page {
            let address = normalize_address(&entry.proxy_wallet);
            let candidate = candidates
                .entry(address.clone())
                .or_insert_with(|| CandidateAccumulator::new(&address));
            candidate.source_tags.insert("leaderboard".into());
            candidate.label = choose_label(&candidate.label, &entry.user_name, &address);
            candidate.verified |= entry.verified_badge;
            let rank = entry.rank.parse::<i32>().ok();
            candidate.leaderboard_rank = match (candidate.leaderboard_rank, rank) {
                (Some(existing), Some(new_rank)) => Some(existing.min(new_rank)),
                (None, some_rank) => some_rank,
                (existing, None) => existing,
            };
            candidate.leaderboard_volume = candidate.leaderboard_volume.max(entry.volume);
            candidate.leaderboard_pnl = candidate.leaderboard_pnl.max(entry.pnl);
        }
        if page_size < PAGE_LIMIT {
            break;
        }
        offset += page_size;
    }
    Ok(())
}

async fn collect_onchain_transfer_candidates(
    client: &reqwest::Client,
    rpc_url: &str,
    args: &Args,
    candidates: &mut HashMap<String, CandidateAccumulator>,
) -> Result<()> {
    let latest_block = rpc_block_number(client, rpc_url).await?;
    if latest_block == 0 {
        return Ok(());
    }
    let from_block = latest_block.saturating_sub(args.onchain_lookback_blocks);
    let mut logs = rpc_get_logs(
        client,
        rpc_url,
        CT_ADDRESS,
        TRANSFER_SINGLE_TOPIC,
        from_block,
        latest_block,
    )
    .await?;
    logs.sort_by_key(|log| std::cmp::Reverse(log.block_number));
    for log in logs.into_iter().take(args.onchain_max_logs) {
        for address in [log.from, log.to] {
            if !is_probably_wallet(&address) {
                continue;
            }
            let candidate = candidates
                .entry(address.clone())
                .or_insert_with(|| CandidateAccumulator::new(&address));
            candidate.source_tags.insert("onchain_ct_transfers".into());
            candidate.onchain_transfer_count += 1;
            candidate.onchain_transfer_volume += log.value;
            candidate.last_seen_at = candidate.last_seen_at.max(log.detected_at);
        }
    }
    Ok(())
}

async fn collect_active_market_candidates(
    client: &reqwest::Client,
    args: &Args,
    candidates: &mut HashMap<String, CandidateAccumulator>,
) -> Result<()> {
    let url = gamma_api_url(
        "/markets",
        &[
            ("active", "true".into()),
            ("closed", "false".into()),
            ("limit", args.market_limit.to_string()),
        ],
    )?;
    let markets: Vec<GammaMarket> = fetch_json(client, url).await?;
    for market in markets {
        let liquidity = parse_decimal(&market.liquidity);
        if liquidity < args.min_market_liquidity {
            continue;
        }
        collect_market_positions(client, &market, args.market_position_limit, candidates).await?;
        collect_market_holders(client, &market, args.holder_limit, candidates).await?;
    }
    Ok(())
}

async fn collect_market_positions(
    client: &reqwest::Client,
    market: &GammaMarket,
    limit: usize,
    candidates: &mut HashMap<String, CandidateAccumulator>,
) -> Result<()> {
    let url = data_api_url(
        "/v1/market-positions",
        &[
            ("market", market.condition_id.clone()),
            ("limit", limit.to_string()),
        ],
    )?;
    let groups: Vec<MarketPositionsGroup> = fetch_json(client, url).await.unwrap_or_default();
    for group in groups {
        let _token = group.token;
        for entry in group.positions.into_iter().take(limit) {
            let address = normalize_address(&entry.proxy_wallet);
            let candidate = candidates
                .entry(address.clone())
                .or_insert_with(|| CandidateAccumulator::new(&address));
            candidate
                .source_tags
                .insert("active_market_positions".into());
            candidate.label = choose_label(&candidate.label, &entry.name, &address);
            candidate.verified |= entry.verified;
            candidate
                .sampled_markets
                .insert(market.condition_id.clone());
            candidate.market_position_count += 1;
            candidate.open_notional += entry.current_value.max(Decimal::ZERO);
            candidate.open_total_bought += entry.total_bought.max(Decimal::ZERO);
            candidate.activity_pnl += entry.total_pnl;
            if !market.question.is_empty() {
                candidate
                    .source_tags
                    .insert(format!("market:{}", truncate_slug(&market.question)));
            }
        }
    }
    Ok(())
}

async fn collect_market_holders(
    client: &reqwest::Client,
    market: &GammaMarket,
    limit: usize,
    candidates: &mut HashMap<String, CandidateAccumulator>,
) -> Result<()> {
    let url = data_api_url(
        "/holders",
        &[
            ("market", market.condition_id.clone()),
            ("limit", limit.to_string()),
        ],
    )?;
    let groups: Vec<HolderGroup> = fetch_json(client, url).await.unwrap_or_default();
    for group in groups {
        let _token = group.token;
        for entry in group.holders.into_iter().take(limit) {
            let address = normalize_address(&entry.proxy_wallet);
            let candidate = candidates
                .entry(address.clone())
                .or_insert_with(|| CandidateAccumulator::new(&address));
            candidate.source_tags.insert("active_market_holders".into());
            candidate.label = choose_label(
                &candidate.label,
                if entry.name.is_empty() {
                    &entry.pseudonym
                } else {
                    &entry.name
                },
                &address,
            );
            candidate.verified |= entry.verified;
            candidate
                .sampled_markets
                .insert(market.condition_id.clone());
            candidate.holder_position_count += 1;
        }
    }
    Ok(())
}

fn select_top_addresses(
    candidates: &HashMap<String, CandidateAccumulator>,
    limit: usize,
) -> Vec<String> {
    let mut entries: Vec<_> = candidates.values().collect();
    entries.sort_by(|a, b| {
        b.leaderboard_pnl
            .cmp(&a.leaderboard_pnl)
            .then_with(|| b.leaderboard_volume.cmp(&a.leaderboard_volume))
            .then_with(|| b.sampled_markets.len().cmp(&a.sampled_markets.len()))
            .then_with(|| b.market_position_count.cmp(&a.market_position_count))
            .then_with(|| a.address.cmp(&b.address))
    });
    entries
        .into_iter()
        .take(limit)
        .map(|candidate| candidate.address.clone())
        .collect()
}

async fn enrich_candidates(
    client: &reqwest::Client,
    candidates: &mut HashMap<String, CandidateAccumulator>,
    addresses: &[String],
) -> Result<()> {
    for address in addresses {
        if let Some(candidate) = candidates.get_mut(address) {
            if let Ok(profile) = fetch_public_profile(client, address).await {
                candidate.verified |= profile.verified_badge;
                candidate.label = choose_label(
                    &candidate.label,
                    if profile.name.is_empty() {
                        &profile.pseudonym
                    } else {
                        &profile.name
                    },
                    &profile.proxy_wallet,
                );
            }
            let open_positions =
                fetch_paginated_positions::<OpenPosition>(client, "/positions", "user", address)
                    .await
                    .unwrap_or_default();
            candidate.open_positions_count = open_positions.len();
            candidate.open_notional = open_positions
                .iter()
                .map(|position| position.current_value.max(Decimal::ZERO))
                .sum();
            candidate.open_total_bought = open_positions
                .iter()
                .map(|position| position.total_bought.max(Decimal::ZERO))
                .sum();

            let closed_positions = fetch_paginated_positions::<ClosedPosition>(
                client,
                "/closed-positions",
                "user",
                address,
            )
            .await
            .unwrap_or_default();
            candidate.closed_positions_count = closed_positions.len();
            candidate.closed_total_bought = closed_positions
                .iter()
                .map(|position| position.total_bought.max(Decimal::ZERO))
                .sum();
            candidate.closed_realized_pnl = closed_positions
                .iter()
                .map(|position| position.realized_pnl)
                .sum();

            let activities = fetch_recent_activity(client, address)
                .await
                .unwrap_or_default();
            candidate.activity_volume = activities.iter().map(|entry| entry.usdc_size).sum();
            if let Some(last_seen) = activities
                .iter()
                .filter_map(|entry| Utc.timestamp_opt(entry.timestamp, 0).single())
                .max()
            {
                candidate.last_seen_at = last_seen;
            }
        }
    }
    Ok(())
}

fn score_candidates(candidates: &mut HashMap<String, CandidateAccumulator>) {
    for candidate in candidates.values_mut() {
        let leaderboard_efficiency =
            ratio_or_zero(candidate.leaderboard_pnl, candidate.leaderboard_volume);
        let realized_efficiency =
            ratio_or_zero(candidate.closed_realized_pnl, candidate.closed_total_bought);
        let open_efficiency = ratio_or_zero(candidate.activity_pnl, candidate.open_total_bought);

        let base = leaderboard_efficiency * Decimal::new(45, 2)
            + realized_efficiency * Decimal::new(35, 2)
            + open_efficiency * Decimal::new(20, 2);
        let breadth_bonus = Decimal::from(candidate.sampled_markets.len() as u64)
            .min(Decimal::from(20))
            * Decimal::new(25, 4);
        let position_bonus = Decimal::from(candidate.market_position_count as u64)
            .min(Decimal::from(20))
            * Decimal::new(10, 4);
        let holder_bonus = Decimal::from(candidate.holder_position_count as u64)
            .min(Decimal::from(20))
            * Decimal::new(5, 4);
        let onchain_bonus = Decimal::from(candidate.onchain_transfer_count as u64)
            .min(Decimal::from(25))
            * Decimal::new(3, 4);
        let onchain_volume_bonus =
            (ratio_or_zero(candidate.onchain_transfer_volume, Decimal::from(10_000))
                * Decimal::new(5, 2))
            .min(Decimal::new(10, 2));
        let verified_bonus = if candidate.verified {
            Decimal::new(2, 2)
        } else {
            Decimal::ZERO
        };
        let scale_volume =
            candidate.leaderboard_volume + candidate.closed_total_bought + candidate.open_notional;
        let size_bonus = (ratio_or_zero(scale_volume, Decimal::from(50_000)) * Decimal::new(5, 2))
            .min(Decimal::new(15, 2));
        candidate.discovery_score = base.max(Decimal::ZERO)
            + breadth_bonus
            + position_bonus
            + holder_bonus
            + onchain_bonus
            + onchain_volume_bonus
            + verified_bonus
            + size_bonus;
    }
}

async fn fetch_public_profile(client: &reqwest::Client, address: &str) -> Result<PublicProfile> {
    let url = gamma_api_url("/public-profile", &[("address", address.to_string())])?;
    fetch_json(client, url).await
}

async fn fetch_recent_activity(
    client: &reqwest::Client,
    address: &str,
) -> Result<Vec<ActivityEntry>> {
    let url = data_api_url(
        "/activity",
        &[
            ("user", address.to_string()),
            ("limit", PAGE_LIMIT.to_string()),
        ],
    )?;
    fetch_json(client, url).await
}

async fn fetch_paginated_positions<T>(
    client: &reqwest::Client,
    path: &str,
    address_key: &str,
    address: &str,
) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    let mut rows = Vec::new();
    let mut offset = 0usize;
    loop {
        let url = data_api_url(
            path,
            &[
                (address_key, address.to_string()),
                ("limit", PAGE_LIMIT.to_string()),
                ("offset", offset.to_string()),
                ("sizeThreshold", "0".into()),
            ],
        )?;
        let page: Vec<T> = fetch_json(client, url).await?;
        let page_len = page.len();
        rows.extend(page);
        if page_len < PAGE_LIMIT {
            break;
        }
        offset += PAGE_LIMIT;
        if offset >= 5_000 {
            break;
        }
    }
    Ok(rows)
}

async fn fetch_json<T>(client: &reqwest::Client, url: Url) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let response = client
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("request failed for {url}"))?
        .error_for_status()
        .with_context(|| format!("request returned error status for {url}"))?;
    response
        .json()
        .await
        .with_context(|| format!("failed to decode JSON from {url}"))
}

fn data_api_url(path: &str, params: &[(impl AsRef<str>, String)]) -> Result<Url> {
    build_url(DATA_API_BASE, path, params)
}

fn gamma_api_url(path: &str, params: &[(impl AsRef<str>, String)]) -> Result<Url> {
    build_url(GAMMA_API_BASE, path, params)
}

fn build_url(base: &str, path: &str, params: &[(impl AsRef<str>, String)]) -> Result<Url> {
    let mut url = Url::parse(base)
        .with_context(|| format!("invalid base URL `{base}`"))?
        .join(path)
        .with_context(|| format!("failed to join `{path}` onto `{base}`"))?;
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            pairs.append_pair(key.as_ref(), value);
        }
    }
    Ok(url)
}

fn candidate_view(candidate: &CandidateAccumulator) -> CandidateView {
    CandidateView {
        address: candidate.address.clone(),
        label: candidate.label.clone(),
        discovery_score: candidate.discovery_score,
        source_tags: candidate.source_tags.iter().cloned().collect(),
        leaderboard_rank: candidate.leaderboard_rank,
        leaderboard_volume: candidate.leaderboard_volume,
        leaderboard_pnl: candidate.leaderboard_pnl,
        open_positions_count: candidate.open_positions_count,
        open_notional: candidate.open_notional,
        closed_positions_count: candidate.closed_positions_count,
        closed_total_bought: candidate.closed_total_bought,
        closed_realized_pnl: candidate.closed_realized_pnl,
        sampled_markets: candidate.sampled_markets.len(),
        market_position_count: candidate.market_position_count,
        holder_position_count: candidate.holder_position_count,
        activity_volume: candidate.activity_volume,
        onchain_transfer_count: candidate.onchain_transfer_count,
        onchain_transfer_volume: candidate.onchain_transfer_volume,
        verified: candidate.verified,
    }
}

fn write_auto_discover_candidates(
    path: &PathBuf,
    ranked: &[CandidateAccumulator],
    count: usize,
) -> Result<()> {
    let mut writer = create_writer(path)?;
    writer.write_all(b"auto_discover_candidates = [\n")?;
    for candidate in ranked.iter().take(count) {
        writeln!(writer, "  \"{}\",", candidate.address)?;
    }
    writer.write_all(b"]\n")?;
    writer.flush()?;
    Ok(())
}

fn write_wallets_toml(path: &PathBuf, ranked: &[CandidateAccumulator], count: usize) -> Result<()> {
    let mut writer = create_writer(path)?;
    for candidate in ranked.iter().take(count) {
        writer.write_all(b"[[smart_money.wallets]]\n")?;
        writeln!(writer, "address = \"{}\"", candidate.address)?;
        writeln!(
            writer,
            "label = \"{}\"",
            toml_escape(&derived_label(candidate))
        )?;
        writer.write_all(b"weight = 1.0\n\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn write_json_file(path: &PathBuf, value: &impl Serialize) -> Result<()> {
    let writer = create_writer(path)?;
    serde_json::to_writer_pretty(writer, value)
        .with_context(|| format!("failed to serialize JSON to {}", path.display()))
}

fn create_writer(path: &PathBuf) -> Result<BufWriter<File>> {
    let file = File::create(path)
        .with_context(|| format!("failed to create output file {}", path.display()))?;
    Ok(BufWriter::new(file))
}

fn print_text_summary(summary: &DiscoverySummary) {
    println!(
        "discovered {} candidates at {} (min_score = {})",
        summary.candidate_count, summary.discovered_at, summary.min_score
    );
    for (idx, candidate) in summary.top_candidates.iter().enumerate() {
        println!(
            "{:>2}. {} score={} rank={:?} vol={} pnl={} markets={} sources={}",
            idx + 1,
            if candidate.label.is_empty() {
                &candidate.address
            } else {
                &candidate.label
            },
            candidate.discovery_score,
            candidate.leaderboard_rank,
            candidate.leaderboard_volume,
            candidate.leaderboard_pnl,
            candidate.sampled_markets,
            candidate.source_tags.join(",")
        );
        println!("    {}", candidate.address);
    }
}

fn derived_label(candidate: &CandidateAccumulator) -> String {
    if !candidate.label.trim().is_empty() {
        candidate.label.trim().to_string()
    } else {
        format!(
            "leader_{}",
            &candidate.address[2..10.min(candidate.address.len())]
        )
    }
}

fn choose_label(existing: &str, proposed: &str, address: &str) -> String {
    if !existing.trim().is_empty() {
        return existing.to_string();
    }
    let proposed = proposed.trim();
    if !proposed.is_empty() && proposed != address {
        proposed.to_string()
    } else {
        String::new()
    }
}

fn normalize_address(address: &str) -> String {
    address.trim().to_lowercase()
}

fn truncate_slug(value: &str) -> String {
    let mut slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug.trim_matches('-').chars().take(32).collect()
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn ratio_or_zero(numerator: Decimal, denominator: Decimal) -> Decimal {
    if denominator > Decimal::ZERO {
        numerator / denominator
    } else {
        Decimal::ZERO
    }
}

fn parse_decimal(value: &str) -> Decimal {
    value.parse().unwrap_or(Decimal::ZERO)
}

#[derive(Debug, Clone)]
struct TransferLog {
    from: String,
    to: String,
    value: Decimal,
    block_number: u64,
    detected_at: DateTime<Utc>,
}

async fn rpc_block_number(client: &reqwest::Client, rpc_url: &str) -> Result<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });
    let resp: serde_json::Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("eth_blockNumber request failed for {rpc_url}"))?
        .json()
        .await
        .with_context(|| format!("eth_blockNumber parse failed for {rpc_url}"))?;
    let hex = resp["result"].as_str().unwrap_or("0x0");
    Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or(0))
}

async fn rpc_get_logs(
    client: &reqwest::Client,
    rpc_url: &str,
    contract: &str,
    event_topic: &str,
    from_block: u64,
    to_block: u64,
) -> Result<Vec<TransferLog>> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getLogs",
        "params": [{
            "address": contract,
            "topics": [event_topic],
            "fromBlock": format!("0x{:x}", from_block),
            "toBlock": format!("0x{:x}", to_block),
        }],
        "id": 1
    });
    let resp: serde_json::Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("eth_getLogs request failed for {rpc_url}"))?
        .json()
        .await
        .with_context(|| format!("eth_getLogs parse failed for {rpc_url}"))?;
    let raw_logs = resp["result"].as_array().cloned().unwrap_or_default();
    Ok(raw_logs.iter().filter_map(parse_transfer_log).collect())
}

fn parse_transfer_log(log: &serde_json::Value) -> Option<TransferLog> {
    let topics = log.get("topics")?.as_array()?;
    if topics.len() < 4 {
        return None;
    }
    let from = extract_address_from_topic(topics[2].as_str()?);
    let to = extract_address_from_topic(topics[3].as_str()?);
    let data = log
        .get("data")?
        .as_str()?
        .strip_prefix("0x")
        .unwrap_or_default();
    if data.len() < 128 {
        return None;
    }
    let value_hex = &data[64..128];
    let value = parse_u256_hex_to_decimal(value_hex);
    let block_number = u64::from_str_radix(
        log.get("blockNumber")
            .and_then(|value| value.as_str())
            .unwrap_or("0x0")
            .trim_start_matches("0x"),
        16,
    )
    .unwrap_or(0);
    let detected_at = Utc::now();
    Some(TransferLog {
        from,
        to,
        value,
        block_number,
        detected_at,
    })
}

fn extract_address_from_topic(topic: &str) -> String {
    let hex = topic.strip_prefix("0x").unwrap_or(topic);
    if hex.len() >= 40 {
        format!("0x{}", &hex[hex.len() - 40..]).to_lowercase()
    } else {
        topic.to_lowercase()
    }
}

fn parse_u256_hex_to_decimal(hex: &str) -> Decimal {
    let trimmed = hex.trim_start_matches('0');
    if trimmed.is_empty() {
        return Decimal::ZERO;
    }
    let mut value = Decimal::ZERO;
    for ch in trimmed.chars() {
        let digit = ch.to_digit(16).unwrap_or(0);
        value = value * Decimal::from(16u64) + Decimal::from(digit);
    }
    value
}

fn is_probably_wallet(address: &str) -> bool {
    address.starts_with("0x")
        && address.len() == 42
        && address != "0x0000000000000000000000000000000000000000"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_label_prefers_existing_non_empty_label() {
        assert_eq!(choose_label("known", "new", "0xabc"), "known");
    }

    #[test]
    fn derived_label_falls_back_to_address_prefix() {
        let candidate = CandidateAccumulator::new("0x1234567890abcdef");
        assert!(derived_label(&candidate).starts_with("leader_"));
    }

    #[test]
    fn score_formula_rewards_positive_efficiency_and_breadth() {
        let mut map = HashMap::new();
        let mut candidate = CandidateAccumulator::new("0xabc");
        candidate.leaderboard_volume = Decimal::from(1_000);
        candidate.leaderboard_pnl = Decimal::from(120);
        candidate.closed_total_bought = Decimal::from(800);
        candidate.closed_realized_pnl = Decimal::from(64);
        candidate.sampled_markets.insert("m1".into());
        candidate.sampled_markets.insert("m2".into());
        candidate.market_position_count = 4;
        map.insert(candidate.address.clone(), candidate);
        score_candidates(&mut map);
        assert!(map.get("0xabc").unwrap().discovery_score > Decimal::new(5, 2));
    }

    #[test]
    fn extracts_address_from_topic_tail() {
        let topic = "0x00000000000000000000000003e8a544e97eeff5753bc1e90d46e5ef22af1697";
        assert_eq!(
            extract_address_from_topic(topic),
            "0x03e8a544e97eeff5753bc1e90d46e5ef22af1697"
        );
    }

    #[test]
    fn parses_large_hex_values_to_decimal() {
        assert_eq!(parse_u256_hex_to_decimal("0f"), Decimal::from(15u64));
        assert_eq!(parse_u256_hex_to_decimal("10"), Decimal::from(16u64));
    }
}
