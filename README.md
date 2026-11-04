# RustMobaX

用 **Rust + Tauri** 复刻 [MobaXterm](https://mobaxterm.mobatek.net/) 的跨平台终端工作台：
把远程会话、终端、文件传输、端口转发、X11 转发、凭据管理和网络工具箱，收拢进**一个窗口、一套会话**里。

> 状态：**M1 进行中**。已交付：会话管理、本地终端、SSH（密码/私钥/指纹三态/一跳跳板）、
> 凭据库、终端体验（分屏/搜索/粘贴保护）、SFTP 文本级、隧道（本地/远程/SOCKS5）、
> 左侧栏四分页（会话/工具/宏/文件，SFTP 常驻）、设置与外观（5 套主题 + 导入自定义主题、缩放、终端字号与字体）。

## 为什么重做一个 MobaXterm

| MobaXterm 的现状 | RustMobaX 的取舍 |
| --- | --- |
| 仅 Windows，非商业版有会话数、隧道数限制 | 三平台（Windows / macOS / Linux），无功能阉割 |
| 闭源，内置 Cygwin 环境不可裁剪 | 开源，Rust 核心，无运行时依赖膨胀 |
| X11 server 与 SFTP 面板是杀手锏 | 保留这两条主线，X11 分阶段实现（先外挂，后自研） |
| 界面陈旧、高分屏与多标签体验一般 | Tauri WebView 前端，xterm.js 渲染，主题/分屏可定制 |

## 文档索引

| 文档 | 内容 |
| --- | --- |
| [docs/product-vision.md](docs/product-vision.md) | 产品定位、目标用户、范围与非目标 |
| [docs/feature-matrix.md](docs/feature-matrix.md) | 对标 MobaXterm 的功能矩阵、优先级、技术可行性 |
| [docs/user-stories.md](docs/user-stories.md) | **核心**：按 Epic 拆分的用户故事与验收标准 |
| [docs/architecture.md](docs/architecture.md) | 分层架构、进程模型、模块划分、技术选型 |
| [docs/roadmap.md](docs/roadmap.md) | M0–M3 里程碑与交付口径 |
| [docs/dev-setup.md](docs/dev-setup.md) | 开发环境、系统依赖、日常命令、本机限制 |

## 技术栈（概览）

| 层 | 选型 |
| --- | --- |
| 外壳 | Tauri 2.x |
| 前端 | React 19 + TypeScript + Vite，xterm.js 渲染终端 |
| SSH / SFTP | `russh` + `russh-sftp`（纯 Rust） |
| 本地终端 | `portable-pty` |
| 串口 | `serialport` |
| 存储 | SQLite（`sqlx`）+ 本地加密库（Argon2id + AES-GCM） |
| RDP / VNC | `ironrdp` / `vnc-rs`（P2 阶段，MVP 允许外挂） |

细节与备选方案见 [docs/architecture.md](docs/architecture.md)。

## 快速开始

```bash
cd RustMobaX
bun install          # 安装前端依赖
bun run dev          # 只跑前端，浏览器访问 http://localhost:1420
bun run tauri dev    # 跑完整桌面应用（需要 webkit2gtk-4.1）
```

> Linux 上 Tauri 2 需要 `libwebkit2gtk-4.1-dev` + `libsoup-3.0`（Ubuntu 22.04+）。
> 依赖清单、本机限制与可选出路见 [docs/dev-setup.md](docs/dev-setup.md)。

## 目录结构

```
RustMobaX/
├── docs/                  # 设计与需求文档
├── index.html             # Vite 入口
├── package.json           # 前端清单（脚本统一走 bun）
├── src/                   # 前端（React + TS）
│   ├── App.tsx            # 骨架页：调用后端 app_info 验证 IPC
│   └── main.tsx
├── src-tauri/             # Tauri 主进程
│   ├── Cargo.toml         # 含独立 [workspace]，避免被上层 workspace 捕获
│   ├── capabilities/      # 能力白名单，当前仅 core:default
│   ├── tauri.conf.json    # 窗口尺寸、标识符 com.rustmobax.desktop
│   └── src/
│       ├── lib.rs         # app_info 命令 + run()
│       └── main.rs        # 二进制入口
├── tsconfig.json
└── vite.config.ts         # 固定端口 1420，strictPort
```

后续业务模块（session / ssh / terminal / vault / toolbox）按需加入，不预建空目录。

## 许可

待定（规划为 MIT / Apache-2.0 双许可）。
