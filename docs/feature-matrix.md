# 功能矩阵与可行性

对标 MobaXterm 的完整功能域，标注优先级（P0/P1/P2）、Rust 技术可行性、实现路径。

优先级口径：**P0 = 首版可用闭环**，**P1 = 第二阶段**，**P2 = 长期**，**— = 不做**。

## 一、会话与连接

| 能力 | MobaXterm | 优先级 | Rust 可行性 | 实现路径 |
| --- | --- | --- | --- | --- |
| SSH2 会话 | ✅ | P0 | 高 | `russh`（纯 Rust，支持 PTY / 通道 / 转发） |
| 密码 / 私钥 / Agent 认证 | ✅ | P0 | 高 | `russh` + `ssh-agent-client` + `keyring` |
| 键盘交互式认证（OTP/2FA） | ✅ | P0 | 高 | `russh` 的 `AuthMethod::KeyboardInteractive` |
| ProxyJump 跳板机（单跳） | ✅ | P0 | 高 | 先建跳板连接，再用 `direct-tcpip` 通道当传输层 |
| ProxyJump 多跳链 | ✅ | P1 | 中 | 通道嵌套，需自己管理转发链生命周期 |
| 本地 Shell（含 WSL） | ✅ | P0 | 高 | `portable-pty` |
| Telnet | ✅ | P1 | 高 | 自研最小 Telnet 协商（仅 IAC/DONT/WILL 子集） |
| 串口 | ✅ | P1 | 高 | `serialport` |
| RDP | ✅ | P2 | 中 | `ironrdp`；MVP 允许外挂 `mstsc`/`xfreerdp` |
| VNC | ✅ | P2 | 中 | `vnc-rs` + noVNC 前端渲染 |
| Mosh | ✅ | — | 低 | 不排期 |
| 会话分组 / 搜索 / 收藏 | ✅ | P0 | 高 | SQLite + 前端虚拟列表 |
| 会话标签 / 分屏 | ✅ | P0 | 高 | 前端布局引擎（tabs + split pane） |
| 会话导入导出 `.mxtsessions` | ✅ | P0 | 中 | 解析 INI 风格文本，字段缺失降级 |
| 便携模式（数据跟着程序走） | ✅ | P1 | 高 | 判可执行文件同级目录，覆盖数据根路径 |

## 二、终端体验

| 能力 | MobaXterm | 优先级 | Rust 可行性 | 实现路径 |
| --- | --- | --- | --- | --- |
| 多标签终端 | ✅ | P0 | 高 | 前端 + 后端按 session id 路由 |
| 分屏（水平/垂直） | ✅ | P0 | 高 | 前端 split 树 |
| TrueColor / 256 色 / 鼠标事件 | ✅ | P0 | 高 | xterm.js + `TERM=xterm-256color` 或 `xterm-direct` |
| 回滚缓冲 / 终端内搜索 | ✅ | P0 | 高 | xterm.js addon-search |
| 粘贴保护 / 括号粘贴 | ✅ | P0 | 高 | xterm.js bracketed paste |
| 字体、字号、主题 | ✅ | P0 | 高 | 前端配置 + 自定义配色方案 JSON |
| 会话日志落盘 | ✅ | P1 | 高 | 后端旁路复制字节流写文件 |
| 多执行（广播到多会话） | ✅ | P1 | 高 | 前端把输入扇出到多个 session writer |
| 宏录制 / 回放 | ✅ | P1 | 中 | 记录按键与时间戳序列，回放时按间隔注入 |
| 终端会话恢复（断线重连） | ✅ | P2 | 中 | 会话保持 + ring buffer 重放 |

## 三、文件操作（SFTP）

| 能力 | MobaXterm | 优先级 | Rust 可行性 | 实现路径 |
| --- | --- | --- | --- | --- |
| SFTP 侧栏浏览 | ✅ | P0 | 高 | `russh-sftp` |
| 跟随终端 `cd` 切换目录 | ✅ | P0 | 中 | 在 shell 侧注入 `PROMPT_COMMAND`/OSC 7 上报 cwd，后端解析 |
| 上传 / 下载（含拖拽） | ✅ | P0 | 高 | Tauri drag-drop 事件 + 分块传输 + 进度事件 |
| 重命名 / 删除 / 新建目录 / 改权限 | ✅ | P0 | 高 | `russh-sftp` 对应操作 |
| 内置文本编辑器（就地保存） | ✅ | P0 | 中 | CodeMirror 6（前端）+ 读写走 SFTP |
| 传输队列 / 断点续传 | ✅ | P1 | 中 | 自有任务队列 + `stat` 大小续传 |
| 目录对比 / 同步 | ✅ | P2 | 中 | 递归 `stat` 比对（大小 + mtime） |
| 远程文件搜索 | ✅ | P2 | 中 | 递归 readdir + 前端过滤 |

## 四、图形转发（X11）

| 能力 | MobaXterm | 优先级 | Rust 可行性 | 实现路径 |
| --- | --- | --- | --- | --- |
| X11 转发通道（`x11` channel） | ✅ | P1 | 中 | `russh` 的 `x11` 通道转发到本地 X server |
| Windows 上开箱可用 | ✅（内嵌 X server） | P1 | 低 | 打包 VcXsrv/Xming 作外挂进程，本轮不内嵌 |
| Linux/mac 复用系统 X server | ✅ | P1 | 高 | 直接连 `$DISPLAY` / XQuartz |
| 内嵌 X server | ✅ | P2 | 很低 | 自研工作量巨大；备选绑定 `xorg-server` 或 `xvfb` 子进程 |

> 结论：X11 转发**链路**能做（P1），**内嵌 server** 是全程最高风险项，必须显式降级并对外说明。

## 五、隧道与网络

| 能力 | MobaXterm | 优先级 | Rust 可行性 | 实现路径 |
| --- | --- | --- | --- | --- |
| 本地端口转发 `-L` | ✅ | P0 | 高 | `russh` `direct-tcpip` + 本地 `tokio::net::TcpListener` |
| 远程端口转发 `-R` | ✅ | P0 | 高 | `russh` `tcpip-forward` + `forwarded-tcpip` |
| 动态转发 SOCKS5 `-D` | ✅ | P0 | 高 | 自研 SOCKS5 握手（CONNECT 域）后走 `direct-tcpip` |
| 隧道状态面板 / 自动重连 | ✅ | P1 | 高 | 后端注册表 + 指数退避 |
| ping | ✅ | P1 | 高 | `surge-ping`（免 root 的 ICMP）或调系统 `ping` |
| traceroute | ✅ | P1 | 中 | 递增 TTL + `pnet` 原始套接字，或调系统命令 |
| DNS 查询 | ✅ | P1 | 高 | `hickory-resolver` |
| whois | ✅ | P1 | 高 | TCP 43 直连 |
| 端口探测 / 扫描 | ✅ | P1 | 高 | `tokio` 并发 connect，限速 |

## 六、安全与凭据

| 能力 | MobaXterm | 优先级 | Rust 可行性 | 实现路径 |
| --- | --- | --- | --- | --- |
| 主密码（Master Password） | ✅ | P0 | 高 | Argon2id 派生 KEK，AES-256-GCM 加密凭据库 |
| 密码 / 私钥加密落盘 | ✅ | P0 | 高 | 同上；明文仅在内存会话期存在 |
| 系统钥匙串集成 | ✅ | P1 | 高 | `keyring`（Keychain / Credential Manager / Secret Service） |
| known_hosts 校验 + 首次指纹确认 | ✅ | P0 | 高 | 自维护 `known_hosts`，未知指纹弹窗确认 |
| SSH Agent 转发 | ✅ | P2 | 中 | 需在远端起 agent socket，跨平台差异大 |
| 凭据访问审计日志 | ❌ | P2 | 高 | 本地 SQLite 记事件 |

## 七、工程化与扩展

| 能力 | MobaXterm | 优先级 | Rust 可行性 | 实现路径 |
| --- | --- | --- | --- | --- |
| 内置包管理（装 git/gcc） | ✅（Cygwin） | — | 高但无意义 | 不做，交给系统包管理器 |
| 自定义启动脚本 | ✅ | P1 | 高 | 会话级 `pre-command` / `post-command` |
| 单实例 + 命令行打开 | ✅ | P1 | 高 | Tauri 单实例插件 + `deep link`/argv 解析 |
| 插件 / 脚本扩展 | ✅ | P2 | 中 | 前端插件 API 优先，Rust 侧后续 |
| 自动更新 | ✅ | P2 | 高 | Tauri updater |

## 可行性总结

| 判定 | 功能 |
| --- | --- |
| **生态成熟，直接用** | SSH、SFTP、本地 PTY、串口、端口转发、DNS、加密存储、终端渲染 |
| **需少量自研** | cwd 跟随、多执行、宏录制、SOCKS5、`.mxtsessions` 解析、跳板链 |
| **需显式降级** | X11（先外挂系统 server）、RDP（先外挂客户端）、多显示器 |
| **高风险，长期** | 内嵌 X server、SSH Agent 转发、终端会话断线恢复 |
