//! 本机系统指标采样：CPU、内存、网络吞吐。
//!
//! 采样器保存上次的时间片与网卡字节数，调用方按固定间隔取差值即可得到速率，
//! 不额外起线程；读不到 `/proc` 的平台返回 `supported = false`，界面据此隐藏指标。

use std::time::Instant;

use serde::Serialize;

/// 一次采样结果。
///
/// 无参数
/// 核心流程：字段全部用前端直接可展示的单位，避免在界面上再做换算。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMetrics {
    /// CPU 使用率，0 到 100。
    pub cpu_percent: f32,
    /// 内存使用率，0 到 100。
    pub mem_percent: f32,
    /// 已用内存（MiB）。
    pub mem_used_mb: u64,
    /// 总内存（MiB）。
    pub mem_total_mb: u64,
    /// 下行速率（KB/s）。
    pub rx_kbps: f64,
    /// 上行速率（KB/s）。
    pub tx_kbps: f64,
    /// 当前平台是否支持采样。
    pub supported: bool,
}

/// 采样器。
///
/// 无参数
/// 核心流程：保留上一次的 CPU 时间片、网卡累计字节与采样时刻，
/// 每次采样用差值除以时间间隔算出速率；首次采样没有基线，速率返回 0。
pub struct MetricsSampler {
    last_cpu: Option<(u64, u64)>,
    last_net: Option<(u64, u64, Instant)>,
}

impl Default for MetricsSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsSampler {
    /// 创建空采样器。
    ///
    /// 参数：无
    /// 核心流程：基线留空，第一次采样只负责建立基线。
    pub fn new() -> Self {
        Self {
            last_cpu: None,
            last_net: None,
        }
    }

    /// 采一次样。
    ///
    /// 参数：无
    /// 核心流程：读 `/proc` -> 用与上次的差值算 CPU 与网速 -> 更新基线 -> 返回结果。
    #[cfg(target_os = "linux")]
    pub fn sample(&mut self) -> SystemMetrics {
        let mut metrics = SystemMetrics {
            supported: true,
            ..Default::default()
        };

        if let Some((idle, total)) = read_cpu_times() {
            if let Some((last_idle, last_total)) = self.last_cpu {
                let total_delta = total.saturating_sub(last_total);
                let idle_delta = idle.saturating_sub(last_idle);
                if total_delta > 0 {
                    let busy = total_delta.saturating_sub(idle_delta);
                    metrics.cpu_percent = (busy as f64 * 100.0 / total_delta as f64) as f32;
                }
            }
            self.last_cpu = Some((idle, total));
        }

        if let Some((total_mb, used_mb)) = read_memory() {
            metrics.mem_total_mb = total_mb;
            metrics.mem_used_mb = used_mb;
            if total_mb > 0 {
                metrics.mem_percent = (used_mb as f64 * 100.0 / total_mb as f64) as f32;
            }
        }

        if let Some((rx, tx)) = read_net_bytes() {
            let now = Instant::now();
            if let Some((last_rx, last_tx, last_at)) = self.last_net {
                let seconds = now.duration_since(last_at).as_secs_f64();
                if seconds > 0.05 {
                    let rx_delta = rx.saturating_sub(last_rx) as f64;
                    let tx_delta = tx.saturating_sub(last_tx) as f64;
                    metrics.rx_kbps = rx_delta / 1024.0 / seconds;
                    metrics.tx_kbps = tx_delta / 1024.0 / seconds;
                }
            }
            self.last_net = Some((rx, tx, now));
        }

        metrics
    }

    /// 采一次样（非 Linux 平台）。
    ///
    /// 参数：无
    /// 核心流程：直接返回 `supported = false`，界面据此隐藏指标区，
    /// 保证 Windows 与 macOS 能编译且不显示假数据。
    #[cfg(not(target_os = "linux"))]
    pub fn sample(&mut self) -> SystemMetrics {
        SystemMetrics::default()
    }
}

/// 读取 CPU 累计时间片。
///
/// 参数：无
/// 核心流程：取 `/proc/stat` 第一行的 idle 与全部时间之和，解析失败返回 None。
#[cfg(target_os = "linux")]
fn read_cpu_times() -> Option<(u64, u64)> {
    let content = std::fs::read_to_string("/proc/stat").ok()?;
    let line = content.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.filter_map(|item| item.parse::<u64>().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    let idle = values.get(3).copied().unwrap_or(0) + values.get(4).copied().unwrap_or(0);
    let total: u64 = values.iter().sum();
    Some((idle, total))
}

/// 读取内存总量与已用量（MiB）。
///
/// 参数：无
/// 核心流程：用 `MemTotal - MemAvailable` 算已用，比单看 `MemFree` 更贴近实际占用。
#[cfg(target_os = "linux")]
fn read_memory() -> Option<(u64, u64)> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = 0u64;
    let mut available_kb = 0u64;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = parse_kb(rest);
        }
    }
    if total_kb == 0 {
        return None;
    }
    let used_kb = total_kb.saturating_sub(available_kb);
    Some((total_kb / 1024, used_kb / 1024))
}

/// 从 `/proc/meminfo` 的剩余部分解析出 kB 数。
///
/// 参数：raw 为冒号后的文本，形如 ` 16316132 kB`
/// 核心流程：取第一个数字，解析失败按 0 处理。
#[cfg(target_os = "linux")]
fn parse_kb(raw: &str) -> u64 {
    raw.split_whitespace()
        .next()
        .and_then(|item| item.parse::<u64>().ok())
        .unwrap_or(0)
}

/// 汇总非回环网卡的累计收发字节。
///
/// 参数：无
/// 核心流程：跳过 `lo`，其余网卡逐个累加 rx/tx 字节，解析失败的行直接略过。
#[cfg(target_os = "linux")]
fn read_net_bytes() -> Option<(u64, u64)> {
    let content = std::fs::read_to_string("/proc/net/dev").ok()?;
    let mut rx_total = 0u64;
    let mut tx_total = 0u64;
    for line in content.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name == "lo" || name == "Inter-|" {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        rx_total += fields[0].parse::<u64>().unwrap_or(0);
        tx_total += fields[8].parse::<u64>().unwrap_or(0);
    }
    Some((rx_total, tx_total))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn first_sample_reports_supported_with_reasonable_values() {
        let mut sampler = MetricsSampler::new();
        let metrics = sampler.sample();
        assert!(metrics.supported);
        assert!(metrics.mem_total_mb > 0, "应该读到物理内存总量");
        assert!(metrics.mem_percent <= 100.0);
        assert!(metrics.cpu_percent <= 100.0);
        // 首次采样没有网络基线，速率必须为 0，不能给出假数据
        assert_eq!(metrics.rx_kbps, 0.0);
        assert_eq!(metrics.tx_kbps, 0.0);
    }

    #[test]
    fn second_sample_stays_within_range() {
        let mut sampler = MetricsSampler::new();
        let _ = sampler.sample();
        std::thread::sleep(std::time::Duration::from_millis(80));
        let metrics = sampler.sample();
        assert!(metrics.cpu_percent >= 0.0 && metrics.cpu_percent <= 100.0);
        assert!(metrics.rx_kbps >= 0.0 && metrics.tx_kbps >= 0.0);
    }
}
