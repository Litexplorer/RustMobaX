# 架构与技术选型

## 一、总体形态

Tauri 2.x 的天然切分正好匹配需求：

```
┌──────────────────────────────────────────────────────────┐
│  WebView（React + TS + xterm.js）                        │
│  终端渲染 / 会话列表 / SFTP 面板 / 设置                   │
└───────────────▲──────────────────────┬───────────────────┘
                │ invoke / Channel      │ events
┌───────────────┴──────────────────────▼───────────────────┐
│  Tauri 主进程（Rust）                                     │
│  命令层（commands）→ 服务层（session / ssh / sftp / …）    │
│  tokio 运行时 / 会话注册表 / 凭据库 / SQLite              │
└───────────────┬──────────────────────────────────────────┘
                │ spawn / channel
┌───────────────▼──────────────────────────────────────────┐
│  外部世界：SSH 服务端、本地 PTY、串口、代理目标            │
└──────────────────────────────────────────────────────────┘
```

**关键原则：所有协议实现、所有凭据、所有字节流都在 Rust 侧。** 前端只做渲染与交互，
不持有密码，不拼 SSH 报文。这样即使 WebView 被调试，也拿不到密钥。

## 二、代码分层

```
src-tauri/
├── src/
│   ├── main.rs              # Tauri 启动、插件注册、单实例
│   ├── commands/            # IPC 边界，只做参数校验 + 转发
│   │   ├── session.rs
│   │   ├── terminal.rs
│   │   ├── sftp.rs
│   │   ├── tunnel.rs
│   │   └── toolbox.rs
│   ├── session/             # 会话模型、生命周期、注册表
│   │   ├── model.rs
│   │   ├── manager.rs
│   │   └── store.rs         # SQLite 访问
│   ├── ssh/                 # 连接、认证、通道、known_hosts
│   ├── transport/           # 协议适配：ssh / local-pty / serial / telnet
│   ├── terminal/            # PTY 泵、窗口尺寸、字节流节流
│   ├── sftp/                # 文件操作、传输队列、续传
│   ├── tunnel/              # L / R / D 三类转发与重连
│   ├── vault/               # Argon2id + AES-GCM 凭据库
│   ├── toolbox/             # ping / dns / whois / portscan
│   ├── x11/                 # X11 通道转发（P1）
│   └── error.rs             # 统一错误类型，转前端可读文案
└── Cargo.toml
```

业务核心（`session/` `ssh/` `sftp/` `tunnel/` `vault/`）不依赖 Tauri 类型，
便于抽成独立 crate `rustmobax-core` 供 CLI / 测试复用。

### transport 抽象

三种会话（SSH / 本地 PTY / 串口）在终端视角下是同一种东西，抽象成一个 trait：

```rust
trait TerminalTransport {
    async fn open(&mut self, size: TermSize) -> Result<()>;
    fn write(&self, data: &[u8]) -> Result<()>;
    fn resize(&self, size: TermSize) -> Result<()>;
    async fn close(&mut self) -> Result<()>;
}
```

终端泵（`terminal/pump.rs`）只认这个 trait，负责：读字节 → 合并 → 节流 → 推给前端；
以及前端的写请求 → 转发给具体传输实现。**新增一种协议只加一个实现，不动泵。**

## 三、关键数据流

### 终端字节流（性能最敏感路径）

1. 后端 `transport` 读出原始字节（SSH channel data / PTY read）。
2. 泵按 4–8 KB 或 8 ms 聚合成批，避免每字节一次 IPC。
3. 通过 Tauri `Channel`（流式）推给前端，`Uint8Array` 传输，不走 JSON 字符串。
4. 前端 `Terminal.write(Uint8Array)` 直接喂给 xterm.js。

反向：前端 `onData` 合并 8 ms 内的按键，批量 `invoke` 到后端。
**节流必须在两端都做**，否则 `cat` 大文件或 `yes` 会把 IPC 打满。

### SFTP 传输

传输走独立任务队列（`tokio::task` + 有界 channel 限流），进度通过 events 广播，
不阻塞终端通道。上传时按 64 KB 分块，块间检查取消标志。

### 隧道

每条隧道是独立 `tokio::spawn` 的 supervisor：监听器 → 接受连接 → 走 SSH 通道 → 双向 `copy_bidirectional`。
supervisor 持有 cancel token 与指数退避状态，UI 只读注册表快照。

## 四、数据模型（SQLite）

```sql
-- 会话分组
CREATE TABLE session_group (
  id TEXT PRIMARY KEY, parent_id TEXT, name TEXT NOT NULL, sort_order INTEGER
);

-- 会话
CREATE TABLE session (
  id TEXT PRIMARY KEY,
  group_id TEXT,
  name TEXT NOT NULL,
  protocol TEXT NOT NULL,        -- ssh | sftp | local | serial | telnet | rdp | vnc
  host TEXT, port INTEGER, username TEXT,
  auth_kind TEXT,                -- password | key | agent | keyboard-interactive
  credential_ref TEXT,           -- 凭据库中的条目 id，非明文
  jump_session_id TEXT,          -- ProxyJump 引用的会话
  extra_json TEXT,               -- 协议私有配置（串口参数、X11 开关等）
  created_at INTEGER, updated_at INTEGER
);

-- 凭据（仅存密文）
CREATE TABLE credential (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, label TEXT NOT NULL,
  nonce BLOB NOT NULL, ciphertext BLOB NOT NULL, updated_at INTEGER
);

-- known_hosts
CREATE TABLE known_host (host TEXT, port INTEGER, key_type TEXT, fingerprint TEXT,
                         PRIMARY KEY (host, port, key_type));

-- 键值配置
CREATE TABLE setting (key TEXT PRIMARY KEY, value TEXT);
```

凭据表里**永远不出现明文**，`credential_ref` 只是外键。
主密码派生的密钥只存在于解锁期内存，锁定时显式 `zeroize`。

## 五、技术选型与备选

| 领域 | 选定 | 理由 | 备选与放弃原因 |
| --- | --- | --- | --- |
| 外壳 | Tauri 2.x | 体积小、Rust 原生、三平台 | Electron（内存与体积不可接受）；Wails（Go 侧生态弱于 russh） |
| 前端 | React 19 + TS + Vite | 生态成熟、xterm.js 集成示例多 | Svelte（体积更好，但团队熟悉度低）；Vue |
| 终端渲染 | xterm.js（`@xterm/xterm` + fit/search/webgl addons） | 事实标准，处理了 TUI 大部分坑 | 自研 VT 解析器（成本极高）；`alacritty_terminal` + canvas（可行但需大量打磨） |
| SSH | `russh` | 纯 Rust、异步、支持 channel 与 forwarding | `ssh2`（libssh2 C 绑定，需交叉编译、阻塞 API）；`thrussh`（已并入 russh） |
| SFTP | `russh-sftp` | 与 russh 同栈，无需额外连接 | 直接手撸 SFTP 报文（无必要） |
| 本地 PTY | `portable-pty` | 已被 wezterm 验证，三平台封装好 | `nix`/`winpty` 手写（重复造轮子） |
| 串口 | `serialport` | 成熟稳定 | `mio-serial` |
| 异步运行时 | `tokio` | russh 强依赖 | — |
| 存储 | `sqlx` + SQLite | 编译期校验 SQL、异步 | `rusqlite`（阻塞，需 spawn_blocking）；`sled`（无 SQL） |
| 加密 | `argon2` + `aes-gcm` + `zeroize` | RustCrypto 出品，审计充分 | `ring`（API 更底层）；OpenSSL（交叉编译负担） |
| RDP | `ironrdp` | 纯 Rust，活跃 | 外挂 `xfreerdp`/`mstsc`（作为降级路径保留） |
| VNC | `vnc-rs` + noVNC 前端 | 复用前端渲染 | 纯 Rust 解码 + canvas（工作量大） |
| 打包 | Tauri bundler | 生成 msi/dmg/deb | — |

## 六、安全设计要点

| 威胁 | 对策 |
| --- | --- |
| 磁盘被拷走读到密码 | 凭据库 AES-256-GCM，密钥由 Argon2id 从主密码派生 |
| 内存被 dump 拿到明文 | `zeroize` 显式擦除，明文生命周期限制在调用栈内 |
| WebView 被调试注入 | 前端不接触凭据；IPC 命令做能力白名单，`withGlobalTauri: false` |
| 中间人攻击 | 严格 known_hosts 校验，指纹变化硬阻断 |
| 粘贴误执行 | 多行粘贴确认 + 危险模式识别（前端拦截 + 后端二次校验） |
| 日志泄露敏感信息 | 日志默认脱敏，诊断包导出前扫描密码字段 |

Tauri 侧需显式收紧：`capabilities` 只开放必要命令、关闭 `devtools` 的 release 构建、
`dangerousDisableAssetCspModification` 保持默认、外部链接走 `shell-open` 白名单。

## 七、性能目标（首版验收线）

| 场景 | 目标 |
| --- | --- |
| 冷启动到会话列表可交互 | ≤ 800 ms |
| 点击连接到终端可输入（内网） | ≤ 3 s |
| 终端吞吐（`cat` 10 MB 文本） | 无丢字节，UI 不卡顿，端到端 ≤ 2 s |
| `yes` 满速输出时 CPU | 单核 ≤ 60% |
| 传输队列并发 4 个大文件 | 终端交互不受影响 |
| 内存占用（5 个活跃会话） | ≤ 250 MB |

## 八、已知风险与降级策略

| 风险 | 影响 | 降级 |
| --- | --- | --- |
| 内嵌 X server 不可行 | Windows 上"开箱即用 GUI"落空 | 打包/引导外挂 VcXsrv，US-903 长期延期 |
| `russh` 对个别老服务端兼容性 | 连不上某些设备 | 提供"兼容模式"开关（关闭扩展算法、降级 KEX） |
| cwd 跟随依赖 shell 上报 | 部分 shell / 网络设备不生效 | 回退为手动刷新，UI 明示能力不可用 |
| Tauri WebView 在三平台差异 | 渲染与输入行为不一致 | 关键路径抽 adapter，CI 三平台冒烟 |
| 终端 IPC 瓶颈 | 大输出卡顿 | 批量传输 + 二进制字节 + 必要时自建本地回环 WebSocket |
| RDP/VNC 自研周期长 | P2 拖延 | 保留"用系统客户端打开"的降级路径（US-1103） |
