//! 远端系统指标采样：在已建立的 SSH 连接上执行一段 POSIX 脚本，
//! 取回远端主机的 CPU、内存与网速。
//!
//! 采样在远端脚本内一次完成（脚本自己采两次算差值），后端不保存任何基线，
//! 因此不同终端、不同主机之间互不干扰；采样失败一律报错，
//! 不会回落成本机数据，避免界面把「本机」的数字挂在「远端」标签下。

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::metrics::SystemMetrics;
use crate::ssh::SshConnection;

/// 远端采样脚本（一次性 exec 执行）。
///
/// 无参数
/// 核心流程：先确认三份 /proc 文件可读（不可读就直接以退出码 3 结束）→
/// 读第一次 CPU 与网卡累计值 → sleep 0.3 秒 → 再读一次 → 用 awk 算差值，
/// 先输出一行：`CPU <百分比> MEM <总内存kB> <可用内存kB> NET <下行KB/s> <上行KB/s>`；
/// 之后每块 GPU 再单独一行：`GPU <名称> <利用率> <已用MiB> <总量MiB> <温度或->`；
/// 若一条 `GPU` 行都没产出（驱动没装、nouveau 也不暴露 /sys 计数），
/// 再退到第三级，用 `lspci` 输出 `GPUX <名称> 0 0 0 -`：只报型号、数值全 0。
///
/// 第三级的名称先过一遍 `gcs`（就是那段 sed）：`lspci` 给的是
/// `NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1)` 这种带厂商前缀和版次的全文，
/// 前端一块卡两三百像素根本放不下，必须精简成 `GP107 [GeForce GTX 1050 Ti]`；
/// 只动 lspci 兜底这一支，nvidia-smi 与 /sys 给的干净名称原样保留。
/// 精简规则：去掉 `bus:dev.fn` 与设备类别前缀 → 去掉 `(rev xx)` →
/// 去掉常见厂商前缀（AMD 那条 `[AMD/ATI]` 假前缀必须单独先处理，否则会被当成型号的一部分）→
/// 最后一条通用规则兜住 `Corporation|Corp.|Inc.|Ltd.|Co.` 这类没列举到的厂商；
/// 通用规则里的 `[^[]*` 保证不越过第一个 `[`，`[...]` 里的型号原文一律不动。
///
/// GPU 段完全可选：没有 nvidia-smi 且没有 /sys 显卡计数时不输出 `GPU` 行，
/// 连 lspci 都没有才彻底不输出，此时前端显示「未检测到 GPU」；
/// 整段用 `2>/dev/null` 与结尾的 `true` 兜底，绝不影响前半段 CPU/MEM/NET 的采集。
const REMOTE_METRICS_SCRIPT: &str = r#"if [ ! -r /proc/stat ] || [ ! -r /proc/meminfo ] || [ ! -r /proc/net/dev ]; then echo "PROC_UNAVAILABLE" >&2; exit 3; fi; s1=$(head -1 /proc/stat); n1=$(awk -F'[: ]+' '!/lo/ && NF>10 {rx+=$3; tx+=$11} END{print rx+0, tx+0}' /proc/net/dev); sleep 0.3; s2=$(head -1 /proc/stat); n2=$(awk -F'[: ]+' '!/lo/ && NF>10 {rx+=$3; tx+=$11} END{print rx+0, tx+0}' /proc/net/dev); awk -v a="$s1" -v b="$s2" 'BEGIN{split(a,x);split(b,y);ta=0;tb=0;for(i=2;i<=9;i++){ta+=x[i];tb+=y[i]}ia=x[5]+x[6];ib=y[5]+y[6];dt=tb-ta;di=ib-ia;if(dt<=0)p=0;else p=(dt-di)*100/dt;printf "CPU %.1f ",p}'; awk '/MemTotal/{t=$2}/MemAvailable/{a=$2}END{printf "MEM %d %d ",t,a}' /proc/meminfo; awk -v a="$n1" -v b="$n2" 'BEGIN{split(a,x);split(b,y);d1=y[1]-x[1];d2=y[2]-x[2];if(d1<0)d1=0;if(d2<0)d2=0;printf "NET %.1f %.1f\n",d1/1024/0.3,d2/1024/0.3}'; gz() { case "$1" in ''|*[!0-9]*) printf '0' ;; *) printf '%s' "$1" ;; esac; }; { gnv=""; if command -v nvidia-smi >/dev/null 2>&1; then gnv=$(nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu --format=csv,noheader,nounits 2>/dev/null); fi; if [ -n "$gnv" ]; then printf '%s\n' "$gnv" | while IFS=, read -r gn gu gm gt gx; do gn=$(printf '%s' "$gn" | sed 's/^ *//; s/ *$//'); [ -n "$gn" ] || gn=GPU; gu=$(gz "$(printf '%s' "$gu" | tr -d ' ')"); gm=$(gz "$(printf '%s' "$gm" | tr -d ' ')"); gt=$(gz "$(printf '%s' "$gt" | tr -d ' ')"); gx=$(printf '%s' "$gx" | tr -d ' '); gt2=-; case "$gx" in ''|*[!0-9]*) : ;; *) gt2=$gx ;; esac; printf 'GPU %s %s %s %s %s\n' "$gn" "$gu" "$gm" "$gt" "$gt2"; done; else gsys=0; for gf in /sys/class/drm/card[0-9]*/device/gpu_busy_percent; do if [ -r "$gf" ]; then gsys=1; fi; done; if [ "$gsys" = 1 ]; then for gf in /sys/class/drm/card[0-9]*/device/gpu_busy_percent; do [ -r "$gf" ] || continue; gd=${gf%/gpu_busy_percent}; gu=$(gz "$(cat "$gf" 2>/dev/null)"); gm=0; gt=0; [ -r "$gd/mem_info_vram_used" ] && gm=$(awk -v b="$(cat "$gd/mem_info_vram_used" 2>/dev/null)" 'BEGIN{printf "%d",b/1048576}'); [ -r "$gd/mem_info_vram_total" ] && gt=$(awk -v b="$(cat "$gd/mem_info_vram_total" 2>/dev/null)" 'BEGIN{printf "%d",b/1048576}'); gn=GPU; [ -r "$gd/product_name" ] && gn=$(cat "$gd/product_name" 2>/dev/null); [ -n "$gn" ] || gn=GPU; printf 'GPU %s %s %s %s -\n' "$gn" "$gu" "$gm" "$gt"; done; else gcs='s/^[0-9A-Fa-f:.]*[[:space:]]*//;s/^[^:]*:[[:space:]]*//;s/^[[:space:]]*//;s/[[:space:]]*$//;s/[[:space:]]*(rev [^)]*)//;s/[[:space:]]*$//;s/^Advanced Micro Devices, Inc\. \[AMD\/ATI\] //;s/^Advanced Micro Devices, Inc\. \[AMD\] //;s/^Advanced Micro Devices, Inc\. //;s/^NVIDIA Corporation //;s/^Intel Corporation //;s/^Red Hat, Inc\. //;s/^[^[]*\(Corporation\|Corp\.\|Inc\.\|Ltd\.\|Co\.\)[[:space:]]*//;s/^[[:space:]]*//'; lspci 2>/dev/null | grep -Ei 'vga|3d|display' | sed -e "$gcs" | while IFS= read -r gl; do [ -n "$gl" ] || continue; printf 'GPUX %s 0 0 0 -\n' "$gl"; done; fi; fi; } 2>/dev/null; true"#;

/// 远端脚本输出的接收上限。
///
/// 参数：无
/// 核心流程：正常输出只有几十字节，留 4KB 余量既能容下异常回声，
/// 又不会让一条命令把内存占满。
const REMOTE_METRICS_MAX_BYTES: usize = 4096;

/// 远端口径标记。
const REMOTE_SCOPE: &str = "remote";

/// 单块 GPU 的采样结果。
///
/// 无参数
/// 核心流程：字段全部是前端可直接展示的单位；取不到的数值退化成 0，
/// 温度取不到用 `None`，这样前端不需要为「缺失」单独分支。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GpuMetrics {
    /// GPU 名称，取不到时为 `"GPU"`。
    pub name: String,
    /// 利用率，0 到 100。
    pub util_percent: f32,
    /// 已用显存（MiB），取不到为 0。
    pub mem_used_mb: u64,
    /// 显存总量（MiB），取不到为 0。
    pub mem_total_mb: u64,
    /// 温度（摄氏度），取不到为 `None`。
    pub temp_c: Option<f32>,
    /// 能否采到指标：`false` 表示只从 lspci 认出了型号（没装驱动、nouveau 也不给计数），
    /// 此时利用率/显存/温度一律为 0/0/0/None，前端只展示型号，不展示这些假数字。
    pub measurable: bool,
}

/// 一次远端采样结果。
///
/// 无参数
/// 核心流程：指标本身与「采自哪台主机、属于哪条口径」一起返回，
/// 前端据此在同一条曲线上区分本机与远端，不会张冠李戴。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMetrics {
    /// 远端指标本体，字段单位与 `SystemMetrics` 一致。
    pub metrics: SystemMetrics,
    /// 该主机上的 GPU 列表；没有 GPU 或采集失败时为空数组。
    pub gpus: Vec<GpuMetrics>,
    /// 被采样的主机地址。
    pub host: String,
    /// 数据口径，固定为 `"remote"`。
    pub scope: String,
}

/// `df` 输出的接收上限。
///
/// 参数：无
/// 核心流程：正常只有表头加一行，留 8 KiB 是为了容纳 df 的告警、
/// 多条挂载点以及路径过长时的折行输出。
const DISK_USAGE_MAX_BYTES: usize = 8192;

/// 一次远端磁盘占用查询结果。
///
/// 无参数
/// 核心流程：容量统一换算成字节，使用率单独算出来给前端画进度条，
/// 同时带上挂载点，好让用户知道统计的是哪个分区。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    /// 已用字节数。
    pub used_bytes: u64,
    /// 总容量字节数。
    pub total_bytes: u64,
    /// 可用字节数（即 df 的 Available 列）。
    pub free_bytes: u64,
    /// 使用率，0 到 100。
    pub percent: f32,
    /// 该路径所在的挂载点。
    pub mount: String,
}

/// 查询远端某个路径所在文件系统的磁盘占用。
///
/// 参数：connection 为该主机上已认证的 SSH 连接，path 为远端路径
/// 核心流程：拼一条 POSIX 口径的 df 命令（`-P` 不折行、`-k` 以 1024 字节为单位）
/// → 用 `--` 让以横线开头的路径也当普通参数，老版本 df 不认 `--` 时回退到不带 `--` 的写法 →
/// 取输出最后一行解析成 DiskUsage；df 跑不起来或解析不出时，
/// 改用 PowerShell 兜底（Windows 远端没有 df），两条路都不行才报错。
/// 路径不存在或解析不出时直接报错，绝不返回 0 值让界面显示一个假数字。
pub fn disk_usage(connection: &SshConnection, path: &str) -> CoreResult<DiskUsage> {
    let target = path.trim();
    if target.is_empty() {
        return Err(CoreError::Validation("查询磁盘占用需要指定远端路径".into()));
    }
    let quoted = shell_quote(target);
    let command = format!("df -Pk -- {quoted} 2>/dev/null || df -Pk {quoted}");
    let df_error = match connection.exec(&command, DISK_USAGE_MAX_BYTES) {
        Ok(raw) => match parse_df_output(&raw) {
            Ok(usage) => return Ok(usage),
            Err(err) => err,
        },
        Err(err) => {
            CoreError::Validation(format!("远端磁盘查询失败（路径 {target}）：{err}"))
        }
    };

    // df 走不通 -> 远端很可能是 Windows，换 PowerShell 再试一次
    match windows_disk_usage(connection, target) {
        Ok(usage) => Ok(usage),
        Err(fallback) => Err(annotate(
            df_error,
            &format!("已尝试 Windows 兜底：{fallback}"),
        )),
    }
}

/// 用 PowerShell 查磁盘占用（Windows 远端）。
///
/// 参数：connection 为该主机上已认证的 SSH 连接，path 为远端路径
/// 核心流程：按盘符前缀匹配 `Get-PSDrive` 里的文件系统盘 → 打印 `已用 可用` 两列字节数 →
/// 解析成 DiskUsage；匹配不到盘符时脚本以退出码 4 结束，由 exec 转成错误。
fn windows_disk_usage(connection: &SshConnection, path: &str) -> CoreResult<DiskUsage> {
    let raw = connection.exec(&windows_disk_command(path), DISK_USAGE_MAX_BYTES)?;
    parse_windows_disk_output(&raw, path)
}

/// 拼 PowerShell 查询命令。
///
/// 参数：path 为远端路径
/// 核心流程：路径里自带的单引号在 PowerShell 单引号串里翻倍转义；
/// 输出用 `ToString() + ' '` 拼两列而不是双引号插值，
/// 这样整条 `-Command` 参数只含一对外层双引号，cmd.exe 与 PowerShell 都不会被内层引号绕晕。
fn windows_disk_command(path: &str) -> String {
    let escaped = path.replace('\'', "''");
    let script = format!(
        "$p='{escaped}'; $d=Get-PSDrive -PSProvider FileSystem | \
Where-Object {{ $p -like ($_.Root + '*') }} | Select-Object -First 1; \
if($d){{ $d.Used.ToString() + ' ' + $d.Free.ToString() }} else {{ exit 4 }}"
    );
    format!("powershell -NoProfile -Command \"{script}\"")
}

/// 解析 PowerShell 兜底输出。
///
/// 参数：raw 为脚本输出（`已用字节 可用字节` 两列），path 为查询路径（用于取盘符）
/// 核心流程：取最后一行非空文本 → 必须正好两列 → 两列都按 u64 解析 →
/// total = used + free，percent 由 used/total 现算；缺列、非数字、总量为 0 都报错。
fn parse_windows_disk_output(raw: &str, path: &str) -> CoreResult<DiskUsage> {
    let line = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .ok_or_else(|| windows_parse_error("PowerShell 没有任何输出", raw))?;

    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() != 2 {
        return Err(windows_parse_error(
            &format!("最后一行有 {} 列，不是 2 列", fields.len()),
            raw,
        ));
    }

    let used_bytes = fields[0]
        .parse::<u64>()
        .map_err(|_| windows_parse_error(&format!("已用字节不是数字：{}", fields[0]), raw))?;
    let free_bytes = fields[1]
        .parse::<u64>()
        .map_err(|_| windows_parse_error(&format!("可用字节不是数字：{}", fields[1]), raw))?;
    let total_bytes = used_bytes.saturating_add(free_bytes);
    if total_bytes == 0 {
        return Err(windows_parse_error("总容量为 0", raw));
    }

    // Windows 的挂载点就是盘符：C:\Users\x -> C:
    let mount = windows_mount_of(path);
    let percent = (used_bytes as f64 * 100.0 / total_bytes as f64) as f32;
    Ok(DiskUsage {
        used_bytes,
        total_bytes,
        free_bytes,
        percent: percent.clamp(0.0, 100.0),
        mount,
    })
}

/// 取 Windows 路径的盘符。
///
/// 参数：path 为远端路径
/// 核心流程：首字符是 ASCII 字母且第二个字符是 `:` 时取前两个字节当盘符；
/// 其它形态（UNC 路径、相对路径）返回空串，前端照旧显示但不会指错盘。
fn windows_mount_of(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        // 前两个字节都是 ASCII，按字节切片不会切坏多字节字符
        return path[..2].to_string();
    }
    String::new()
}

/// 构造 PowerShell 兜底的解析错误。
///
/// 参数：reason 为失败原因，raw 为原始输出
/// 核心流程：与 df 的错误一样带一段原文片段，便于排查又不刷屏。
fn windows_parse_error(reason: &str, raw: &str) -> CoreError {
    let snippet: String = raw.trim().chars().take(160).collect();
    CoreError::Validation(format!(
        "Windows 磁盘信息解析失败（{reason}）：{snippet:?}"
    ))
}

/// 在原错误后面补一句旁注。
///
/// 参数：err 为原始错误（df 那一路），note 为要追加的说明
/// 核心流程：只处理携带自由文本的校验错误，其余分类原样返回，
/// 这样「df 的原始报错」仍然是主信息，兜底尝试只作为附注。
fn annotate(err: CoreError, note: &str) -> CoreError {
    match err {
        CoreError::Validation(message) => CoreError::Validation(format!("{message}（{note}）")),
        other => other,
    }
}

/// 给远端 shell 的参数加单引号。
///
/// 参数：value 为待转义的远端路径
/// 核心流程：单引号内的字符全部按字面量处理，路径里自带的单引号替换成
/// `'\''`（先闭合、再转义、再重开），这样带空格、分号、`$` 的路径不会被拆开执行。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// 解析 `df -Pk` 的输出。
///
/// 参数：raw 为 df 的完整输出（含表头，可能有多条挂载点或折行）
/// 核心流程：丢掉空行后取最后一行（df 的结论行总在末尾，表头与折行都不用管）→
/// 按空白切成至少 6 列（Filesystem 1024-blocks Used Available Capacity Mounted）→
/// 把 KiB 换算成字节；percent 用 used/total 现算，不采信 df 的 Capacity 列，
/// 免得被它的四舍五入带偏。缺列、非数字、总容量为 0 都直接报错。
fn parse_df_output(raw: &str) -> CoreResult<DiskUsage> {
    let line = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .ok_or_else(|| parse_error("df 没有任何输出", raw))?;

    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 6 {
        return Err(parse_error(
            &format!("最后一行只有 {} 列，少于 6 列", fields.len()),
            raw,
        ));
    }

    let total_kb = df_kib(fields[1], "总容量（1024-blocks）", raw)?;
    let used_kb = df_kib(fields[2], "已用（Used）", raw)?;
    let free_kb = df_kib(fields[3], "可用（Available）", raw)?;
    if total_kb == 0 {
        return Err(parse_error("总容量为 0", raw));
    }

    // 挂载点可能带空格 -> 第 6 列往后的都算挂载点
    let mount = fields[5..].join(" ");
    let percent = (used_kb as f64 * 100.0 / total_kb as f64) as f32;
    Ok(DiskUsage {
        used_bytes: used_kb.saturating_mul(1024),
        total_bytes: total_kb.saturating_mul(1024),
        free_bytes: free_kb.saturating_mul(1024),
        percent: percent.clamp(0.0, 100.0),
        mount,
    })
}

/// 取出一个 df 容量列（单位 KiB）。
///
/// 参数：token 为待解析字段，name 为字段中文名（用于报错），raw 为原始输出
/// 核心流程：按 u64 解析，失败转成带原文的错误，绝不 panic。
fn df_kib(token: &str, name: &str, raw: &str) -> CoreResult<u64> {
    token
        .parse::<u64>()
        .map_err(|_| parse_error(&format!("{name}不是数字：{token}"), raw))
}

/// 采一次远端主机的系统指标。
///
/// 参数：connection 为该主机上已认证的 SSH 连接，host 为主机地址（仅用于标注）
/// 核心流程：在连接上一次性执行采样脚本 → 解析成 `SystemMetrics` →
/// 打上 `scope = "remote"` 返回；任何一步失败都直接返回错误，不回落本机数据。
pub fn sample_remote(connection: &SshConnection, host: &str) -> CoreResult<RemoteMetrics> {
    let raw = connection.exec(REMOTE_METRICS_SCRIPT, REMOTE_METRICS_MAX_BYTES)?;
    let metrics = parse_remote_output(&raw)?;
    Ok(RemoteMetrics {
        metrics,
        gpus: parse_gpu_lines(&raw),
        host: host.to_string(),
        scope: REMOTE_SCOPE.to_string(),
    })
}

/// 可采集 GPU 行的固定前缀（脚本约定，前缀后紧跟一个空格）。
const GPU_LINE_PREFIX: &str = "GPU ";

/// 「只知道型号」的 GPU 行前缀（脚本第三级 lspci 兜底，前缀后紧跟一个空格）。
///
/// 用独立前缀而不是给 `GPU` 行尾加标记，是因为温度字段本来就可能取到任意数值或 `-`，
/// 行尾再加一个 0/1 会和温度撞车；独立前缀让两种行在解析时零歧义，也完全不动原有行格式。
const GPU_NAME_ONLY_PREFIX: &str = "GPUX ";

/// 从脚本输出里挑出全部 GPU 行。
///
/// 参数：raw 为脚本完整输出（第一行是 CPU/MEM/NET，之后可能每行一块 GPU）
/// 核心流程：逐行尝试解析，解析不了的行直接跳过；一行都没有就返回空数组，
/// 让「没有 GPU」和「GPU 采集失败」走同一条前端路径，不报错。
fn parse_gpu_lines(raw: &str) -> Vec<GpuMetrics> {
    raw.lines().filter_map(parse_gpu_line).collect()
}

/// 解析一行 GPU 输出。
///
/// 参数：line 为形如 `GPU <名称> <利用率> <已用MiB> <总量MiB> <温度或->`
/// 或 `GPUX <名称> 0 0 0 -` 的一行
/// 核心流程：先按前缀判定 `measurable`（`GPU ` 可采集、`GPUX ` 仅型号）→
/// 去掉前缀后，末尾 4 个空格分隔字段是数值，剩下的全是名称（名称里带空格也能完整还原）→
/// 「仅型号」行不看数值字段，一律填 0 与 `None`，避免兜底行里的 0 被当成真实读数；
/// 字段数不够或名称为空视为畸形行返回 `None`，单个数值解析失败退化成默认值，
/// 绝不让一块坏卡拖垮整次采样。
fn parse_gpu_line(line: &str) -> Option<GpuMetrics> {
    let line = line.trim();
    let (rest, measurable) = match line.strip_prefix(GPU_LINE_PREFIX) {
        Some(rest) => (rest, true),
        None => (line.strip_prefix(GPU_NAME_ONLY_PREFIX)?, false),
    };

    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() < 5 {
        return None;
    }

    let (name_tokens, tail) = tokens.split_at(tokens.len() - 4);
    let name = name_tokens.join(" ");
    if name.is_empty() {
        return None;
    }

    if !measurable {
        return Some(GpuMetrics {
            name,
            measurable: false,
            ..Default::default()
        });
    }

    Some(GpuMetrics {
        name,
        util_percent: f32_or(tail[0], 0.0).clamp(0.0, 100.0),
        mem_used_mb: u64_or(tail[1], 0),
        mem_total_mb: u64_or(tail[2], 0),
        temp_c: tail[3].parse::<f32>().ok(),
        measurable: true,
    })
}

/// 取一个浮点字段，解析不了用兜底值。
///
/// 参数：token 为字段原文，fallback 为解析失败时的取值
/// 核心流程：显式调用 `parse` 再 `unwrap_or`，不 panic；调用方自行决定是否裁剪范围。
fn f32_or(token: &str, fallback: f32) -> f32 {
    token.parse::<f32>().unwrap_or(fallback)
}

/// 取一个无符号整数字段，解析不了用兜底值。
///
/// 参数：token 为字段原文，fallback 为解析失败时的取值
/// 核心流程：同上，避免脚本漏出 `[N/A]` 这类文本时把整行丢掉。
fn u64_or(token: &str, fallback: u64) -> u64 {
    token.parse::<u64>().unwrap_or(fallback)
}

/// 解析脚本输出。
///
/// 参数：raw 为脚本打印的那一行文本
/// 核心流程：按固定顺序校验标签并取值（CPU 1 个数、MEM 2 个数、NET 2 个数）→
/// 换算成 MiB 与百分比 → 数值越界时按物理范围裁剪；缺字段或非数字直接报错。
fn parse_remote_output(raw: &str) -> CoreResult<SystemMetrics> {
    let mut tokens = raw.split_whitespace();

    expect_label(&mut tokens, "CPU", raw)?;
    let cpu_percent = number::<f64>(&mut tokens, "CPU 使用率", raw)?;

    expect_label(&mut tokens, "MEM", raw)?;
    let total_kb = number::<u64>(&mut tokens, "内存总量", raw)?;
    let available_kb = number::<u64>(&mut tokens, "可用内存", raw)?;

    expect_label(&mut tokens, "NET", raw)?;
    let rx_kbps = number::<f64>(&mut tokens, "下行速率", raw)?;
    let tx_kbps = number::<f64>(&mut tokens, "上行速率", raw)?;

    if total_kb == 0 {
        return Err(parse_error("内存总量为 0", raw));
    }

    let used_kb = total_kb.saturating_sub(available_kb);
    Ok(SystemMetrics {
        cpu_percent: cpu_percent.clamp(0.0, 100.0) as f32,
        mem_percent: (used_kb as f64 * 100.0 / total_kb as f64) as f32,
        mem_used_mb: used_kb / 1024,
        mem_total_mb: total_kb / 1024,
        rx_kbps: rx_kbps.max(0.0),
        tx_kbps: tx_kbps.max(0.0),
        supported: true,
    })
}

/// 校验下一个字段是不是期望的标签。
///
/// 参数：tokens 为按空白切分的字段流，label 为期望标签，raw 为原始输出
/// 核心流程：取一个字段比对，缺字段或标签错位都转成带原文的错误。
fn expect_label<'a>(
    tokens: &mut impl Iterator<Item = &'a str>,
    label: &str,
    raw: &str,
) -> CoreResult<()> {
    match tokens.next() {
        Some(value) if value == label => Ok(()),
        Some(value) => Err(parse_error(
            &format!("第 {label} 段标签应为 {label}，实际为 {value}"),
            raw,
        )),
        None => Err(parse_error(&format!("缺少 {label} 段"), raw)),
    }
}

/// 取出一个数字字段。
///
/// 参数：tokens 为字段流，name 为字段中文名（用于报错），raw 为原始输出
/// 核心流程：缺字段与解析失败分别给文案，绝不 panic。
fn number<'a, T: FromStr>(
    tokens: &mut impl Iterator<Item = &'a str>,
    name: &str,
    raw: &str,
) -> CoreResult<T> {
    let token = tokens
        .next()
        .ok_or_else(|| parse_error(&format!("缺少{name}"), raw))?;
    token
        .parse::<T>()
        .map_err(|_| parse_error(&format!("{name}不是数字：{token}"), raw))
}

/// 构造解析错误。
///
/// 参数：reason 为失败原因，raw 为原始输出
/// 核心流程：把原文压成最多 160 字符的片段附在消息里，便于排查又不刷屏。
fn parse_error(reason: &str, raw: &str) -> CoreError {
    let snippet: String = raw.trim().chars().take(160).collect();
    CoreError::Validation(format!("远端指标解析失败（{reason}）：{snippet:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_output() {
        let metrics = parse_remote_output("CPU 12.5 MEM 16000000 8000000 NET 1.5 2.5\n")
            .expect("固定输出应能解析");

        assert!((metrics.cpu_percent - 12.5).abs() < 0.01);
        assert_eq!(metrics.mem_total_mb, 15_625);
        assert_eq!(metrics.mem_used_mb, 7_812);
        assert!((metrics.mem_percent - 50.0).abs() < 0.1);
        assert_eq!(metrics.rx_kbps, 1.5);
        assert_eq!(metrics.tx_kbps, 2.5);
        assert!(metrics.supported);
    }

    #[test]
    fn rejects_missing_field_with_clear_error() {
        let err = parse_remote_output("CPU 12.5 MEM 16000000 8000000 NET 1.5")
            .expect_err("少一个字段应报错");

        assert!(matches!(err, CoreError::Validation(_)));
        assert!(
            err.to_string().contains("上行速率"),
            "错误应点明缺的是哪个字段：{err}"
        );
    }

    #[test]
    fn rejects_non_numeric_value() {
        let err = parse_remote_output("CPU abc MEM 16000000 8000000 NET 1.5 2.5")
            .expect_err("非数字应报错");

        assert!(
            err.to_string().contains("CPU 使用率"),
            "错误应点明哪个字段不是数字：{err}"
        );
    }

    #[test]
    fn parses_last_line_of_df_output() {
        // 表头 + 多个挂载点 + 一条折行的极端输出（设备名过长被挤到上一行）
        // 结论行永远在最后 -> 必须取最后一行，前面的都不能算
        let raw = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
/dev/sda1 103080832 41077632 56741504 43% /\n\
/dev/sdb1 515928320 12345678 490000000 3% /data\n\
/dev/mapper/very-long-volume-name-that-gets-squeezed 209715200\n\
/dev/mapper/very-long-volume-name-that-gets-squeezed 209715200 104857600 104857600 50% /mnt/long\n";

        let usage = parse_df_output(raw).expect("固定输出应能解析");
        assert_eq!(usage.total_bytes, 209_715_200u64 * 1024, "总容量按 KiB 换算成字节");
        assert_eq!(usage.used_bytes, 104_857_600u64 * 1024, "已用按 KiB 换算成字节");
        assert_eq!(usage.free_bytes, 104_857_600u64 * 1024, "可用按 KiB 换算成字节");
        assert_eq!(usage.mount, "/mnt/long", "挂载点应来自最后一行");
        assert!(
            (usage.percent - 50.0).abs() < 0.01,
            "使用率应由 used/total 算出，实际：{}",
            usage.percent
        );
    }

    #[test]
    fn computes_percent_from_bytes_not_capacity_column() {
        // df 的 Capacity 列会是 33%，真实比例 33.333...% -> 必须以后者为准
        let raw = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
/dev/sda1 300 100 200 33% /\n";

        let usage = parse_df_output(raw).expect("应能解析");
        assert!(
            (usage.percent - 33.333_332).abs() < 0.01,
            "不应直接采信 Capacity 列的整数，实际：{}",
            usage.percent
        );
    }

    #[test]
    fn keeps_mount_point_with_spaces() {
        let raw = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
/dev/sda1 100 50 50 50% /mnt/my disk\n";

        let usage = parse_df_output(raw).expect("应能解析");
        assert_eq!(usage.mount, "/mnt/my disk", "带空格的挂载点应完整保留");
    }

    #[test]
    fn rejects_line_with_missing_columns() {
        let raw = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
/dev/sda1 100 50\n";

        let err = parse_df_output(raw).expect_err("列数不足应报错");
        assert!(matches!(err, CoreError::Validation(_)));
        assert!(
            err.to_string().contains("少于 6 列"),
            "错误应点明列数不足：{err}"
        );
    }

    #[test]
    fn rejects_non_numeric_columns() {
        let raw = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
/dev/sda1 abc 50 50 50% /\n";

        let err = parse_df_output(raw).expect_err("非数字应报错");
        assert!(
            err.to_string().contains("总容量"),
            "错误应点明哪一列不是数字：{err}"
        );
    }

    #[test]
    fn rejects_empty_df_output() {
        let err = parse_df_output("\n\n").expect_err("空输出应报错");
        assert!(
            err.to_string().contains("没有任何输出"),
            "错误应说明 df 没输出：{err}"
        );
    }

    #[test]
    fn rejects_zero_total_capacity() {
        let raw = "/dev/sda1 0 0 0 - /\n";

        let err = parse_df_output(raw).expect_err("总容量为 0 应报错");
        assert!(
            err.to_string().contains("总容量为 0"),
            "错误应点明容量异常：{err}"
        );
    }

    #[test]
    fn rejects_zero_memory_total() {
        let err = parse_remote_output("CPU 1.0 MEM 0 0 NET 0 0").expect_err("总内存为 0 应报错");

        assert!(err.to_string().contains("内存总量"), "错误应点明内存异常：{err}");
    }

    #[test]
    fn parses_windows_used_free_pair() {
        // 典型输出：一行两列字节数，行尾带 CRLF
        let raw = "53687091200 10737418240\r\n";

        let usage = parse_windows_disk_output(raw, "D:\\work").expect("固定输出应能解析");
        assert_eq!(usage.used_bytes, 53_687_091_200);
        assert_eq!(usage.free_bytes, 10_737_418_240);
        assert_eq!(
            usage.total_bytes,
            53_687_091_200 + 10_737_418_240,
            "总容量应等于已用加可用"
        );
        assert_eq!(usage.mount, "D:", "挂载点应是盘符");
        let expected = 53_687_091_200f64 * 100.0 / (53_687_091_200f64 + 10_737_418_240f64);
        assert!(
            (usage.percent - expected as f32).abs() < 0.01,
            "使用率应由已用/总量算出，实际：{}",
            usage.percent
        );
    }

    #[test]
    fn rejects_windows_output_with_missing_column() {
        let err = parse_windows_disk_output("53687091200\r\n", "C:\\")
            .expect_err("只有一列应报错");

        assert!(
            err.to_string().contains("不是 2 列"),
            "错误应点明列数不对：{err}"
        );
    }

    #[test]
    fn rejects_windows_output_with_non_numeric_column() {
        let err = parse_windows_disk_output("abc 10737418240\r\n", "C:\\")
            .expect_err("非数字应报错");

        assert!(
            err.to_string().contains("已用字节不是数字"),
            "错误应点明哪一列不是数字：{err}"
        );
    }

    #[test]
    fn windows_command_escapes_single_quotes() {
        let command = windows_disk_command("C:\\it's here");
        assert!(
            command.contains("$p='C:\\it''s here'"),
            "路径里的单引号应在 PowerShell 单引号串里翻倍：{command}"
        );
        assert!(
            !command.contains("\"$("),
            "命令里不应出现嵌套双引号，否则会绕晕外层 shell：{command}"
        );
    }

    #[test]
    fn parses_single_gpu_line() {
        // nvidia-smi 的典型输出被脚本整理成一行一块卡
        let raw = "CPU 12.5 MEM 16000000 8000000 NET 1.5 2.5\n\
GPU NVIDIA GeForce RTX 4090 45 1234 24564 62\n";

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 1, "应只解析出一块卡：{gpus:?}");
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert!((gpus[0].util_percent - 45.0).abs() < 0.01);
        assert_eq!(gpus[0].mem_used_mb, 1234);
        assert_eq!(gpus[0].mem_total_mb, 24564);
        assert_eq!(gpus[0].temp_c, Some(62.0));
        assert!(gpus[0].measurable, "nvidia-smi 采到的行必须标记为可采集");
    }

    #[test]
    fn parses_multiple_gpu_lines() {
        let raw = "CPU 1.0 MEM 16000000 8000000 NET 0.0 0.0\n\
GPU NVIDIA GeForce RTX 4090 45 1234 24564 62\n\
GPU Tesla T4 0 0 15360 -\n";

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 2, "两块卡都要保留：{gpus:?}");
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpus[1].name, "Tesla T4");
        assert!((gpus[1].util_percent - 0.0).abs() < 0.01);
        assert_eq!(gpus[1].mem_total_mb, 15360);
        assert_eq!(gpus[1].temp_c, None, "温度占位符 - 应解析成 None");
    }

    #[test]
    fn keeps_spaces_in_gpu_name() {
        // 名称里有多个空格、且以数字结尾 -> 从行尾往前数 4 个字段才是数值
        let raw = "GPU AMD  Radeon   PRO W7900 12 345 48960 55\n";
        let gpus = parse_gpu_lines(raw);

        assert_eq!(gpus.len(), 1, "多空格名称不应被拆成多块卡：{gpus:?}");
        assert_eq!(gpus[0].name, "AMD Radeon PRO W7900");
        assert!((gpus[0].util_percent - 12.0).abs() < 0.01);
        assert_eq!(gpus[0].mem_used_mb, 345);
        assert_eq!(gpus[0].mem_total_mb, 48960);
        assert_eq!(gpus[0].temp_c, Some(55.0));
    }

    #[test]
    fn returns_empty_gpus_when_no_gpu_lines() {
        // 没有 GPU 的机器只有 CPU/MEM/NET 一行 -> 空数组而不是报错
        let raw = "CPU 12.5 MEM 16000000 8000000 NET 1.5 2.5\n";
        assert!(parse_gpu_lines(raw).is_empty());
        assert!(parse_gpu_lines("").is_empty(), "空输出也应给空数组");
    }

    #[test]
    fn skips_malformed_gpu_lines_and_keeps_good_ones() {
        let raw = "GPU 只有名字没有数值\n\
GPU NVIDIA GeForce RTX 4090 45 1234 24564 62\n\
GPU\n\
GPU A100 1 2\n\
   关于采样的告警文字，混在输出里\n\
GPU Tesla T4 0 0 15360 -\n";

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 2, "畸形行应被跳过：{gpus:?}");
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpus[1].name, "Tesla T4");
    }

    #[test]
    fn defaults_gpu_numbers_when_values_are_not_numeric() {
        // 结构对但数值是文本（如 [N/A]、[Not Supported]）-> 该字段取默认值
        let raw = "GPU Tesla T4 abc 0 15360 [N/A]\n\
GPU NVIDIA A100 30 100 40960 70\n";

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 2, "数值坏掉不该把行丢掉：{gpus:?}");
        assert!((gpus[0].util_percent - 0.0).abs() < 0.01, "坏值退回 0");
        assert_eq!(gpus[0].mem_total_mb, 15360, "同行的好值仍要保留");
        assert_eq!(gpus[0].temp_c, None);
        assert!((gpus[1].util_percent - 30.0).abs() < 0.01);
        assert_eq!(gpus[1].temp_c, Some(70.0));
    }

    #[test]
    fn clamps_gpu_util_percent_to_physical_range() {
        let gpus = parse_gpu_lines("GPU GPU 140 0 0 20\n");

        assert_eq!(gpus.len(), 1);
        assert!((gpus[0].util_percent - 100.0).abs() < 0.01, "利用率应裁剪到 0-100");
    }

    #[test]
    fn gpu_lines_do_not_break_cpu_mem_net_parsing() {
        // 追加 GPU 行以后，原来的 CPU/MEM/NET 解析必须完全不受影响
        let raw = "CPU 12.5 MEM 16000000 8000000 NET 1.5 2.5\n\
GPU NVIDIA GeForce RTX 4090 45 1234 24564 62\n";

        let metrics = parse_remote_output(raw).expect("GPU 行不应影响指标解析");
        assert!((metrics.cpu_percent - 12.5).abs() < 0.01);
        assert_eq!(metrics.mem_total_mb, 15_625);
    }

    #[test]
    fn remote_metrics_serializes_gpus_as_camel_case() {
        let remote = RemoteMetrics {
            metrics: SystemMetrics {
                supported: true,
                ..Default::default()
            },
            gpus: vec![GpuMetrics {
                name: "NVIDIA GeForce RTX 4090".into(),
                util_percent: 45.0,
                mem_used_mb: 1234,
                mem_total_mb: 24564,
                temp_c: Some(62.0),
                measurable: true,
            }],
            host: "10.0.0.1".into(),
            scope: REMOTE_SCOPE.to_string(),
        };

        let json = serde_json::to_value(&remote).expect("应能序列化");
        let gpus = json.get("gpus").expect("序列化结果必须带 gpus 字段");
        assert!(gpus.is_array(), "gpus 必须是数组：{gpus}");

        let gpu = &gpus[0];
        assert_eq!(gpu["name"], "NVIDIA GeForce RTX 4090");
        assert_eq!(gpu["utilPercent"], 45.0);
        assert_eq!(gpu["memUsedMb"], 1234);
        assert_eq!(gpu["memTotalMb"], 24564);
        assert_eq!(gpu["tempC"], 62.0);
        assert_eq!(gpu["measurable"], true, "序列化必须带 measurable");
        assert!(gpu.get("util_percent").is_none(), "字段名必须是 camelCase");
        assert!(gpu.get("measurable").is_some(), "measurable 本身也是 camelCase");
        assert_eq!(json["scope"], "remote");
    }

    #[test]
    fn remote_metrics_serializes_empty_gpus_as_empty_array() {
        let remote = RemoteMetrics {
            metrics: SystemMetrics::default(),
            gpus: Vec::new(),
            host: "10.0.0.2".into(),
            scope: REMOTE_SCOPE.to_string(),
        };

        let json = serde_json::to_value(&remote).expect("应能序列化");
        assert_eq!(json["gpus"], serde_json::json!([]), "无 GPU 时必须是空数组");
    }
    #[test]
    fn parses_name_only_gpu_line_as_not_measurable() {
        // 第三级兜底：只认出型号，数值全 0、温度占位符 -，必须标成「采不到指标」
        let raw = "CPU 12.5 MEM 16000000 8000000 NET 1.5 2.5\n\
GPUX NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1) 0 0 0 -\n";

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 1, "仅型号行也要解析出一块卡：{gpus:?}");
        assert_eq!(
            gpus[0].name,
            "NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1)",
            "名称里的空格、方括号、括号都要原样保留"
        );
        assert!(!gpus[0].measurable, "仅型号行必须标记为不可采集");
        assert!((gpus[0].util_percent - 0.0).abs() < 0.01);
        assert_eq!(gpus[0].mem_used_mb, 0);
        assert_eq!(gpus[0].mem_total_mb, 0);
        assert_eq!(gpus[0].temp_c, None);
    }

    #[test]
    fn name_only_line_ignores_its_numeric_fields() {
        // 兜底行即使被塞了数值也不能当成读数：不可采集就是不可采集
        let gpus = parse_gpu_lines("GPUX Tesla T4 99 4096 15360 77\n");

        assert_eq!(gpus.len(), 1);
        assert!(!gpus[0].measurable);
        assert!((gpus[0].util_percent - 0.0).abs() < 0.01, "不可采集就不该有利用率");
        assert_eq!(gpus[0].mem_used_mb, 0);
        assert_eq!(gpus[0].mem_total_mb, 0);
        assert_eq!(gpus[0].temp_c, None);
    }

    #[test]
    fn parses_mixed_measurable_and_name_only_gpus() {
        // 一块能采、一块只能认型号：各自的字段互不污染
        let raw = "CPU 3.0 MEM 16000000 8000000 NET 0.0 0.0\n\
GPU NVIDIA GeForce RTX 4090 45 1234 24564 62\n\
GPUX NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1) 0 0 0 -\n";

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 2, "两种行都要保留：{gpus:?}");

        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert!(gpus[0].measurable);
        assert!((gpus[0].util_percent - 45.0).abs() < 0.01);
        assert_eq!(gpus[0].mem_used_mb, 1234);
        assert_eq!(gpus[0].mem_total_mb, 24564);
        assert_eq!(gpus[0].temp_c, Some(62.0));

        assert_eq!(
            gpus[1].name,
            "NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1)"
        );
        assert!(!gpus[1].measurable);
        assert!((gpus[1].util_percent - 0.0).abs() < 0.01);
        assert_eq!(gpus[1].mem_total_mb, 0);
        assert_eq!(gpus[1].temp_c, None);
    }

    #[test]
    fn name_only_lines_do_not_break_cpu_mem_net_parsing() {
        // 兜底行紧跟 CPU/MEM/NET 时，前半段指标仍要解析正确
        let raw = "CPU 12.5 MEM 16000000 8000000 NET 1.5 2.5\n\
GPUX NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1) 0 0 0 -\n";

        let metrics = parse_remote_output(raw).expect("兜底行不应影响指标解析");
        assert!((metrics.cpu_percent - 12.5).abs() < 0.01);
        assert_eq!(metrics.mem_total_mb, 15_625);
        assert!((metrics.rx_kbps - 1.5).abs() < 0.01);

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 1);
        assert!(!gpus[0].measurable);
    }

    #[test]
    fn skips_malformed_name_only_lines() {
        // 兜底行同样要能扛畸形输入：没有名称、名称后没有数值占位都不算一块卡
        let raw = "GPUX\n\
GPUX   0 0 0 -\n\
GPUX NVIDIA GP107 0 0 0 -\n";

        let gpus = parse_gpu_lines(raw);
        assert_eq!(gpus.len(), 1, "只有第三行是合法兜底行：{gpus:?}");
        assert_eq!(gpus[0].name, "NVIDIA GP107");
        assert!(!gpus[0].measurable);
    }

    #[test]
    fn serializes_name_only_gpu_with_measurable_false() {
        let remote = RemoteMetrics {
            metrics: SystemMetrics::default(),
            gpus: vec![GpuMetrics {
                name: "NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1)".into(),
                measurable: false,
                ..Default::default()
            }],
            host: "10.0.0.3".into(),
            scope: REMOTE_SCOPE.to_string(),
        };

        let json = serde_json::to_value(&remote).expect("应能序列化");
        let gpu = &json["gpus"][0];
        assert_eq!(gpu["measurable"], false);
        assert_eq!(gpu["utilPercent"], 0.0);
        assert_eq!(gpu["tempC"], serde_json::Value::Null);
    }

    /// 脚本里定义 lspci 名称精简 sed 的前缀标记（脚本用 `gcs='<sed>'` 定义它）。
    const LSPCI_NAME_SED_MARKER: &str = "gcs='";

    /// 从脚本里抽出 lspci 名称精简的那段 sed。
    ///
    /// 参数：无
    /// 核心流程：按 `gcs='` 与紧随的 `';` 截出中间文本——
    /// 测试跑的就是线上脚本用的同一份表达式，不另抄一份规则去测。
    fn lspci_name_sed() -> String {
        let script = REMOTE_METRICS_SCRIPT;
        let start = script
            .find(LSPCI_NAME_SED_MARKER)
            .expect("脚本里应有 gcs='...' 定义 lspci 名称精简规则")
            + LSPCI_NAME_SED_MARKER.len();
        let rest = &script[start..];
        let end = rest.find("';").expect("gcs 的 sed 表达式应以单引号收尾");
        rest[..end].to_string()
    }

    /// 把若干行 lspci 原文喂给脚本里那段 sed，逐行返回精简后的名称。
    ///
    /// 参数：lines 为假 lspci 输出（一行一块卡）
    /// 核心流程：直接起 `sed -e <脚本里那段>`，原文从 stdin 灌进去——
    /// 这样测的是真命令行行为（BRE 转义、`[^[]` 这类写法），不是 Rust 里的复刻。
    fn clean_lspci_names(lines: &[&str]) -> Vec<String> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("sed")
            .arg("-e")
            .arg(lspci_name_sed())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("应能启动 sed：远端脚本本来就依赖 sed，测试机也一样");
        child
            .stdin
            .as_mut()
            .expect("stdin 应可用")
            .write_all(lines.join("\n").as_bytes())
            .expect("应能写入假 lspci 输出");
        let out = child.wait_with_output().expect("sed 应能正常结束");
        assert!(out.status.success(), "sed 应以 0 退出：{out:?}");
        String::from_utf8(out.stdout)
            .expect("sed 输出应是 UTF-8")
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect()
    }

    #[test]
    fn lspci_name_sed_trims_vendor_prefix_and_revision() {
        // 真机三种典型 lspci 全文：NV 带方括号、Intel 不带方括号、AMD 带 [AMD/ATI] 假前缀
        let got = clean_lspci_names(&[
            "01:00.0 VGA compatible controller: NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1)",
            "00:02.0 VGA compatible controller: Intel Corporation UHD Graphics 630 (Desktop) (rev 04)",
            "03:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Navi 23 [Radeon RX 6600] (rev c7)",
        ]);

        assert_eq!(
            got,
            vec![
                "GP107 [GeForce GTX 1050 Ti]",
                "UHD Graphics 630 (Desktop)",
                "Navi 23 [Radeon RX 6600]",
            ],
            "厂商前缀与 (rev xx) 都要去掉，方括号里的型号原文一个字都不能丢"
        );
    }

    #[test]
    fn lspci_name_sed_handles_names_without_revision() {
        // 没有 (rev) 的行同样要精简；本来就干净的名称不能被误伤
        let got = clean_lspci_names(&[
            "04:00.0 VGA compatible controller: NVIDIA Corporation GP107 [GeForce GTX 1050 Ti]",
            "05:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Navi 23 [Radeon RX 6600]",
            "06:00.0 VGA compatible controller: Intel Corporation UHD Graphics 630 (Desktop)",
            "07:00.0 VGA compatible controller: Tesla T4",
        ]);

        assert_eq!(
            got,
            vec![
                "GP107 [GeForce GTX 1050 Ti]",
                "Navi 23 [Radeon RX 6600]",
                "UHD Graphics 630 (Desktop)",
                "Tesla T4",
            ],
            "不带 (rev) 也要精简，且不能把没有厂商前缀的名称削掉"
        );
    }

    #[test]
    fn lspci_name_sed_falls_back_to_generic_vendor_pattern() {
        // 没列举到的厂商靠通用规则兜住；[^[] 保证不会越过第一个方括号
        let got = clean_lspci_names(&[
            "08:00.0 VGA compatible controller: Red Hat, Inc. QXL paravirtual graphic card (rev 05)",
            "09:00.0 VGA compatible controller: Some Unknown Corp. Weird Card [ABC 1] (rev 10)",
        ]);

        assert_eq!(
            got,
            vec!["QXL paravirtual graphic card", "Weird Card [ABC 1]"],
            "未知厂商按 Corporation|Inc.|Ltd.|Co. 兜底删除，方括号里的型号保留"
        );
    }

    #[test]
    fn name_cleaning_is_wired_only_into_lspci_fallback() {
        // nvidia-smi 与 /sys 给的名称本来就是干净的，精简规则只准挂在 lspci 支路上
        let script = REMOTE_METRICS_SCRIPT;
        assert_eq!(
            script.matches("gcs").count(),
            2,
            "gcs 只应定义一次、使用一次，否则可能被挂到别的支路上"
        );
        let lspci_at = script.find("lspci").expect("脚本里应有 lspci 兜底");
        let use_at = script.find("sed -e \"$gcs\"").expect("脚本里应用上 gcs");
        assert!(
            use_at > lspci_at,
            "gcs 只应在 lspci 管道里生效，不能碰 nvidia-smi 的 GPU 行"
        );
    }
}
