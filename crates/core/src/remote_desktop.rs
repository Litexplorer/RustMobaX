//! 远程桌面降级方案：找出本机已安装的 RDP / VNC 客户端，用它打开会话。
//!
//! 本期不自研 RDP / VNC 渲染（`vnc-rs` 的认证覆盖不足、`ironrdp-client`
//! 只能出整幅帧，投入产出不划算），因此这两类会话一律「交给系统客户端」：
//! 这里只负责两件事 —— 检测装了哪些客户端，以及把 host/port/user
//! 拼成该客户端的命令行参数。
//!
//! 三条硬性设计约束：
//!
//! 1. **绝不拼 shell 字符串**：参数一律以独立 argv 元素交给
//!    `Command::new(program).args(args)`，主机名里出现 `;`、`|`、`$(...)`
//!    也只会是一个普通字符，不会被任何 shell 解释。
//! 2. **绝不传密码**：命令行参数在进程列表（Linux 的 `ps`、macOS 的活动监视器）
//!    里对同机所有用户可见，所以密码永远不进参数表 —— 由系统客户端自己弹框询问。
//! 3. **检测失败不报错**：任何异常（没有 PATH、权限不足、路径不存在）都回空
//!    `Vec`，前端据「空列表」给安装引导。这样调用方不需要为平台差异写分支。

use serde::Serialize;

use crate::{CoreError, CoreResult};

/// 一个可用于打开远程桌面的系统客户端。
///
/// `kind` 是稳定的枚举键（前端按它选启动参数），`name` 给人看，
/// `path` 是探测到的绝对路径（`launch` 优先用它，因为 PATH 上未必有同名命令，
/// 比如 FreeRDP 3 只提供 `xfreerdp3`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDesktopClient {
    pub kind: String,
    pub name: String,
    pub path: String,
}

/// kind 常量：Windows 自带的「远程桌面连接」。
pub const KIND_MSTSC: &str = "mstsc";
/// kind 常量：FreeRDP 命令行客户端（`xfreerdp` / `xfreerdp3` 都归到这里）。
pub const KIND_XFREERDP: &str = "xfreerdp";
/// kind 常量：Remmina 的 RDP 连接。
pub const KIND_REMMINA: &str = "remmina";
/// kind 常量：各类 VNC Viewer（`vncviewer` / `xtightvncviewer` / RealVNC）。
pub const KIND_VNCVIEWER: &str = "vncviewer";

/// 探测本机已安装的远程桌面客户端。
///
/// 无参数
/// 核心流程：按平台给出候选表（kind、展示名、可执行文件名或 .app 路径）→
/// 逐个在 PATH 里找（macOS 直接判断固定路径的 App bundle）→
/// 命中即记录绝对路径，同一 kind 只留第一个。全部失败回空列表，绝不 panic。
pub fn detect_clients() -> Vec<RemoteDesktopClient> {
    let mut found: Vec<RemoteDesktopClient> = Vec::new();

    #[cfg(windows)]
    {
        // mstsc.exe 一定在 %SystemRoot%\System32 下，但 PATH 里通常没有 System32
        // （Tauri 起的 GUI 进程继承的 PATH 可能被裁剪），所以先探这个固定位置。
        detect_windows_system32(KIND_MSTSC, "远程桌面连接", "mstsc.exe", &mut found);
        detect_in_path(KIND_MSTSC, "远程桌面连接", &["mstsc.exe"], &mut found);
        detect_in_path(KIND_VNCVIEWER, "VNC Viewer", &["vncviewer.exe"], &mut found);
    }

    #[cfg(target_os = "linux")]
    {
        // xfreerdp3 是 FreeRDP 3 的可执行名，与 xfreerdp 参数完全兼容
        detect_in_path(
            KIND_XFREERDP,
            "FreeRDP",
            &["xfreerdp", "xfreerdp3"],
            &mut found,
        );
        detect_in_path(KIND_REMMINA, "Remmina", &["remmina"], &mut found);
        detect_in_path(
            KIND_VNCVIEWER,
            "VNC Viewer",
            &["vncviewer", "xtightvncviewer"],
            &mut found,
        );
    }

    #[cfg(target_os = "macos")]
    {
        // macOS 的客户端都是 .app 包，没有命令行入口，只能判断 bundle 是否存在
        detect_bundle(
            KIND_MSTSC,
            "Microsoft Remote Desktop",
            "/Applications/Microsoft Remote Desktop.app",
            &mut found,
        );
        detect_bundle(
            KIND_VNCVIEWER,
            "VNC Viewer",
            "/Applications/RealVNC Viewer.app",
            &mut found,
        );
    }

    #[allow(unreachable_code)]
    found
}

/// 登记一个客户端，同一 kind 只留第一个。
///
/// 参数：list 为结果集，kind / name / path 为待登记项
/// 核心流程：先查 kind 是否已存在 —— 同时装了 `vncviewer` 与
/// `xtightvncviewer` 时，两个按钮会指向同一条启动参数，用户只会困惑。
fn push_unique(list: &mut Vec<RemoteDesktopClient>, kind: &str, name: &str, path: String) {
    if list.iter().any(|existing| existing.kind == kind) {
        return;
    }
    list.push(RemoteDesktopClient {
        kind: kind.to_string(),
        name: name.to_string(),
        path,
    });
}

/// 在 PATH 里按顺序找可执行文件。
///
/// 参数：list 为结果集，kind / name 为展示信息，names 为候选可执行文件名
/// 核心流程：遍历 PATH 的每个目录 × 每个候选名，第一个「存在且可执行」的胜出。
/// 这里**不**起 `where` / `which` 子进程：少一次进程创建，也避免某些环境
/// 里 `where` 不存在或输出编码不同带来的解析问题。
#[cfg(any(windows, target_os = "linux"))]
fn detect_in_path(
    kind: &str,
    name: &str,
    names: &[&str],
    list: &mut Vec<RemoteDesktopClient>,
) {
    if list.iter().any(|existing| existing.kind == kind) {
        return;
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        // PATH 里的空项（表示当前目录）跳过：那是明显的配置错误，不该信任
        if dir.as_os_str().is_empty() {
            continue;
        }
        for candidate in names {
            let path = dir.join(candidate);
            if is_executable_file(&path) {
                push_unique(list, kind, name, path.to_string_lossy().into_owned());
                return;
            }
        }
    }
}

/// 探测 Windows 系统目录里的固定可执行文件。
///
/// 参数：list 为结果集，kind / name / file 为展示信息与文件名
/// 核心流程：用 `%SystemRoot%` 拼 `System32\<file>`，文件存在就登记。
/// `%SystemRoot%` 缺失时直接跳过，绝不回退到硬编码的 `C:\Windows`
/// （系统盘不一定是 C）。
#[cfg(windows)]
fn detect_windows_system32(
    kind: &str,
    name: &str,
    file: &str,
    list: &mut Vec<RemoteDesktopClient>,
) {
    let root = match std::env::var_os("SystemRoot") {
        Some(root) => root,
        None => return,
    };
    let path = std::path::Path::new(&root).join("System32").join(file);
    if is_executable_file(&path) {
        push_unique(list, kind, name, path.to_string_lossy().into_owned());
    }
}

/// 判断 macOS 的 App bundle 是否存在。
///
/// 参数：list 为结果集，kind / name 为展示信息，bundle 为 .app 路径
/// 核心流程：只判断存在性（`.app` 是目录，不是可执行文件）。
#[cfg(target_os = "macos")]
fn detect_bundle(kind: &str, name: &str, bundle: &str, list: &mut Vec<RemoteDesktopClient>) {
    if std::path::Path::new(bundle).exists() {
        push_unique(list, kind, name, bundle.to_string());
    }
}

/// 判断路径是否为「可执行文件」。
///
/// 参数：path 为待判断路径
/// 核心流程：Unix 下还要求任一执行位，避免把同名的数据文件当成客户端；
/// 其它平台只看是否普通文件。读不到元数据一律当「不是」。
#[cfg(unix)]
#[allow(dead_code)] // macOS 只判断 .app 是否存在，用不到这个函数
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// 判断路径是否为可执行文件（非 Unix）。
///
/// 参数：path 为待判断路径
/// 核心流程：Windows 下可执行性由扩展名表达，候选名本来就带 `.exe`，只看是否存在。
#[cfg(not(unix))]
fn is_executable_file(path: &std::path::Path) -> bool {
    path.is_file()
}

/// 把主机与端口拼成 `<host>:<port>`。
///
/// 参数：host 为去掉两端空白的主机名或 IP，port 为端口
/// 核心流程：IPv6 字面量本身含冒号，必须加方括号，否则 `::1:3389`
/// 会被客户端解析成两个字段。已是 `[..]` 形式的不再重复加括号。
fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 拼出某个客户端的启动程序与参数。
///
/// 参数：kind 为客户端类型，host / port 为目标，username 为可选的登录名
/// 核心流程：按客户端各自的命令行约定产出 argv。**任何分支都不放密码**：
/// `mstsc` / `xfreerdp` 都能在参数里带口令，这里刻意不用，改由客户端弹框询问，
/// 避免密码出现在同机可见的进程列表里。host 为空或 kind 未知时回 `None`，
/// 让调用方给出明确的错误而不是启动一个必然失败的进程。
pub fn build_launch_args(
    kind: &str,
    host: &str,
    port: u16,
    username: &str,
) -> Option<(String, Vec<String>)> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let user = username.trim();
    let address = host_port(host, port);

    let (program, args) = match kind {
        KIND_MSTSC => {
            // mstsc 用 `/v:<主机>[:<端口>]`，用户名用 `/u:<用户>`
            let mut args = vec![format!("/v:{address}")];
            if !user.is_empty() {
                args.push(format!("/u:{user}"));
            }
            ("mstsc.exe", args)
        }
        KIND_XFREERDP => {
            // FreeRDP 用 `/v:` 与 `/u:`，端口跟着主机写成 `host:port`
            let mut args = vec![format!("/v:{address}")];
            if !user.is_empty() {
                args.push(format!("/u:{user}"));
            }
            ("xfreerdp", args)
        }
        KIND_VNCVIEWER => {
            // vncviewer / xtightvncviewer 都接受「主机:端口」作为位置参数
            ("vncviewer", vec![address])
        }
        KIND_REMMINA => {
            // Remmina 走 URI：rdp://[user@]host:port，不认 `/v:` 这类参数
            let uri = if user.is_empty() {
                format!("rdp://{address}")
            } else {
                format!("rdp://{user}@{address}")
            };
            ("remmina", vec!["-c".to_string(), uri])
        }
        _ => return None,
    };

    Some((program.to_string(), args))
}

/// 启动系统客户端打开一个远程桌面会话。
///
/// 参数：kind / host / port / username 同 `build_launch_args`
/// 核心流程：拼参数 → 优先用检测到的绝对路径当程序（PATH 上未必有同名命令）→
/// spawn 后立即返回，不等子进程结束（客户端是长驻的 GUI，等它会卡住界面）。
/// macOS 的客户端是 .app，改用 `open -a` 拉起。
pub fn launch(kind: &str, host: &str, port: u16, username: &str) -> CoreResult<()> {
    let (program, args) = build_launch_args(kind, host, port, username)
        .ok_or_else(|| CoreError::NotFound(format!("未安装可用的远程桌面客户端（{kind}）")))?;

    // 检测到的路径优先：`xfreerdp3`、`xtightvncviewer` 与 .app 包都只有路径没有同名命令
    let program = detect_clients()
        .into_iter()
        .find(|client| client.kind == kind)
        .map(|client| client.path)
        .unwrap_or(program);

    #[cfg(target_os = "macos")]
    {
        // macOS 的 .app 不能直接执行，交给 open 拉起并把地址作为 URL 传进去
        if program.ends_with(".app") {
            let url = args
                .last()
                .cloned()
                .unwrap_or_else(|| host_port(host.trim(), port));
            let scheme = if kind == KIND_VNCVIEWER { "vnc" } else { "rdp" };
            let target = if url.starts_with("rdp://") || url.starts_with("vnc://") {
                url
            } else {
                format!("{scheme}://{url}")
            };
            std::process::Command::new("open")
                .args(["-a", &program, &target])
                .spawn()?;
            return Ok(());
        }
    }

    std::process::Command::new(program).args(args).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 收集参数里所有字面量，便于断言「密码没出现」。
    fn joined(args: &[String]) -> String {
        args.join(" ")
    }

    /// mstsc：`/v:host:port`，有用户名时追加 `/u:user`。
    #[test]
    fn build_args_for_mstsc() {
        let (program, args) = build_launch_args(KIND_MSTSC, "win.example.com", 3389, "alice")
            .expect("mstsc 必须支持");
        assert_eq!(program, "mstsc.exe");
        assert_eq!(
            args,
            vec!["/v:win.example.com:3389".to_string(), "/u:alice".to_string()],
            "mstsc 应先给 /v 再给 /u"
        );

        // 用户名为空（或全空白）时不产出 /u: —— 否则客户端会去连一个空账号
        for blank in ["", "  "] {
            let (_, args) = build_launch_args(KIND_MSTSC, "10.0.0.1", 13389, blank)
                .expect("mstsc 必须支持");
            assert_eq!(args, vec!["/v:10.0.0.1:13389".to_string()]);
        }
    }

    /// xfreerdp：`/v:host:port`，有用户名时追加 `/u:user`。
    #[test]
    fn build_args_for_xfreerdp() {
        let (program, args) = build_launch_args(KIND_XFREERDP, "rdp.local", 3389, "bob")
            .expect("xfreerdp 必须支持");
        assert_eq!(program, "xfreerdp");
        assert_eq!(
            args,
            vec!["/v:rdp.local:3389".to_string(), "/u:bob".to_string()]
        );

        let (_, args) =
            build_launch_args(KIND_XFREERDP, "rdp.local", 3389, "").expect("xfreerdp 必须支持");
        assert_eq!(args, vec!["/v:rdp.local:3389".to_string()]);
    }

    /// vncviewer：位置参数 `host:port`，不带任何用户/密码选项。
    #[test]
    fn build_args_for_vncviewer() {
        let (program, args) =
            build_launch_args(KIND_VNCVIEWER, "vnc.local", 5900, "carol").expect("vncviewer 必须支持");
        assert_eq!(program, "vncviewer");
        assert_eq!(
            args,
            vec!["vnc.local:5900".to_string()],
            "vncviewer 只吃一个位置参数，用户名不应出现"
        );

        let (_, args) =
            build_launch_args(KIND_VNCVIEWER, "vnc.local", 5901, "").expect("vncviewer 必须支持");
        assert_eq!(args, vec!["vnc.local:5901".to_string()]);
    }

    /// remmina：URI 形式，用户名拼进 URI，端口拼在主机后面。
    #[test]
    fn build_args_for_remmina() {
        let (program, args) = build_launch_args(KIND_REMMINA, "10.1.2.3", 3389, "dave")
            .expect("remmina 必须支持");
        assert_eq!(program, "remmina");
        assert_eq!(
            args,
            vec!["-c".to_string(), "rdp://dave@10.1.2.3:3389".to_string()]
        );

        let (_, args) =
            build_launch_args(KIND_REMMINA, "10.1.2.3", 3389, "").expect("remmina 必须支持");
        assert_eq!(args, vec!["-c".to_string(), "rdp://10.1.2.3:3389".to_string()]);
    }

    /// 密码永远不进参数表：所有分支的参数里都不能出现任何口令选项。
    #[test]
    fn build_args_never_carry_password() {
        // 这些是各客户端真正用来传口令的开关，出现任何一个都算回归
        const PASSWORD_SWITCHES: [&str; 5] = ["/p:", "-p ", "--password", "/pass:", "password="];

        for kind in [KIND_MSTSC, KIND_XFREERDP, KIND_REMMINA, KIND_VNCVIEWER] {
            let (_, args) = build_launch_args(kind, "host.example.com", 3389, "user")
                .unwrap_or_else(|| panic!("{kind} 必须支持"));
            let text = joined(&args);
            for switch in PASSWORD_SWITCHES {
                assert!(
                    !text.contains(switch),
                    "{kind} 的参数里不应出现口令开关 {switch}，实际：{text}"
                );
            }
            assert!(
                !text.to_ascii_lowercase().contains("passwd"),
                "{kind} 的参数里不应出现 passwd，实际：{text}"
            );
        }
    }

    /// 未知 kind 回 None，空主机也回 None（不启动必然失败的进程）。
    #[test]
    fn build_args_rejects_unknown_kind_and_empty_host() {
        assert!(
            build_launch_args("anydesk", "host", 3389, "user").is_none(),
            "未支持的客户端必须回 None"
        );
        assert!(build_launch_args("", "host", 3389, "").is_none());
        for host in ["", "   "] {
            assert!(
                build_launch_args(KIND_MSTSC, host, 3389, "user").is_none(),
                "空主机名必须回 None"
            );
        }
    }

    /// 主机名里的 shell 元字符只能原样待在同一个 argv 元素里。
    #[test]
    fn build_args_keeps_shell_metacharacters_inside_one_argument() {
        let hostile = "host;rm -rf /";
        for kind in [KIND_MSTSC, KIND_XFREERDP, KIND_REMMINA, KIND_VNCVIEWER] {
            let (_, args) = build_launch_args(kind, hostile, 3389, "user")
                .unwrap_or_else(|| panic!("{kind} 必须支持"));
            let carries = args
                .iter()
                .any(|arg| arg.contains("host;rm -rf /"));
            assert!(
                carries,
                "{kind} 必须把主机名整体作为单个 argv 元素，实际：{args:?}"
            );
            // 被空格拆开就说明有人做了字符串拼接后按空白切分
            assert!(
                !args.iter().any(|arg| arg == "rm" || arg == "-rf"),
                "{kind} 的参数被空格拆开了：{args:?}"
            );
        }

        // 空格也是同理：`/v:` 后面的主机名不能变成两个参数
        let (_, args) = build_launch_args(KIND_MSTSC, "my host", 3389, "").expect("mstsc 必须支持");
        assert_eq!(args, vec!["/v:my host:3389".to_string()]);
    }

    /// IPv6 字面量要加方括号，否则客户端会把地址里的冒号当端口分隔符。
    #[test]
    fn build_args_brackets_ipv6_literal() {
        let (_, args) = build_launch_args(KIND_VNCVIEWER, "::1", 5900, "").expect("vncviewer 必须支持");
        assert_eq!(args, vec!["[::1]:5900".to_string()]);

        // 已经带方括号的不再重复加
        let (_, args) =
            build_launch_args(KIND_VNCVIEWER, "[::1]", 5900, "").expect("vncviewer 必须支持");
        assert_eq!(args, vec!["[::1]:5900".to_string()]);
    }

    /// 检测在任何环境都不能 panic —— 本机装没装客户端不写进断言。
    #[test]
    fn detect_clients_never_panics() {
        let clients = detect_clients();
        for client in &clients {
            assert!(
                [KIND_MSTSC, KIND_XFREERDP, KIND_REMMINA, KIND_VNCVIEWER].contains(&&*client.kind),
                "出现了未登记的 kind：{}",
                client.kind
            );
            assert!(!client.path.trim().is_empty(), "命中项的路径不应为空");
            assert!(!client.name.trim().is_empty(), "命中项应有展示名");
        }
        // 同一 kind 只留一个，前端才不会出现两个指向同一命令的按钮
        let mut kinds: Vec<&str> = clients.iter().map(|client| client.kind.as_str()).collect();
        kinds.sort_unstable();
        let before = kinds.len();
        kinds.dedup();
        assert_eq!(before, kinds.len(), "同一 kind 不应重复出现");
    }

    /// 未知 kind 启动时给出「未安装」错误，而不是尝试执行空程序名。
    #[test]
    fn launch_rejects_unknown_kind() {
        let err = launch("anydesk", "host", 3389, "user").expect_err("未知 kind 必须报错");
        assert!(
            matches!(err, CoreError::NotFound(_)),
            "未知 kind 应回 NotFound，实际：{err:?}"
        );
    }
}
