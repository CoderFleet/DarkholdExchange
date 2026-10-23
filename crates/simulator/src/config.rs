//! Configuration for the repeatable gateway load baseline.

use std::path::PathBuf;
use std::str::FromStr;

use dx_types::{MarketId, Minor, Qty, Ticks, UserId};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workload {
    PlaceCancel,
    Fills,
}

impl FromStr for Workload {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "place-cancel" => Ok(Self::PlaceCancel),
            "fills" => Ok(Self::Fills),
            _ => Err(format!(
                "unknown workload `{value}`; expected `place-cancel` or `fills`"
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadConfig {
    pub base_url: String,
    pub workload: Workload,
    pub duration_secs: u64,
    pub placements_per_second: u64,
    pub fills_per_second: u64,
    pub fill_makers: usize,
    pub fill_takers: usize,
    pub fill_passes: Qty,
    pub fill_price_ticks: Ticks,
    pub users: usize,
    pub ws_connections: usize,
    pub probe_rate_per_second: u64,
    pub user_base: UserId,
    pub market_id: MarketId,
    pub price_ticks: Ticks,
    pub qty: Qty,
    pub probe_user: UserId,
    pub probe_market_id: MarketId,
    pub probe_price_ticks: Ticks,
    pub seed_cash: Minor,
    pub seed_passes: Qty,
    #[serde(skip)]
    pub json_out: Option<PathBuf>,
}

impl Default for LoadConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:18080".into(),
            workload: Workload::PlaceCancel,
            duration_secs: 10,
            placements_per_second: 1_000,
            fills_per_second: 1_000,
            fill_makers: 3_000,
            fill_takers: 3_000,
            fill_passes: 4,
            fill_price_ticks: 5_000,
            users: 32,
            ws_connections: 4,
            probe_rate_per_second: 10,
            user_base: 1_000,
            market_id: 1,
            price_ticks: 4_700,
            qty: 1,
            probe_user: 5,
            probe_market_id: 2,
            probe_price_ticks: 9_600,
            seed_cash: 10_000_000,
            seed_passes: 2,
            json_out: None,
        }
    }
}

impl LoadConfig {
    pub fn from_args<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = String>,
    {
        let mut config = Self::default();
        let mut args = args.into_iter();

        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--base-url" => config.base_url = next_value(&mut args, &flag)?,
                "--workload" => {
                    config.workload = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--duration-secs" => {
                    config.duration_secs = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--placements-per-second" => {
                    config.placements_per_second =
                        parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--fills-per-second" => {
                    config.fills_per_second = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--fill-makers" => {
                    config.fill_makers = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--fill-takers" => {
                    config.fill_takers = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--fill-passes" => {
                    config.fill_passes = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--fill-price-ticks" => {
                    config.fill_price_ticks = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--users" => config.users = parse_value(&flag, &next_value(&mut args, &flag)?)?,
                "--ws-connections" => {
                    config.ws_connections = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--probe-rate" => {
                    config.probe_rate_per_second =
                        parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--user-base" => {
                    config.user_base = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--market-id" => {
                    config.market_id = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--price-ticks" => {
                    config.price_ticks = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--qty" => config.qty = parse_value(&flag, &next_value(&mut args, &flag)?)?,
                "--probe-user" => {
                    config.probe_user = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--probe-market-id" => {
                    config.probe_market_id = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--probe-price-ticks" => {
                    config.probe_price_ticks = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--seed-cash" => {
                    config.seed_cash = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--seed-passes" => {
                    config.seed_passes = parse_value(&flag, &next_value(&mut args, &flag)?)?
                }
                "--json-out" => {
                    config.json_out = Some(PathBuf::from(next_value(&mut args, &flag)?))
                }
                "--help" | "-h" => return Err(Self::usage().into()),
                _ => return Err(format!("unknown argument `{flag}`\n\n{}", Self::usage())),
            }
        }

        config.normalize();
        config.validate()?;
        Ok(config)
    }

    fn normalize(&mut self) {
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
    }

    fn validate(&self) -> Result<(), String> {
        if !self.base_url.starts_with("http://") || self.base_url.len() <= "http://".len() {
            return Err("--base-url must be a local http:// URL".into());
        }
        if self.duration_secs == 0 || self.duration_secs > 3_600 {
            return Err("--duration-secs must be between 1 and 3600".into());
        }
        if self.placements_per_second == 0 || self.placements_per_second > 100_000 {
            return Err("--placements-per-second must be between 1 and 100000".into());
        }
        if self.workload == Workload::Fills {
            if self.fills_per_second == 0 || self.fills_per_second > 100_000 {
                return Err("--fills-per-second must be between 1 and 100000".into());
            }
            if self.fill_makers == 0 || self.fill_makers > 10_000 {
                return Err("--fill-makers must be between 1 and 10000".into());
            }
            if self.fill_takers == 0 || self.fill_takers > 10_000 {
                return Err("--fill-takers must be between 1 and 10000".into());
            }
            if self.fill_makers + self.fill_takers > 10_000 {
                return Err("--fill-makers + --fill-takers must be at most 10000".into());
            }
            if self.fill_passes == 0 || self.fill_passes > 100 {
                return Err("--fill-passes must be between 1 and 100".into());
            }
            if self.fill_price_ticks == 0 {
                return Err("--fill-price-ticks must be positive".into());
            }
            if self.qty != 1 {
                return Err("--workload fills currently requires --qty 1".into());
            }
            let Some(expected_fills) = self.fills_per_second.checked_mul(self.duration_secs) else {
                return Err("--fills-per-second * --duration-secs overflows".into());
            };
            let Some(maker_capacity) = (self.fill_makers as u64).checked_mul(self.fill_passes)
            else {
                return Err("--fill-makers * --fill-passes overflows".into());
            };
            let Some(taker_capacity) = (self.fill_takers as u64).checked_mul(self.fill_passes)
            else {
                return Err("--fill-takers * --fill-passes overflows".into());
            };
            if maker_capacity < expected_fills {
                return Err(format!(
                    "not enough maker capacity: need {expected_fills} fills, have {maker_capacity}; raise --fill-makers or --fill-passes"
                ));
            }
            if taker_capacity < expected_fills {
                return Err(format!(
                    "not enough taker capacity: need {expected_fills} fills, have {taker_capacity}; raise --fill-takers or --fill-passes"
                ));
            }
        }
        if self.users == 0 || self.users > 10_000 {
            return Err("--users must be between 1 and 10000".into());
        }
        if self.ws_connections == 0 || self.ws_connections > 64 {
            return Err("--ws-connections must be between 1 and 64".into());
        }
        if self.probe_rate_per_second > 10_000 {
            return Err("--probe-rate must be at most 10000".into());
        }
        if self.user_base < 1_000 {
            return Err(
                "--user-base must be at least 1000 so seeded demo users are untouched".into(),
            );
        }
        if self.user_base.checked_add(self.users as u64).is_none() {
            return Err("--user-base + --users overflows user id space".into());
        }
        if self
            .user_base
            .checked_add((self.fill_makers + self.fill_takers) as u64)
            .is_none()
        {
            return Err("--user-base + fill users overflows user id space".into());
        }
        if self.market_id == 0 {
            return Err("--market-id must be positive".into());
        }
        if self.price_ticks == 0 {
            return Err("--price-ticks must be positive".into());
        }
        if self.qty == 0 {
            return Err("--qty must be positive".into());
        }
        if self.probe_user == 0 {
            return Err("--probe-user must be positive".into());
        }
        if self.probe_market_id == 0 {
            return Err("--probe-market-id must be positive".into());
        }
        if self.probe_rate_per_second > 0 && self.probe_price_ticks == 0 {
            return Err("--probe-price-ticks must be positive when the probe is enabled".into());
        }
        if self.seed_cash <= 0 {
            return Err("--seed-cash must be positive".into());
        }
        if self.seed_passes == 0 {
            return Err("--seed-passes must be positive".into());
        }
        Ok(())
    }

    pub fn usage() -> &'static str {
        "dx-load - Darkhold Exchange gateway load baseline\n\
         \n\
         Usage:\n\
           dx-load [flags]\n\
         \n\
         Flags:\n\
           --base-url <url>                default http://127.0.0.1:18080\n\
           --workload <name>               place-cancel (default) or fills\n\
           --duration-secs <n>             default 10\n\
           --placements-per-second <n>     default 1000 (place-cancel)\n\
           --fills-per-second <n>          default 1000 (fills)\n\
           --fill-makers <n>               default 3000 (fills)\n\
           --fill-takers <n>               default 3000 (fills)\n\
           --fill-passes <qty>             default 4 (fills)\n\
           --fill-price-ticks <ticks>      default 5000 (fills)\n\
           --users <n>                     default 32 (place-cancel users / fill workers)\n\
           --ws-connections <n>            default 4\n\
           --probe-rate <n>                default 10 (0 disables)\n\
           --user-base <id>                default 1000\n\
           --market-id <id>                default 1\n\
           --price-ticks <ticks>           default 4700\n\
           --qty <qty>                     default 1\n\
           --probe-user <id>               default 5\n\
           --probe-market-id <id>          default 2\n\
           --probe-price-ticks <ticks>     default 9600\n\
           --seed-cash <minor>             default 10000000\n\
           --seed-passes <qty>             default 2\n\
           --json-out <path>               write the JSON report\n"
    }
}

fn next_value<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| format!("missing value for `{flag}`\n\n{}", LoadConfig::usage()))
}

fn parse_value<T>(flag: &str, value: &str) -> Result<T, String>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| format!("invalid value for `{flag}`: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        LoadConfig::default().validate().unwrap();
    }

    #[test]
    fn parses_overrides() {
        let config = LoadConfig::from_args(
            [
                "--duration-secs",
                "3",
                "--placements-per-second",
                "250",
                "--users",
                "8",
                "--ws-connections",
                "1",
                "--probe-rate",
                "0",
                "--json-out",
                "bench/results/test.json",
            ]
            .map(String::from),
        )
        .unwrap();

        assert_eq!(config.duration_secs, 3);
        assert_eq!(config.placements_per_second, 250);
        assert_eq!(config.users, 8);
        assert_eq!(config.ws_connections, 1);
        assert_eq!(config.probe_rate_per_second, 0);
        assert_eq!(
            config.json_out,
            Some(PathBuf::from("bench/results/test.json"))
        );
    }

    #[test]
    fn rejects_zero_rate() {
        let error =
            LoadConfig::from_args(["--placements-per-second", "0"].map(String::from)).unwrap_err();
        assert!(error.contains("--placements-per-second"));
    }

    #[test]
    fn rejects_demo_user_collision() {
        let error = LoadConfig::from_args(["--user-base", "5"].map(String::from)).unwrap_err();
        assert!(error.contains("--user-base"));
    }

    #[test]
    fn parses_fill_workload() {
        let config = LoadConfig::from_args(
            [
                "--workload",
                "fills",
                "--fills-per-second",
                "1100",
                "--fill-makers",
                "3000",
                "--fill-takers",
                "3000",
                "--fill-passes",
                "4",
                "--fill-price-ticks",
                "5000",
            ]
            .map(String::from),
        )
        .unwrap();

        assert_eq!(config.workload, Workload::Fills);
        assert_eq!(config.fills_per_second, 1_100);
        assert_eq!(config.fill_makers, 3_000);
        assert_eq!(config.fill_takers, 3_000);
        assert_eq!(config.fill_passes, 4);
        assert_eq!(config.fill_price_ticks, 5_000);
    }

    #[test]
    fn rejects_insufficient_fill_capacity() {
        let error = LoadConfig::from_args(
            [
                "--workload",
                "fills",
                "--fills-per-second",
                "1100",
                "--duration-secs",
                "10",
                "--fill-makers",
                "100",
                "--fill-takers",
                "100",
                "--fill-passes",
                "4",
            ]
            .map(String::from),
        )
        .unwrap_err();
        assert!(error.contains("not enough maker capacity"));
    }

    #[test]
    fn rejects_non_unit_fill_qty() {
        let error = LoadConfig::from_args(["--workload", "fills", "--qty", "2"].map(String::from))
            .unwrap_err();
        assert!(error.contains("--qty 1"));
    }
}
