//! WSL 支持：列出已安装的发行版，并把「发行版」拼成一条本地终端命令。
//!
//! 这里的全部逻辑都按「WSL 只是换一个可执行程序的本地 PTY」来设计：
//! 枚举发行版要调外部进程（仅 Windows 有意义），拼命令是纯字符串运算，
//! 因此后者在三平台都能编译与测试，前端不必关心平台差异。

/// WSL 的可执行文件名。
///
/// 固定用 `wsl.exe` 而不是 `wsl`：它是 Windows 系统目录里的真实文件名，
/// 不依赖 PATH 上的 `wsl` 别名，也不受 PowerShell 别名解析影响。
const WSL_EXE: &str = "wsl.exe";

/// 需要跳过的发行版前缀。
///
/// `docker-desktop` 与 `docker-desktop-data` 是 Docker Desktop 自己注册的内部
/// 发行版，用户既看不到也进不去交互式 shell，列在会话表单里只会造成误选。
const INTERNAL_DISTRO_PREFIX: &str = "docker-desktop";

/// 提示信息里一定会出现的片段。
///
/// 部分 wsl.exe 版本把「没有已安装的分发版」这类提示打到 stdout 且退出码为 0，
/// 若照单全收就会把整句提示当成一个发行版名。命中这些片段的行一律丢弃。
const NOISE_MARKERS: [&str; 3] = [
    "Windows Subsystem for Linux",
    "适用于 Linux 的 Windows 子系统",
    "Linux 的 Windows 子系统",
];

/// 默认工作目录参数。
///
/// `--cd ~` 由 wsl.exe 自己解析成该发行版里的用户主目录，
/// 比让上层去猜 `/home/<user>` 更可靠。
const DEFAULT_CD: (&str, &str) = ("--cd", "~");

/// 列出本机已安装的 WSL 发行版。
///
/// 无参数
/// 核心流程：仅 Windows 下真的执行 `wsl.exe -l -q` 并解析输出；
/// 其它平台直接返回空列表。WSL 未安装、命令失败、输出无法解析
/// 一律回空 `Vec`，绝不返回错误 —— 前端据「空列表」给出安装引导，
/// 而「没有错误」保证了 Linux / macOS 上调用方不需要额外分支。
pub fn list_distros() -> Vec<String> {
    #[cfg(windows)]
    {
        list_distros_windows()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Windows 下实际执行枚举命令。
///
/// 无参数
/// 核心流程：起子进程收集 stdout（创建时不弹控制台窗口）→
/// 退出码非零视为「WSL 没装 / 没发行版」→ 交给纯函数解析。
#[cfg(windows)]
fn list_distros_windows() -> Vec<String> {
    use std::os::windows::process::CommandExt;

    /// 不弹出控制台窗口：GUI 进程里起 wsl.exe 会闪一个黑框。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut command = std::process::Command::new(WSL_EXE);
    command.args(["-l", "-q"]);
    command.creation_flags(CREATE_NO_WINDOW);

    let output = match command.output() {
        Ok(output) => output,
        // 找不到 wsl.exe / 无法启动 -> 当作「未安装」
        Err(_) => return Vec::new(),
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_distro_output(&output.stdout)
}

/// 解析 `wsl.exe -l -q` 的原始输出。
///
/// 参数：raw 为子进程 stdout 的原始字节
/// 核心流程：先按 BOM / 零字节分布判断是 UTF-16LE 还是 UTF-8 →
/// 解码成文本 → 逐行裁掉空白与残留的 NUL、跳过空行、提示文字与内部发行版。
/// `wsl.exe` 在中文等非英文区域仍可能给 UTF-16LE，所以两种编码都要认。
pub fn parse_distro_output(raw: &[u8]) -> Vec<String> {
    let text = decode_output(raw);
    let mut distros: Vec<String> = Vec::new();
    for line in text.lines() {
        // trim_matches 一并去掉 NUL / BOM / 各类空白：UTF-16 解码后可能留 \0
        let name = line.trim_matches(|ch: char| ch.is_whitespace() || ch == '\0' || ch == '\u{feff}');
        if name.is_empty() {
            continue;
        }
        // 提示文字（如「没有已安装的分发版」）不是发行版名
        if NOISE_MARKERS
            .iter()
            .any(|marker| name.contains(marker))
        {
            continue;
        }
        // 内部发行版（docker-desktop、docker-desktop-data）对用户无意义
        if name
            .to_ascii_lowercase()
            .starts_with(INTERNAL_DISTRO_PREFIX)
        {
            continue;
        }
        if !distros.iter().any(|existing| existing == name) {
            distros.push(name.to_string());
        }
    }
    distros
}

/// 把子进程输出解码成字符串。
///
/// 参数：raw 为原始字节
/// 核心流程：有 UTF-16LE BOM，或奇数位大量为 0（ASCII 文本的 UTF-16LE 特征）
/// 时按 UTF-16LE 解码，否则按 UTF-8 宽松解码；解码失败不报错，只丢非法字符。
fn decode_output(raw: &[u8]) -> String {
    let has_le_bom = raw.starts_with(&[0xFF, 0xFE]);
    if has_le_bom || looks_like_utf16le(raw) {
        let start = if has_le_bom { 2 } else { 0 };
        let units: Vec<u16> = raw[start..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(raw).into_owned()
}

/// 判断字节流是否像 UTF-16LE 编码的 ASCII 文本。
///
/// 参数：raw 为原始字节
/// 核心流程：ASCII 字符在 UTF-16LE 里高字节恒为 0，于是奇数位大半是 0；
/// 抽样前 64 个字节，奇数位为 0 的比例超过一半即认定是 UTF-16LE。
/// 纯 UTF-8 文本几乎不可能满足这一比例。
fn looks_like_utf16le(raw: &[u8]) -> bool {
    let sample = &raw[..raw.len().min(64)];
    if sample.len() < 4 {
        return false;
    }
    let odd: Vec<u8> = sample
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .collect();
    if odd.is_empty() {
        return false;
    }
    let zeros = odd.iter().filter(|byte| **byte == 0).count();
    zeros * 2 > odd.len()
}

/// 把一个发行版名拼成「本地终端可执行程序 + 参数」。
///
/// 参数：distro 为发行版名，空串（或纯空白）表示用 WSL 默认发行版
/// 核心流程：非空时先给 `-d <发行版>`，再统一给 `--cd ~`；
/// 发行版名为空则**完全不传 `-d`**，交给 wsl.exe 自己挑默认发行版，
/// 避免拼出 `wsl.exe -d ""` 这种必然失败的参数。
pub fn build_command(distro: &str) -> (String, Vec<String>) {
    let name = distro.trim();
    let mut args: Vec<String> = Vec::new();
    if !name.is_empty() {
        args.push("-d".to_string());
        args.push(name.to_string());
    }
    args.push(DEFAULT_CD.0.to_string());
    args.push(DEFAULT_CD.1.to_string());
    (WSL_EXE.to_string(), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 指定发行版时必须产出 `wsl.exe -d <名称> --cd ~`。
    #[test]
    fn build_command_passes_distro_when_named() {
        let (program, args) = build_command("Ubuntu-22.04");
        assert_eq!(program, "wsl.exe", "程序名必须是 wsl.exe");
        assert_eq!(
            args,
            vec![
                "-d".to_string(),
                "Ubuntu-22.04".to_string(),
                "--cd".to_string(),
                "~".to_string()
            ],
            "带发行版时应先给 -d 再给工作目录"
        );
    }

    /// 发行版名为空时不传 `-d` -> 走 WSL 默认发行版。
    #[test]
    fn build_command_omits_distro_when_empty() {
        for empty in ["", "   ", "\t\n"] {
            let (program, args) = build_command(empty);
            assert_eq!(program, "wsl.exe");
            assert!(
                !args.iter().any(|arg| arg == "-d"),
                "空发行版不应产 -d，实际参数：{args:?}"
            );
            assert_eq!(
                args,
                vec!["--cd".to_string(), "~".to_string()],
                "空发行版只应保留工作目录参数"
            );
        }
    }

    /// 发行版名两端的空白要去掉，避免拼出带空格的 `-d` 值。
    #[test]
    fn build_command_trims_distro_name() {
        let (_, args) = build_command("  Debian \n");
        assert_eq!(args[0], "-d");
        assert_eq!(args[1], "Debian", "发行版名应被裁剪后再传");
    }

    /// 非 Windows 平台没有 WSL：必须回空列表，且不能 panic、不能报错。
    #[test]
    fn list_distros_is_empty_and_safe_off_windows() {
        let distros = list_distros();
        if cfg!(windows) {
            // Windows 上结果取决于本机装了什么 -> 只验证不 panic
            assert!(distros.iter().all(|name| !name.trim().is_empty()));
        } else {
            assert!(distros.is_empty(), "非 Windows 平台必须返回空列表");
        }
    }

    /// UTF-8 输出（新版本 wsl.exe 的常见形态）按行切分并跳过空行。
    #[test]
    fn parses_utf8_output_and_skips_blank_lines() {
        let raw = b"Ubuntu-22.04\r\n\r\nDebian\n\nAlpine\n";
        assert_eq!(
            parse_distro_output(raw),
            vec![
                "Ubuntu-22.04".to_string(),
                "Debian".to_string(),
                "Alpine".to_string()
            ]
        );
    }

    /// UTF-16LE 输出（带 BOM 与不带 BOM 两种）都要能解析。
    #[test]
    fn parses_utf16le_output_with_and_without_bom() {
        let text = "Ubuntu\r\nDebian\r\n";
        let mut body: Vec<u8> = text
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();

        assert_eq!(
            parse_distro_output(&body),
            vec!["Ubuntu".to_string(), "Debian".to_string()],
            "无 BOM 的 UTF-16LE 输出应能解析"
        );

        let mut with_bom = vec![0xFF, 0xFE];
        with_bom.append(&mut body);
        assert_eq!(
            parse_distro_output(&with_bom),
            vec!["Ubuntu".to_string(), "Debian".to_string()],
            "带 BOM 的 UTF-16LE 输出应能解析"
        );
    }

    /// Docker Desktop 注册的内部发行版、重复项与提示文字都要被过滤掉。
    #[test]
    fn parses_and_drops_internal_distros() {
        let raw = b"Ubuntu\ndocker-desktop\ndocker-desktop-data\nUbuntu\n";
        assert_eq!(
            parse_distro_output(raw),
            vec!["Ubuntu".to_string()],
            "内部发行版与重复项都不应出现在结果里"
        );

        // 「没有已安装的分发版」这类提示有时打到 stdout 且退出码为 0
        let noisy = b"\r\nWindows Subsystem for Linux has no installed distributions.\r\n";
        assert!(
            parse_distro_output(noisy).is_empty(),
            "提示文字不应被当成发行版名"
        );
    }

    /// 完全无法识别的输出（空、只有空白或无效字节）不能 panic。
    #[test]
    fn parses_without_panicking_on_junk() {
        assert!(parse_distro_output(b"").is_empty());
        assert!(parse_distro_output(b"\r\n  \r\n").is_empty());
        // 单个字节凑不成 UTF-16 -> 走 UTF-8 宽松解码，只要不 panic 即可
        let junk = parse_distro_output(&[0xFF]);
        assert!(
            junk.len() <= 1,
            "无法识别的输出不应被拆成多行，实际：{junk:?}"
        );
    }
}
