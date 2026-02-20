use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use pa_core::types::{ExecutionResult, ExecutionStatus, StrategyType};

use crate::simulator::SimulatorConfig;

/// Configuration for a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub initial_balance: Decimal,
    pub simulator: SimulatorConfig,
}

/// A single point on the PnL curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnlPoint {
    pub timestamp: DateTime<Utc>,
    pub cumulative_pnl: Decimal,
    pub balance: Decimal,
}

/// Per-strategy performance breakdown.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrategyStats {
    pub count: usize,
    pub wins: usize,
    pub total_pnl: Decimal,
    pub win_rate: Decimal,
}

/// Aggregate performance summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceSummary {
    pub total_pnl: Decimal,
    pub total_fees: Decimal,
    pub total_gas: Decimal,
    pub trade_count: usize,
    pub win_count: usize,
    pub loss_count: usize,
    pub win_rate: Decimal,
    pub avg_profit_per_trade: Decimal,
    pub max_drawdown: Decimal,
    pub sharpe_ratio: Option<Decimal>,
    pub roi: Decimal,
    pub by_strategy: HashMap<StrategyType, StrategyStats>,
}

/// Full backtest result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub config: BacktestConfig,
    pub total_frames: usize,
    pub opportunities_detected: usize,
    pub opportunities_rejected: usize,
    pub trades_executed: usize,
    pub executions: Vec<ExecutionResult>,
    pub pnl_curve: Vec<PnlPoint>,
    pub summary: PerformanceSummary,
}

impl BacktestResult {
    /// Build a `BacktestResult` from a list of execution results and metadata.
    pub fn build(
        config: BacktestConfig,
        total_frames: usize,
        opportunities_detected: usize,
        opportunities_rejected: usize,
        executions: Vec<ExecutionResult>,
        strategy_types: &[StrategyType],
    ) -> Self {
        let trades_executed = executions
            .iter()
            .filter(|e| !matches!(e.status, ExecutionStatus::NoFill | ExecutionStatus::Failed))
            .count();

        let pnl_curve = Self::build_pnl_curve(&executions, config.initial_balance);
        let summary = Self::compute_summary(
            &executions,
            &pnl_curve,
            config.initial_balance,
            strategy_types,
        );

        Self {
            config,
            total_frames,
            opportunities_detected,
            opportunities_rejected,
            trades_executed,
            executions,
            pnl_curve,
            summary,
        }
    }

    fn build_pnl_curve(executions: &[ExecutionResult], initial_balance: Decimal) -> Vec<PnlPoint> {
        let mut curve = Vec::with_capacity(executions.len() + 1);
        let mut cumulative = Decimal::ZERO;

        for exec in executions {
            if matches!(exec.status, ExecutionStatus::NoFill | ExecutionStatus::Failed) {
                continue;
            }
            cumulative += exec.realized_profit;
            curve.push(PnlPoint {
                timestamp: exec.executed_at,
                cumulative_pnl: cumulative,
                balance: initial_balance + cumulative,
            });
        }

        curve
    }

    fn compute_summary(
        executions: &[ExecutionResult],
        pnl_curve: &[PnlPoint],
        initial_balance: Decimal,
        strategy_types: &[StrategyType],
    ) -> PerformanceSummary {
        let filled: Vec<&ExecutionResult> = executions
            .iter()
            .filter(|e| !matches!(e.status, ExecutionStatus::NoFill | ExecutionStatus::Failed))
            .collect();

        let trade_count = filled.len();
        let total_pnl: Decimal = filled.iter().map(|e| e.realized_profit).sum();
        let total_fees: Decimal = filled.iter().map(|e| e.total_fees).sum();
        let total_gas: Decimal = filled.iter().map(|e| e.total_gas).sum();
        let win_count = filled.iter().filter(|e| e.realized_profit > Decimal::ZERO).count();
        let loss_count = trade_count.saturating_sub(win_count);

        let win_rate = if trade_count > 0 {
            Decimal::from(win_count as u64) / Decimal::from(trade_count as u64)
        } else {
            Decimal::ZERO
        };

        let avg_profit_per_trade = if trade_count > 0 {
            total_pnl / Decimal::from(trade_count as u64)
        } else {
            Decimal::ZERO
        };

        let max_drawdown = Self::compute_max_drawdown(pnl_curve, initial_balance);
        let sharpe_ratio = Self::compute_sharpe(&filled);

        let roi = if initial_balance > Decimal::ZERO {
            total_pnl / initial_balance
        } else {
            Decimal::ZERO
        };

        // Per-strategy breakdown (paired by index with strategy_types)
        let mut by_strategy: HashMap<StrategyType, StrategyStats> = HashMap::new();
        for (i, exec) in executions.iter().enumerate() {
            if matches!(exec.status, ExecutionStatus::NoFill | ExecutionStatus::Failed) {
                continue;
            }
            if let Some(&st) = strategy_types.get(i) {
                let entry = by_strategy.entry(st).or_default();
                entry.count += 1;
                entry.total_pnl += exec.realized_profit;
                if exec.realized_profit > Decimal::ZERO {
                    entry.wins += 1;
                }
            }
        }
        for stats in by_strategy.values_mut() {
            stats.win_rate = if stats.count > 0 {
                Decimal::from(stats.wins as u64) / Decimal::from(stats.count as u64)
            } else {
                Decimal::ZERO
            };
        }

        PerformanceSummary {
            total_pnl,
            total_fees,
            total_gas,
            trade_count,
            win_count,
            loss_count,
            win_rate,
            avg_profit_per_trade,
            max_drawdown,
            sharpe_ratio,
            roi,
            by_strategy,
        }
    }

    /// Compute maximum drawdown from the PnL curve.
    fn compute_max_drawdown(pnl_curve: &[PnlPoint], initial_balance: Decimal) -> Decimal {
        if pnl_curve.is_empty() {
            return Decimal::ZERO;
        }

        let mut peak = initial_balance;
        let mut max_dd = Decimal::ZERO;

        for point in pnl_curve {
            if point.balance > peak {
                peak = point.balance;
            }
            let drawdown = peak - point.balance;
            if drawdown > max_dd {
                max_dd = drawdown;
            }
        }

        max_dd
    }

    /// Compute Sharpe ratio from individual trade profits.
    ///
    /// Sharpe = mean(returns) / std(returns)
    /// Returns `None` if fewer than 2 trades.
    fn compute_sharpe(filled: &[&ExecutionResult]) -> Option<Decimal> {
        if filled.len() < 2 {
            return None;
        }

        let profits: Vec<Decimal> = filled.iter().map(|e| e.realized_profit).collect();
        let n = Decimal::from(profits.len() as u64);
        let mean = profits.iter().copied().sum::<Decimal>() / n;

        let variance = profits
            .iter()
            .map(|&p| {
                let diff = p - mean;
                diff * diff
            })
            .sum::<Decimal>()
            / (n - Decimal::ONE);

        let std_dev = decimal_sqrt(variance)?;
        if std_dev <= Decimal::ZERO {
            return None;
        }

        Some(mean / std_dev)
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Print a formatted summary to stdout.
    pub fn print_summary(&self) {
        println!("{self}");
    }
}

impl fmt::Display for BacktestResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = &self.summary;
        writeln!(f, "=== Backtest Report ===")?;
        writeln!(
            f,
            "Period: {} to {}",
            self.config.from.format("%Y-%m-%d %H:%M"),
            self.config.to.format("%Y-%m-%d %H:%M"),
        )?;
        writeln!(f, "Initial balance:     ${}", self.config.initial_balance)?;
        writeln!(f, "Frames replayed:     {}", self.total_frames)?;
        writeln!(f, "Opportunities found: {}", self.opportunities_detected)?;
        writeln!(f, "Rejected by risk:    {}", self.opportunities_rejected)?;
        writeln!(f, "Trades executed:     {}", self.trades_executed)?;
        writeln!(f, "---")?;
        writeln!(f, "Total PnL:           ${}", s.total_pnl)?;
        writeln!(f, "Total fees:          ${}", s.total_fees)?;
        writeln!(f, "Total gas:           ${}", s.total_gas)?;
        writeln!(f, "Win / Loss:          {} / {}", s.win_count, s.loss_count)?;
        writeln!(f, "Win rate:            {:.1}%", s.win_rate * dec!(100))?;
        writeln!(f, "Avg profit/trade:    ${}", s.avg_profit_per_trade)?;
        writeln!(f, "Max drawdown:        ${}", s.max_drawdown)?;
        writeln!(f, "ROI:                 {:.2}%", s.roi * dec!(100))?;
        if let Some(sharpe) = s.sharpe_ratio {
            writeln!(f, "Sharpe ratio:        {sharpe:.4}")?;
        }
        writeln!(f, "=== End Report ===")?;
        Ok(())
    }
}

/// Approximate square root for Decimal using Newton's method.
fn decimal_sqrt(value: Decimal) -> Option<Decimal> {
    if value < Decimal::ZERO {
        return None;
    }
    if value == Decimal::ZERO {
        return Some(Decimal::ZERO);
    }

    let mut guess = value / dec!(2);
    for _ in 0..20 {
        if guess == Decimal::ZERO {
            return Some(Decimal::ZERO);
        }
        let next = (guess + value / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.0000001) {
            return Some(next);
        }
        guess = next;
    }
    Some(guess)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    fn make_exec(profit: Decimal, fees: Decimal, gas: Decimal) -> ExecutionResult {
        ExecutionResult {
            opportunity_id: Uuid::now_v7(),
            strategy_type: StrategyType::YesNoMerge,
            status: if profit > Decimal::ZERO {
                ExecutionStatus::Success
            } else {
                ExecutionStatus::PartialFill
            },
            trades: vec![],
            realized_profit: profit,
            total_fees: fees,
            total_gas: gas,
            executed_at: Utc::now(),
        }
    }

    #[test]
    fn test_build_result_basic() {
        let config = BacktestConfig {
            from: Utc::now(),
            to: Utc::now(),
            initial_balance: dec!(1000),
            simulator: SimulatorConfig::default(),
        };

        let execs = vec![
            make_exec(dec!(5.0), dec!(0.5), dec!(0.01)),
            make_exec(dec!(-2.0), dec!(0.3), dec!(0.01)),
            make_exec(dec!(3.0), dec!(0.4), dec!(0.01)),
        ];

        let result = BacktestResult::build(config, 100, 10, 2, execs, &[]);

        assert_eq!(result.summary.trade_count, 3);
        assert_eq!(result.summary.win_count, 2);
        assert_eq!(result.summary.loss_count, 1);
        assert_eq!(result.summary.total_pnl, dec!(6.0));
        assert_eq!(result.summary.total_fees, dec!(1.2));
        assert!(result.summary.win_rate > dec!(0.6));
    }

    #[test]
    fn test_max_drawdown() {
        let config = BacktestConfig {
            from: Utc::now(),
            to: Utc::now(),
            initial_balance: dec!(100),
            simulator: SimulatorConfig::default(),
        };

        // +10, -20, +5 -> balance: 110, 90, 95
        // Peak at 110, trough at 90 -> drawdown = 20
        let execs = vec![
            make_exec(dec!(10.0), Decimal::ZERO, Decimal::ZERO),
            make_exec(dec!(-20.0), Decimal::ZERO, Decimal::ZERO),
            make_exec(dec!(5.0), Decimal::ZERO, Decimal::ZERO),
        ];

        let result = BacktestResult::build(config, 50, 5, 0, execs, &[]);
        assert_eq!(result.summary.max_drawdown, dec!(20.0));
    }

    #[test]
    fn test_empty_backtest() {
        let config = BacktestConfig {
            from: Utc::now(),
            to: Utc::now(),
            initial_balance: dec!(1000),
            simulator: SimulatorConfig::default(),
        };

        let result = BacktestResult::build(config, 0, 0, 0, vec![], &[]);

        assert_eq!(result.summary.trade_count, 0);
        assert_eq!(result.summary.total_pnl, Decimal::ZERO);
        assert_eq!(result.summary.max_drawdown, Decimal::ZERO);
        assert_eq!(result.summary.sharpe_ratio, None);
    }

    #[test]
    fn test_decimal_sqrt() {
        let result = decimal_sqrt(dec!(4.0)).unwrap();
        assert!((result - dec!(2.0)).abs() < dec!(0.001));

        let result = decimal_sqrt(dec!(9.0)).unwrap();
        assert!((result - dec!(3.0)).abs() < dec!(0.001));

        assert_eq!(decimal_sqrt(dec!(0.0)), Some(Decimal::ZERO));
        assert_eq!(decimal_sqrt(dec!(-1.0)), None);
    }
}
