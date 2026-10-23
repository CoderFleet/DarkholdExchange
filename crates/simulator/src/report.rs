//! Machine-readable and Markdown results for the load baseline.

use std::fmt::Write as _;

use dx_types::MetricsSnapshot;
use serde::Serialize;

use crate::config::LoadConfig;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct OutcomeCounts {
    pub attempts: u64,
    pub ok: u64,
    pub rejected: u64,
    pub queue_full: u64,
    pub unavailable: u64,
    pub http_other: u64,
    pub unexpected: u64,
    pub transport_errors: u64,
}

impl OutcomeCounts {
    pub fn merge(&mut self, other: Self) {
        self.attempts += other.attempts;
        self.ok += other.ok;
        self.rejected += other.rejected;
        self.queue_full += other.queue_full;
        self.unavailable += other.unavailable;
        self.http_other += other.http_other;
        self.unexpected += other.unexpected;
        self.transport_errors += other.transport_errors;
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LatencySummary {
    pub samples: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
}

impl LatencySummary {
    pub fn from_samples(mut samples: Vec<u64>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_unstable();
        Self {
            samples: samples.len(),
            p50_us: percentile(&samples, 50.0),
            p95_us: percentile(&samples, 95.0),
            p99_us: percentile(&samples, 99.0),
            p999_us: percentile(&samples, 99.9),
            max_us: *samples.last().expect("non-empty samples"),
        }
    }
}

fn percentile(sorted: &[u64], percentile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let percentile = percentile.clamp(1.0, 100.0);
    let rank = ((percentile / 100.0) * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientReport {
    pub place: OutcomeCounts,
    pub cancel: OutcomeCounts,
    pub place_ack: LatencySummary,
    pub cancel_ack: LatencySummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProbeReport {
    pub enabled: bool,
    pub place_attempts: u64,
    pub place_ok: u64,
    pub cancel_attempts: u64,
    pub cancel_ok: u64,
    pub overloaded: u64,
    pub place_e2e: LatencySummary,
    pub cancel_e2e: LatencySummary,
    pub ws_messages: u64,
    pub ws_deltas: u64,
    pub ws_resync_required: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WsConnectionReport {
    pub messages: u64,
    pub deltas: u64,
    pub trades: u64,
    pub snapshots: u64,
    pub metrics: u64,
    pub resync_required: u64,
    pub seq_gaps: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub deltas_per_second: f64,
    pub bytes_per_second: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WsReport {
    pub connections: Vec<WsConnectionReport>,
    pub messages: u64,
    pub deltas: u64,
    pub trades: u64,
    pub snapshots: u64,
    pub metrics: u64,
    pub resync_required: u64,
    pub seq_gaps: u64,
    pub bytes: u64,
    pub deltas_per_second: f64,
    pub bytes_per_second: f64,
}

impl WsReport {
    pub fn from_connections(connections: Vec<WsConnectionReport>) -> Self {
        let mut report = Self {
            connections,
            ..Self::default()
        };
        let mut longest_elapsed_ms = 0u64;
        for connection in &report.connections {
            report.messages += connection.messages;
            report.deltas += connection.deltas;
            report.trades += connection.trades;
            report.snapshots += connection.snapshots;
            report.metrics += connection.metrics;
            report.resync_required += connection.resync_required;
            report.seq_gaps += connection.seq_gaps;
            report.bytes += connection.bytes;
            longest_elapsed_ms = longest_elapsed_ms.max(connection.elapsed_ms);
        }
        if longest_elapsed_ms > 0 {
            let seconds = longest_elapsed_ms as f64 / 1_000.0;
            report.deltas_per_second = report.deltas as f64 / seconds;
            report.bytes_per_second = report.bytes as f64 / seconds;
        }
        report
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayReport {
    pub before: MetricsSnapshot,
    pub after: MetricsSnapshot,
}

impl GatewayReport {
    pub fn seq_delta(&self) -> u64 {
        self.after.seq.saturating_sub(self.before.seq)
    }

    pub fn commands_delta(&self) -> u64 {
        self.after
            .commands_total
            .saturating_sub(self.before.commands_total)
    }

    pub fn rejected_delta(&self) -> u64 {
        self.after
            .rejected_total
            .saturating_sub(self.before.rejected_total)
    }

    pub fn slow_consumers_delta(&self) -> u64 {
        self.after
            .slow_consumers
            .saturating_sub(self.before.slow_consumers)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentReport {
    pub os: &'static str,
    pub arch: &'static str,
    pub available_parallelism: usize,
    pub gateway_queue_capacity: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OfferedLoad {
    pub placements: u64,
    pub commands: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AchievedLoad {
    pub placements_per_second: f64,
    pub commands_per_second: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadReport {
    pub config: LoadConfig,
    pub environment: EnvironmentReport,
    pub started_at_unix_ms: u64,
    pub duration_ms: u64,
    pub offered: OfferedLoad,
    pub achieved: AchievedLoad,
    pub client: ClientReport,
    pub probe: ProbeReport,
    pub ws: WsReport,
    pub gateway: GatewayReport,
    pub notes: Vec<String>,
}

impl LoadReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        writeln!(out, "## Environment").unwrap();
        writeln!(out).unwrap();
        writeln!(
            out,
            "- OS / arch: `{} / {}`",
            self.environment.os, self.environment.arch
        )
        .unwrap();
        writeln!(
            out,
            "- Available parallelism: `{}`",
            self.environment.available_parallelism
        )
        .unwrap();
        match self.environment.gateway_queue_capacity {
            Some(capacity) => writeln!(out, "- Gateway queue capacity: `{capacity}`").unwrap(),
            None => writeln!(out, "- Gateway queue capacity: `unknown`").unwrap(),
        }
        writeln!(
            out,
            "- Duration: `{:.3} s`",
            self.duration_ms as f64 / 1_000.0
        )
        .unwrap();
        writeln!(out).unwrap();

        writeln!(out, "## Configuration").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "| Setting | Value |").unwrap();
        writeln!(out, "|---|---:|").unwrap();
        writeln!(
            out,
            "| Target placements/s | {} |",
            self.config.placements_per_second
        )
        .unwrap();
        writeln!(out, "| Users | {} |", self.config.users).unwrap();
        writeln!(out, "| WS connections | {} |", self.config.ws_connections).unwrap();
        writeln!(
            out,
            "| Probe rate/s | {} |",
            self.config.probe_rate_per_second
        )
        .unwrap();
        writeln!(
            out,
            "| Main market / price | {} / {} ticks |",
            self.config.market_id, self.config.price_ticks
        )
        .unwrap();
        writeln!(
            out,
            "| Probe market / price | {} / {} ticks |",
            self.config.probe_market_id, self.config.probe_price_ticks
        )
        .unwrap();
        writeln!(out).unwrap();

        writeln!(out, "## Achieved").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "| Metric | Value |").unwrap();
        writeln!(out, "|---|---:|").unwrap();
        writeln!(
            out,
            "| Offered placements/s | {:.2} |",
            self.offered.placements as f64 / (self.duration_ms as f64 / 1_000.0)
        )
        .unwrap();
        writeln!(
            out,
            "| Achieved placements/s | {:.2} |",
            self.achieved.placements_per_second
        )
        .unwrap();
        writeln!(
            out,
            "| Achieved accepted commands/s | {:.2} |",
            self.achieved.commands_per_second
        )
        .unwrap();
        writeln!(
            out,
            "| Gateway commands | {} |",
            self.gateway.commands_delta()
        )
        .unwrap();
        writeln!(
            out,
            "| Gateway rejections | {} |",
            self.gateway.rejected_delta()
        )
        .unwrap();
        writeln!(out).unwrap();

        writeln!(out, "## Client latency (microseconds)").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "| Path | Samples | p50 | p95 | p99 | p99.9 | Max |").unwrap();
        writeln!(out, "|---|---:|---:|---:|---:|---:|---:|").unwrap();
        write_latency_row(&mut out, "Place acknowledgement", &self.client.place_ack);
        write_latency_row(&mut out, "Cancel acknowledgement", &self.client.cancel_ack);
        write_latency_row(&mut out, "Probe end-to-end (place)", &self.probe.place_e2e);
        write_latency_row(
            &mut out,
            "Probe end-to-end (cancel)",
            &self.probe.cancel_e2e,
        );
        writeln!(out).unwrap();

        writeln!(out, "## Outcomes").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "| Operation | Attempts | OK | Rejected | 429 | 503 | HTTP other | Unexpected | Transport |").unwrap();
        writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|").unwrap();
        write_outcome_row(&mut out, "Place", &self.client.place);
        write_outcome_row(&mut out, "Cancel", &self.client.cancel);
        writeln!(out).unwrap();

        writeln!(out, "## Probe").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "| Metric | Value |").unwrap();
        writeln!(out, "|---|---:|").unwrap();
        writeln!(out, "| Enabled | {} |", self.probe.enabled).unwrap();
        writeln!(out, "| Place attempts | {} |", self.probe.place_attempts).unwrap();
        writeln!(out, "| Place OK | {} |", self.probe.place_ok).unwrap();
        writeln!(out, "| Cancel attempts | {} |", self.probe.cancel_attempts).unwrap();
        writeln!(out, "| Cancel OK | {} |", self.probe.cancel_ok).unwrap();
        writeln!(out, "| Overloaded (429/503) | {} |", self.probe.overloaded).unwrap();
        writeln!(out, "| Probe WS messages | {} |", self.probe.ws_messages).unwrap();
        writeln!(out, "| Probe WS deltas | {} |", self.probe.ws_deltas).unwrap();
        writeln!(
            out,
            "| Probe WS resyncs | {} |",
            self.probe.ws_resync_required
        )
        .unwrap();
        writeln!(out).unwrap();

        writeln!(out, "## Gateway metrics").unwrap();
        writeln!(out).unwrap();
        writeln!(out, "| Metric | Before | After | Delta |").unwrap();
        writeln!(out, "|---|---:|---:|---:|").unwrap();
        writeln!(
            out,
            "| Global seq | {} | {} | {} |",
            self.gateway.before.seq,
            self.gateway.after.seq,
            self.gateway.seq_delta()
        )
        .unwrap();
        writeln!(
            out,
            "| Commands total | {} | {} | {} |",
            self.gateway.before.commands_total,
            self.gateway.after.commands_total,
            self.gateway.commands_delta()
        )
        .unwrap();
        writeln!(
            out,
            "| Rejected total | {} | {} | {} |",
            self.gateway.before.rejected_total,
            self.gateway.after.rejected_total,
            self.gateway.rejected_delta()
        )
        .unwrap();
        writeln!(
            out,
            "| Queue depth | {} | {} | - |",
            self.gateway.before.queue_depth, self.gateway.after.queue_depth
        )
        .unwrap();
        writeln!(
            out,
            "| Queue wait p99 (us) | {} | {} | - |",
            self.gateway.before.queue_wait_p99_us, self.gateway.after.queue_wait_p99_us
        )
        .unwrap();
        writeln!(
            out,
            "| Core p99 (us) | {} | {} | - |",
            self.gateway.before.core_p99_us, self.gateway.after.core_p99_us
        )
        .unwrap();
        writeln!(
            out,
            "| WS clients | {} | {} | - |",
            self.gateway.before.ws_clients, self.gateway.after.ws_clients
        )
        .unwrap();
        writeln!(
            out,
            "| Slow consumers | {} | {} | {} |",
            self.gateway.before.slow_consumers,
            self.gateway.after.slow_consumers,
            self.gateway.slow_consumers_delta()
        )
        .unwrap();
        writeln!(out).unwrap();

        writeln!(out, "## WebSocket fanout").unwrap();
        writeln!(out).unwrap();
        writeln!(
            out,
            "| Connection | Messages | Deltas | Trades | Resyncs | Seq gaps | Deltas/s | Bytes |"
        )
        .unwrap();
        writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|").unwrap();
        for (index, connection) in self.ws.connections.iter().enumerate() {
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {:.2} | {} |",
                index + 1,
                connection.messages,
                connection.deltas,
                connection.trades,
                connection.resync_required,
                connection.seq_gaps,
                connection.deltas_per_second,
                connection.bytes
            )
            .unwrap();
        }
        writeln!(
            out,
            "| **Total** | **{}** | **{}** | **{}** | **{}** | **{}** | **{:.2}** | **{}** |",
            self.ws.messages,
            self.ws.deltas,
            self.ws.trades,
            self.ws.resync_required,
            self.ws.seq_gaps,
            self.ws.deltas_per_second,
            self.ws.bytes
        )
        .unwrap();
        writeln!(out).unwrap();

        writeln!(out, "## Notes").unwrap();
        writeln!(out).unwrap();
        for note in &self.notes {
            writeln!(out, "- {note}").unwrap();
        }
        out
    }
}

fn write_latency_row(out: &mut String, label: &str, summary: &LatencySummary) {
    writeln!(
        out,
        "| {label} | {} | {} | {} | {} | {} | {} |",
        summary.samples,
        summary.p50_us,
        summary.p95_us,
        summary.p99_us,
        summary.p999_us,
        summary.max_us
    )
    .unwrap();
}

fn write_outcome_row(out: &mut String, label: &str, counts: &OutcomeCounts) {
    writeln!(
        out,
        "| {label} | {} | {} | {} | {} | {} | {} | {} | {} |",
        counts.attempts,
        counts.ok,
        counts.rejected,
        counts.queue_full,
        counts.unavailable,
        counts.http_other,
        counts.unexpected,
        counts.transport_errors
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let values = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&values, 50.0), 5);
        assert_eq!(percentile(&values, 95.0), 10);
        assert_eq!(percentile(&values, 100.0), 10);
        assert_eq!(percentile(&values, 1.0), 1);
    }

    #[test]
    fn latency_summary_handles_empty_and_populated_samples() {
        assert_eq!(LatencySummary::from_samples(Vec::new()).samples, 0);
        let summary = LatencySummary::from_samples(vec![400, 100, 300, 200]);
        assert_eq!(summary.samples, 4);
        assert_eq!(summary.p50_us, 200);
        assert_eq!(summary.max_us, 400);
    }

    #[test]
    fn outcome_counts_merge() {
        let mut left = OutcomeCounts {
            attempts: 1,
            ok: 1,
            ..OutcomeCounts::default()
        };
        left.merge(OutcomeCounts {
            attempts: 2,
            queue_full: 1,
            ..OutcomeCounts::default()
        });
        assert_eq!(left.attempts, 3);
        assert_eq!(left.ok, 1);
        assert_eq!(left.queue_full, 1);
    }
}
