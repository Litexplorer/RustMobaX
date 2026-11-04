# 开发环境

## 一、工具链要求

| 工具 | 版本要求 | 本机实测 |
| --- | --- | --- |
| Rust | ≥ 1.82（Tauri 2 与 edition 2024 要求） | 1.92.0 |
| bun | ≥ 1.2 | 1.3.11 |
| Node.js | 仅作为部分工具兜底，本项目统一用 bun | 22.22.2 |
| Tauri CLI | 2.x（作为 devDependency 安装） | 2.12.4 |

## 二、Linux 系统依赖（关键）

Tauri 2.x 的 Linux 端依赖 **webkit2gtk-4.1 + libsoup-3.0**（Tauri 1.x 用的是 4.0）。

```bash
# Ubuntu 22.04+ / Debian 12+
/usr/bin/apt install -y \
  libwebkit2gtk-4.1-dev librsvg2-dev patchelf \
  build-essential curl wget file libxdo-dev libssl-dev \
  libayatana-appindicator3-dev
```

发行版差异：

| 系统 | 包名 |
| --- | --- |
| Ubuntu 22.04 / 24.04 | `libwebkit2gtk-4.1-dev`、`libsoup-3.0-dev` |
| Fedora | `webkit2gtk4.1-devel`、`libsoup3-devel` |
| Arch | `webkit2gtk-4.1`、`libsoup3` |
| macOS | 无需额外依赖，装 Xcode Command Line Tools |
| Windows | 无需额外依赖，需 WebView2 Runtime（Win11 自带） |

### 当前开发机的限制（必须知道）

本机是 **Ubuntu 20.04.4 LTS**，两处硬性不满足：

```bash
# 1) 没有 webkit2gtk-4.1
$ pkg-config --modversion webkit2gtk-4.1
Package webkit2gtk-4.1 was not found
$ /usr/bin/apt-cache policy libwebkit2gtk-4.1-dev
（无输出，源里不存在该包）

# 2) 更底层：GLib 版本不够（Tauri 2 要求 >= 2.70）
$ pkg-config --modversion glib-2.0
2.64.6
```

实测 `cargo check` 的真实失败点比预想的更早，连 webkit 都没走到：

```
error: failed to run custom build command for `glib-sys v0.18.1`
  Requested 'glib-2.0 >= 2.70' but version of GLib is 2.64.6
```

结论：**在这台机器上无法编译 Tauri 2.x 的桌面二进制**。GLib 2.70 是 Ubuntu 22.04 的基线，
20.04 上升级会牵动整个桌面栈，不可行。这不是代码问题。

可选出路（按推荐度排序）：

1. **在 Ubuntu 22.04+ 环境开发**（容器或虚拟机），这是唯一无副作用的路。
2. 本机只做前端开发：`bun run dev` 起 Vite，浏览器里调 UI（IPC 相关功能不可用）。
3. 本机只做 Rust 代码的语法与格式校验：`cargo metadata`、`cargo fmt --check`。
4. 混源装 jammy 包：**不推荐**，会拉起 glibc 依赖链，容易把系统搞坏。

## 三、日常命令

| 目的 | 命令 | 本机可用 |
| --- | --- | --- |
| 安装前端依赖 | `bun install` | ✅ |
| 前端开发服务器 | `bun run dev` | ✅（浏览器访问 1420） |
| 前端类型检查 | `bun run typecheck` | ✅ |
| 桌面应用开发 | `bun run tauri dev` | ❌ 缺 webkit2gtk-4.1 |
| 桌面应用打包 | `bun run tauri build` | ❌ 同上 |
| Rust 依赖与清单校验 | `cargo metadata --no-deps` | ✅（在 `src-tauri/`） |
| Rust 格式校验 | `cargo fmt --check` | ✅ |
| Rust 静态检查 | `cargo clippy --all-targets` | ❌ 依赖 webkit |

> 约定：本项目前端一律用 bun，不使用 npm / yarn。

## 四、目录说明

```
RustMobaX/
├── docs/                    # 需求与设计文档
├── index.html               # Vite 入口
├── package.json             # 前端清单（脚本统一走 bun）
├── src/                     # 前端：React + TS
│   ├── App.tsx              # 骨架页，调用后端 app_info 验证 IPC
│   └── main.tsx
├── src-tauri/               # Tauri 主进程
│   ├── Cargo.toml           # 含独立 [workspace]，避免被上层仓库 workspace 捕获
│   ├── capabilities/        # 能力白名单，当前只开 core:default
│   ├── src/lib.rs           # app_info 命令 + run()
│   ├── src/main.rs          # 二进制入口
│   └── tauri.conf.json      # 窗口尺寸、标识符 com.rustmobax.desktop
├── tsconfig.json
└── vite.config.ts           # 固定端口 1420，strictPort
```

`src-tauri/Cargo.toml` 里的 `[workspace]` 是必需的：本项目的上层目录
`/root/job/ai-code` 自带一个无关 workspace，不声明独立 workspace 会报
`current package believes it's in a workspace when it's not`。

## 五、骨架当前状态

| 项 | 状态 |
| --- | --- |
| Tauri 2 + React 19 + TS + Vite 结构 | ✅ 已生成并改造成 RustMobaX |
| 模板演示内容（greet、logo、示例资源） | ✅ 已清理 |
| 最小 IPC 通路（前端 `invoke("app_info")` ↔ 后端命令） | ✅ 代码就位，本机未能实跑（缺 webkit） |
| 前端类型检查 | ✅ 通过 |
| Vite 开发服务器 | ✅ 起得来，`/` 返回 200 |
| Rust 清单与格式 | ✅ `cargo metadata` / `cargo fmt --check` 通过 |
| Rust 类型检查与运行 | ❌ 受本机系统库限制 |

### 已知告警

`src-tauri/capabilities/default.json` 会报 `gen/schemas/desktop-schema.json` 不存在。
该文件由 `tauri-build` 在首次成功编译时生成，本机编不出来所以缺失，**不是错误**，
在能编译的环境里首次 `bun run tauri dev` 后自动消失。

## 六、远端构建（xiaojie）

本机编不出来时，桌面端构建在 SSH 主机 `xiaojie` 上完成（通过 ssh-mcp）。

| 项 | 值 |
| --- | --- |
| 主机 | `172.25.103.134:22`，用户 `huya`，sudo 免密 |
| 系统 | Ubuntu 22.04.5 LTS（Jammy） |
| 关键库 | glib 2.72.4、webkit2gtk-4.1 2.50.4、libsoup-3.0 3.0.7、gtk 3.24.33 |
| 工具链 | cargo 1.99.0、bun 1.4.2 |
| 工作目录 | `/home/huya/work/RustMobaX` |

### 一次性依赖安装

```bash
sudo /usr/bin/apt-get update -qq
sudo /usr/bin/apt-get install -y \
  pkg-config libwebkit2gtk-4.1-dev build-essential \
  curl wget file libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev patchelf
```

注意：该机初始**没有** `pkg-config`，缺它时 `glib-sys` 的探测会直接失败，先装它。

### 构建流程

```bash
# 本地打包（排除依赖与产物）
cd /root/job/ai-code
tar czf /tmp/rustmobax-src.tar.gz \
  --exclude=node_modules --exclude=target --exclude=dist --exclude=.git RustMobaX

# 上传解压（经 ssh-mcp 的 upload_file / execute_command）
cd /home/huya/work && tar xzf rustmobax-src.tar.gz

# 远端构建，长任务必须后台跑（MCP 调用会超时打断前台命令）
cd /home/huya/work/RustMobaX
nohup setsid bash -c 'bun install && bun run tauri build; echo "BUILD_EXIT=$?"' \
  > /tmp/rmx-build.log 2>&1 < /dev/null &

# 轮询
tail -5 /tmp/rmx-build.log
```

### 构建结果（2026-11-02）

`Finished release profile in 3m 07s`，`BUILD_EXIT=0`：

| 产物 | 路径 | 大小 |
| --- | --- | --- |
| 裸二进制 | `src-tauri/target/release/rustmobax` | 4.5 MB（stripped） |
| deb | `src-tauri/target/release/bundle/deb/RustMobaX_0.1.0_amd64.deb` | 1.76 MB |
| rpm | `src-tauri/target/release/bundle/rpm/RustMobaX-0.1.0-1.x86_64.rpm` | 1.76 MB |
| AppImage | `src-tauri/target/release/bundle/appimage/RustMobaX_0.1.0_amd64.AppImage` | 78.19 MB |

deb 的运行时依赖：`libwebkit2gtk-4.1-0, libgtk-3-0`。
deb 已回传到本地 `dist/RustMobaX_0.1.0_amd64.deb`（`dist/` 在 `.gitignore` 内）。

### 已知小问题

Tauri 由 `productName` 推导 deb 包名为 `rust-moba-x`（而非 `rustmobax`）。
安装不受影响，若要与包名对齐需调整 `productName` 或后续在 `bundle.linux` 里覆盖，
当前优先不动配置以免触发重编译。

## 七、E1 会话管理交付记录

用户故事 E1（`docs/user-stories.md`）已落地，代码分两层：

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 核心逻辑 | `crates/core`（`rustmobax-core`） | 不依赖 Tauri，可本机 `cargo test` |
| 桌面外壳 | `src-tauri/src/commands/` | 只做参数转发，无业务逻辑 |
| 界面 | `src/App.tsx`、`src/components/` | 菜单栏 + 工具栏 + 会话树 + 标签页 + 状态栏 |

### 已完成的故事

| 故事 | 状态 | 说明 |
| --- | --- | --- |
| US-101 新建并保存会话 | ✅ | 先选协议（SSH/SFTP/Shell），再填分区设置 |
| US-102 分组组织 | ⚠️ 部分 | 增删改、嵌套、折叠持久化已完成；**拖拽移动未做 UI**（后端 `move_group` 已就绪） |
| US-103 搜索 | ✅ | 匹配名称/主机/用户名/分组名，大小写不敏感 |
| US-104 复制派生 | ✅ | 自带 `副本`/`副本 N` 去重 |
| US-105 导入 .mxtsessions | ✅ | 先预览后落地，跳过项列出原因 |
| US-106 导出 | ⚠️ 部分 | 导出 JSON 成功；**「包含凭据」选项待 E6 凭据库** |

### extra 字段约定

会话的高级项统一序列化进 `session.extra`，当前结构：

```json
{
  "privateKeyPath": "/home/huya/.ssh/id_rsa",
  "x11Forwarding": true,
  "executeCommand": "cd /var/log && tail -f app.log"
}
```

空值不写入，全空时为 `null`。认证方式由是否填私钥推导（填了即 `key`，否则 `password`）。

### 验证记录（2026-11-02）

本机（无 GUI 能力）：

- `cargo test -p rustmobax-core` → **11 passed**，覆盖建组/建会话、必填校验、搜索、删组会话上移、防环、复制命名、解析、导入复用分组、导出脱敏
- `bun run typecheck` → 通过

远端 xiaojie（Ubuntu 22.04，`DISPLAY=:10`）：

| 验证点 | 结果 |
| --- | --- |
| 会话树渲染（嵌套分组 + 计数 + 主机名） | ✅ 截图确认 |
| 点击「新建会话」打开设置弹窗 | ✅ |
| 选择 SSH 后展示基本/高级设置 | ✅ |
| 手填私钥路径 + X11 转发后保存 | ✅ 落库 `extra` 正确 |
| 重新打开编辑回显私钥与 X11 | ✅ |
| 保存后新会话入树、标签打开、详情渲染 | ✅ |

### 已知限制

1. **文件选择器在无桌面集成环境不可用**：xiaojie 走 xrdp，`xdg-desktop-portal` 无响应，
   点「浏览…」后 5 秒会提示改为手填路径。Windows/macOS 与完整桌面 Linux 不受影响。
2. US-106 的「包含凭据」导出待 E6 凭据库落地。

## 八、本地终端（US-301）

### 链路

```
shell 子进程 ←→ PTY ←→ 读线程（8KB）→ base64 → Tauri Channel → xterm.js
                     ↑                                  ↓
                 写入端 ← base64 ← invoke ← 键盘输入
```

| 环节 | 位置 | 说明 |
| --- | --- | --- |
| PTY 会话 | `crates/core/src/terminal.rs` | `TerminalSession::spawn`，读线程持续回调，EOF 时回报退出码 |
| 命令层 | `src-tauri/src/commands/terminal.rs` | 终端注册表 + `Channel<TerminalEvent>` 推事件 |
| 面板 | `src/components/TerminalPanel.tsx` | xterm 实例 + FitAddon + ResizeObserver |
| 编码 | `src/api/terminal.ts` | 文本 ↔ base64 ↔ `Uint8Array` |

### 两个必须踩过的坑

**1. `TERM` 必须显式设置。** PTY 继承的 `TERM` 可能为空，此时 `less` 会打印
`WARNING: terminal is not fully functional`，`vim`/`htop` 也降级。`spawn` 时显式注入
`TERM=xterm-256color` 与 `COLORTERM=truecolor`。

**2. 拖拽不能用 HTML5 DnD。** Tauri 的 `dragDropEnabled`（默认开启，用于文件拖放）
会拦截 DOM 的 `dragstart`。会话/分组拖拽改用指针事件（`pointerdown/move/up` +
`elementFromPoint` + `data-group-id`）实现，这样既不影响拖拽排序，也保留了将来文件拖放的能力。

### 为什么用 base64

Tauri 2 的 `Channel` 只能推 `Serialize` 类型，传 `Vec<u8>` 会被序列化成 JSON 数字数组，
体积膨胀约 3–4 倍。base64 只放大 33%，且前后端都有原生 API（`btoa`/`atob`、`base64` crate）。

### 性能基线（实测，xiaojie `:10` 软件渲染）

**稳态与吞吐（2026-11-02 压测）**

| 场景 | 命令 | 结果 |
| --- | --- | --- |
| 开终端到可输入 | - | < 1 s |
| 彩色多列输出 | `ls --color=auto` | 正常，列宽与终端宽度一致 |
| alt-screen 分页 | `less /etc/services` | 正常，`q` 可退出 |
| **吞吐** | `time seq 1 300000`（2.1 MB） | **0.306 s ≈ 6.9 MB/s** |
| **不丢字节** | `seq 1 300000` | 末尾 `300000` 正确，无缺行 |
| 稳态 CPU | - | 0.8%–1.9% |
| 稳态内存 | - | RSS 162 MB（1 个会话） |

对照 `docs/architecture.md` 的目标（`cat` 10 MB 端到端 ≤ 2 s，即 5 MB/s）：**已达标**。

**突发压力（已修复）**

| 场景 | 修复前 | 修复后 |
| --- | --- | --- |
| `timeout 5 yes` 结束后恢复 | **16 s** 才渲完，期间像卡住 | **< 3 s** 回到提示符 |
| 峰值 RSS | 365 MB | 162 MB |
| 稳态吞吐 | 8.2 MB/s | 6.9 MB/s（-16%，换来突发可恢复） |

原因：读线程原本**每读到 8 KB 就推一次 IPC**，没有时间维度合并。`yes` 在 5 秒内灌入的数据
远超消费速度，调用在队列里堆积，前端要十几秒才追平。

修复方式：读线程只往共享缓冲塞字节（`Pump`），另起一个 8 ms 定时冲刷线程整块交付，
把「8 KB × N」合成「8 ms 内所有数据」一次推。交互输入最多多等 8 ms（低于人眼感知阈值），
大流量则把 IPC 调用数降一个量级。回归测试 `merges_burst_output_into_fewer_deliveries`
断言 288 KB 输出被合并到 25 次以内的推送。

### 拖拽移动（US-102 补齐）

- 会话可拖到任意分组，或拖到树底部「拖到此处可移到未分组」
- 分组可拖到其他分组下；拖进自己的子树会被后端拒绝并提示
- 拖拽中源行半透明，目标分组显示虚线高亮

## 九、SSH 会话（E2 进行中）

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 连接与认证 | `crates/core/src/ssh.rs` | `russh` + 常驻 tokio runtime，同步接口包装异步流程 |
| 统一控制面 | `TerminalControl` trait | 本地 PTY 与 SSH 各实现一次，命令层只认 trait |
| 命令层 | `src-tauri/src/commands/terminal.rs` | `start_ssh_terminal`，连接在阻塞线程池执行 |
| 前端 | `TerminalPanel` + `SecretPromptDialog` | 按协议选启动方式，SSH 先要密码 |

### 已完成

| 项 | 状态 |
| --- | --- |
| 密码认证 + 打开交互式 shell（US-201 主路径） | ✅ |
| 终端渲染 / 输入 / resize（复用本地终端面板） | ✅ |
| 认证失败与连接失败的分类文案 | ✅ |
| 密码仅存内存，关应用即失效 | ✅ |

### 实测记录（2026-11-02，xiaojie `:10`）

用临时账号 `rmxtest` 验证（验完已 `userdel -r` 删除）：

| 场景 | 结果 |
| --- | --- |
| 连 127.0.0.1 并显示远端登录 banner | ✅ 显示 Ubuntu 22.04.5 banner |
| 执行 `whoami; uname -sr` | ✅ 返回 `rmxtest` / `Linux 6.8.0-138-generic` |
| 错误密码 | ✅ 终端内提示「SSH 错误：认证失败：用户名或密码错误」 |
| 自包含单测（russh 起 echo 服务端） | ✅ `connects_with_password_and_receives_data` |

### 主机指纹校验（US-603，已完成）

`check_server_key` 现在做三态判定，不再无条件接受任意主机密钥：

| 状态 | 行为 |
| --- | --- |
| 指纹与 `known_host` 记录一致 | 放行 |
| 无记录（未知主机） | **阻断**，前端弹窗展示 SHA256 + MD5 供核对，确认后写库并重连 |
| 有记录但指纹不同 | **硬阻断**，前端红色告警，只提供「删除旧记录并重连」，不允许直接信任 |

存储：`known_host(host, port, key_type, fingerprint, updated_at)`，主键 `(host, port, key_type)`。
指纹格式与 OpenSSH 对齐，可用 `ssh-keygen -l -f` 对照：
`SHA256:base64`（无填充）与 `MD5:aa:bb:...`（16 组冒号分隔）。

实测记录（xiaojie `:10`，临时账号 `rmxtest`，验完已删除）：

| 场景 | 结果 |
| --- | --- |
| 首次连接 | 弹「未知主机」，双指纹展示完整 |
| 点「信任并连接」 | 落库 `SHA256:gG9VK37m...`，自动重连并显示远端 shell |
| 篡改库中指纹后重连 | 弹「主机密钥已变更」，红色告警，只给「删除旧记录并重连」 |

单测覆盖三态：`rejects_unknown_host_key_when_not_confirmed`、
`records_host_key_after_confirmation`、`blocks_when_fingerprint_changed`。

### 私钥认证（已完成）

| 项 | 说明 |
| --- | --- |
| 认证方式 | `SshAuth::{Password, PrivateKey}`，命令层用 `SshAuthRequest` 枚举透传 |
| 私钥来源 | 会话 `extra.privateKeyPath`（E1 时已存），无需重复配置 |
| 密钥解析 | `russh::keys::load_secret_key(path, passphrase)`，支持带口令的私钥 |
| 前端决策 | `resolveSshAuth()`：有私钥路径直接走 pubkey，否则弹密码框 |

实测（xiaojie，临时账号 + `ssh-keygen` 生成的 ed25519 密钥，验完已清理）：

| 场景 | 结果 |
| --- | --- |
| 点 authKind=key 的会话 | **不弹密码框**，直接进入指纹确认 |
| 信任后连接 | 用私钥认证成功，显示 `rmxtest@huya-B250M-D3H:~$` |
| 私钥文件不存在 | 「读取私钥失败（路径）：...」，明确指出路径问题 |

单测覆盖：`authenticates_with_private_key`、`reports_clear_error_when_key_missing`
（测试内部自建 echo 服务端并即时生成密钥，不依赖外部环境）。

### 尚未完成

| 项 | 说明 |
| --- | --- |
| 带口令私钥的交互输入 | 后端已支持 passphrase 参数，前端还没有输入入口 |
| known_hosts 导入导出 | 表与命令已就绪（`list_known_hosts` / `forget_host_key`），缺 UI |
| 凭据库的「临时保存」入口 | 密码框输完不能一键入库，必须先去「会话 → 凭据库…」录入 |

## 十、下一步

1. 带口令私钥的输入入口。
2. US-202/203/204 终端体验补齐（多标签已具备，缺分屏与搜索）。
3. E4 SFTP 文件浏览（依赖 E2 连接复用）。

## 十一、凭据库（E6，已完成）

目标：密码不再每次手输，但明文绝不落库。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 存储 | `crates/core/src/store.rs` | `credential`（密文）+ `vault_meta`（盐与校验块） |
| 加密 | `crates/core/src/vault.rs` | Argon2id 派生 + AES-256-GCM，密钥只驻内存 |
| 状态机 | `Vault` | `is_initialized` / `unlock` / `lock` / `save_secret` / `reveal` |
| 命令层 | `src-tauri/src/commands/vault.rs` | 7 个命令：状态、设置、解锁、锁定、增、查、删 |
| 认证接线 | `SshAuthRequest::Credential { id }` | 连接前才 `reveal` 成明文，其余路径不变 |
| 前端 | `VaultDialog` + `api/vault.ts` | 会话 → 凭据库…；会话设置在「凭据库密码」里选 |

### 加密参数

| 项 | 取值 | 说明 |
| --- | --- | --- |
| KDF | Argon2id，19 MiB / 2 轮 / 1 并行 | OWASP 最小推荐档，本机约 50ms |
| 盐 | 16 字节随机，base64 存 `vault_meta.salt` | 同一主密码不同库派生不同密钥 |
| 加密 | AES-256-GCM，12 字节随机 nonce 随密文存 | 每次保存都换 nonce，不复用 |
| 主密码校验 | 加密固定串 `rustmobax-vault-check` | 解得开即密码正确，不存密码本身 |
| 密钥生命周期 | `Zeroizing<Vec<u8>>`，锁定/退出即擦除 | 关应用后库里凭据重新变不可读 |

### 表结构

```sql
credential(id TEXT PK, kind TEXT, label TEXT, nonce BLOB, ciphertext BLOB, updated_at INTEGER)
vault_meta(k TEXT PK, v TEXT)   -- salt / check_nonce / check_ciphertext
```

会话侧不新增列：`session.extra` 里加 `credentialId`，与已有的 `privateKeyPath` 同级。

### 实测记录（2026-11-02，xiaojie `:10`）

临时账号 `rmxtest`（验完 `userdel -r` 已删），主密码 `master-pass-123`：

| 步骤 | 结果 |
| --- | --- |
| 会话 → 凭据库… → 设置主密码 | ✅ 弹窗切到「已解锁 · 0 条凭据」 |
| 新增凭据 `rmxtest-pw` | ✅ 状态栏提示「已保存凭据「rmxtest-pw」」 |
| 新建会话勾「凭据库密码」 | ✅ 下拉列出 `rmxtest-pw` |
| 保存会话后自动连接 | ✅ 只弹主机指纹，**没有密码框** |
| 信任指纹后 shell | ✅ `whoami` → `rmxtest`，`echo vault-ok` → `vault-ok` |
| 重启应用后打开该会话 | ✅ 状态「已锁定」，弹解锁框 |
| 输错主密码 | ✅ 红字「凭据库错误：主密码错误」，仍锁定 |
| 输对主密码 | ✅ 自动续上被暂缓的会话，直接登录 |

落库核对（`strings` + 逐字段）：

| 检查 | 结果 |
| --- | --- |
| `strings sessions.db \| grep Xk9mQ2vL7pz` | 0 命中 |
| `strings sessions.db \| grep WGs5bVEydkw3cHo=`（密码的 base64） | 0 命中 |
| `grep master-pass-123` | 0 命中 |
| `credential` 行 | nonce 12 字节，ciphertext 27 字节（12 字符明文 + 15 字节 GCM tag） |
| `session.extra` | 只有 `{"credentialId":"fad6ef4d-..."}` |

### 踩坑

| 现象 | 原因与处理 |
| --- | --- |
| 凭据库命令 401 一样报「终端不可用」 | 先核对 `AppState` 里 `vault` 是否用 `store.clone()` 构造 |
| 解锁后会话没自动打开 | `vault` state 在回调里是旧的，改为「成功回调里直接用暂存会话续开」而不是重跑 `openSession` |
| 主密码输错却显示「解密失败」 | `unlock` 把底层 GCM 错误统一改写成「主密码错误」，避免暴露实现细节 |

## 十二、终端体验（US-202 / US-203 / US-204）

本轮补齐分屏、搜索、粘贴保护三项，都落在同一个标签的终端面板内。

**验证状态**：三项在真机（xiaojie）走过一遍；搜索条 Esc 与 `Ctrl+V` 粘贴两个问题正是该轮验收发现，修复记在「十六」，本轮真机构建通过（`BUILD_EXIT=0`）并截图 5 张确认。多行粘贴 `[200~` 经字节级复测判定为误判，写入路径本身无缺陷。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 分屏状态 | `src/App.tsx` | `splitTabIds` 记录哪些标签处于分屏 |
| 分屏布局 | `src/App.css` | `.terminal-split` 左右两栏，各占一半宽 |
| 终端面板 | `src/components/TerminalPanel.tsx` | 每块终端各自建 xterm 实例，各自独立 PTY/SSH 连接与尺寸 |
| 搜索条 | `src/components/TerminalSearchBar.tsx` | `@xterm/addon-search`，上一个/下一个 + 命中计数 |
| 粘贴确认 | `src/components/PasteConfirmDialog.tsx` | 多行粘贴弹确认框，显示总行数 + 前 3 行预览 |
| 依赖 | `package.json` | 新增 `@xterm/addon-search@0.16` |

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 同一标签最多 2 块，左右排列 | 覆盖「一边看日志一边敲命令」，四宫格会让每块宽度压到 40 列以下 |
| 不做四宫格，不做拖拽比例 | 比例要持久化并处理最小尺寸，本轮收益低于成本 |
| 写入统一走 `terminal.paste()` | 自己拼字符串会在 bracketed paste 模式下漏掉成对的起始/结束标记 |
| 粘贴键交给浏览器默认 paste 事件 | xterm 会把 `Ctrl+V` 编码成 `0x16` 发给远端，`attachCustomKeyEventHandler` 因此对 `Ctrl/Cmd + V` 返回 `false` 且不 `preventDefault`，仍走多行确认框 / 单行直写 |
| 多行粘贴先确认 | 多行粘贴最容易误触远端命令，先给行数和预览让人停一下 |
| 搜索唤出入口有两个 | Ctrl+F 与面板按钮并行，键盘和鼠标习惯都能满足 |

### 已知限制

- 分屏固定左右各半，比例不可调；关标签时两块一起关。
- 两块终端各自独立连接，同一个会话分屏会占两条 SSH 连接。
- 单行粘贴不弹确认，直接写入。
- 搜索只做 addon 自带的命中高亮，没有正则/全库搜索选项。

## 十三、SFTP（E4）

文件浏览与文本编辑落地，二进制与整目录传输不在本轮范围内。

**验证状态**：core 侧新增 3 个测试通过，其中一条对自建 russh SFTP 服务端端到端。本轮与真实 OpenSSH 的 sftp-server 联调了默认目录与列目录：首屏、刷新、回到默认目录三种入口在真机构建（`BUILD_EXIT=0`）下走通并截图确认。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 协议 | `crates/core/src/sftp.rs` | 依赖 `russh-sftp`（`Cargo.toml` 声明 `3.0`，锁定 3.0.1） |
| 数据模型 | `SftpEntry` | `{ name, path, is_dir, size, permissions, modified }` |
| 客户端 | `SftpClient` | `{ connect, list, read_file, write_file, create_dir, remove, rename, close, is_closed }` |
| 命令层 | `src-tauri/src/commands/sftp.rs` | `sftp_connect/list/read_text/write_text/mkdir/remove/rename/close` |
| 前端 API | `src/api/sftp.ts` | 多词参数 JS 侧用 camelCase，如 `isDir` |
| 面板 | `src/components/SftpPanel.tsx` | 目录浏览、文本编辑、cwd 跟随 |

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 选 `russh-sftp` 3.0.1 | 从 2.0 起该 crate 把 russh 降为 dev-dependency，协议层只吃 `AsyncRead + AsyncWrite`，与 `russh 0.65` 不冲突；russh 的 `Channel::into_stream()` 正好对接 |
| 每个 SFTP 会话独占一条 SSH 连接 | 现有语义是「shell 结束即断开整条连接」，共享会破坏它，故 `sftp_connect` 自建连接 |
| cwd 跟随只认 OSC 7 | 远端 shell 不发 `file://host/path` 时无从得知目录，宁可保持手动导航 |
| 传输先只走文本通道 | 后端 `read_text_file` / `write_text_file` 只有文本通道，先保证改配置文件这条主线 |
| 默认目录一律传 `"."` | OpenSSH 的 sftp 子系统对空路径 `OPENDIR` 直接返回 `No such file`，空串不能表示家目录；`"."` 是 POSIX 当前目录，登录后即家目录 |

cwd 跟随的实现细节：`TerminalPanel` 用 `registerOscHandler(7, …)` 上报当前目录，SFTP 面板按 `用户名@主机:端口` 匹配。

默认目录的实现细节（`src/components/SftpPanel.tsx`）：

- `DEFAULT_DIR = "."`，首屏、刷新、回到默认目录、路径栏空输入回车一律传 `"."`。
- `joinPath` 从 `"."` 起算只返回条目名，不会拼出 `./xxx`。
- `parentPath` 在默认目录保持 `"."`，不会退成根目录。
- 默认目录不回填路径输入框，只显示占位 `~（默认目录，可输入绝对路径后回车）`。
- 列目录失败时同时给两条自救：可直接输入绝对路径（例如 `/home/<用户名>`）后回车，或点「回到默认目录」。

### 已知限制

- `sftp_read_text` 上限 1 MiB，且拒绝含 NUL 或非 UTF-8 的内容。
- `sftp_write_text` 由前端自限 2 MiB。
- 上传下载只支持 UTF-8 文本，二进制与整目录传输未做。
- 远端 shell 不发 OSC 7 时 cwd 不跟随，只能手动导航。
- 真实 OpenSSH 只联调了默认目录与列目录，二进制与整目录传输未验证。

## 十四、隧道（US-501 / US-502 / US-503）

本地转发、远程转发、SOCKS5 动态转发三条链路都在 core 里实现。

**验证状态**：新增 10 个测试通过，Local / Dynamic / Remote 各有一条真端到端（自建 russh 服务端）；未与真实 OpenSSH 联调。`src-tauri` 命令层只做过垫片 `cargo check`，真机未编译验证。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 核心 | `crates/core/src/tunnel.rs` | `TunnelKind{Local,Remote,Dynamic}`、`TunnelSpec` |
| 句柄 | `TunnelHandle` | `{ start, stop, describe, is_running, listen_port, spec }`，一个句柄一条隧道 |
| 远程转发 | `ClientHandler` 的 `forwarded-tcpip` 信箱 | `ForwardedChannel` 承接服务端回连 |
| 命令层 | `src-tauri/src/commands/tunnel.rs` | `tunnel_start/tunnel_stop/tunnel_list` |
| 前端 | `src/components/TunnelDialog.tsx`、`src/api/tunnel.ts` | 入口「会话 → 隧道…」 |

命令返回 `TunnelInfo{id,kind,describe,running,listenHost,listenPort,targetHost,targetPort}`。

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 远程转发必须在握手前装入信箱 | `forwarded-tcpip` 可能在认证后立刻到达，`ClientHandler` 构造完再塞会丢首包，因此有 `SshConnection::connect_async_with_forward` |
| SOCKS5 只做 CONNECT + 无认证 + IPv4/域名 | 覆盖浏览器与 `curl --socks5` 的常用路径；BIND 与 UDP 在本地转发里没有对应语义 |
| `listenPort` 以后端返回为准 | 0 表示自动分配，前端必须回读实际端口，不能自己记账 |
| 隧道列表只放内存 | 端口占用与连接状态无法跨重启恢复，落库只会制造陈旧条目 |

### 已知限制

- 目标不可达时只表现为本地连接被立刻关闭，UI 无法区分「拒绝」与「超时」。
- 隧道列表仅在内存，重启后失效。
- 无 keepalive，无自动重连。
- SOCKS5 不支持 BIND、UDP ASSOCIATE、IPv6 与用户名密码认证。
- 未与真实 OpenSSH 联调，仅对自建 russh 服务端验证。

## 十五、跳板机（US-505，一跳）

只做一跳：先连跳板机，再在跳板通道上向目标握手。

**验证状态**：新增 6 个测试通过，含经跳板连到目标、两类指纹错误可区分、拒绝嵌套跳板；均对自建 russh 服务端验证。前端类型与 UI 尚未接 `jump` 字段（见十六）。`src-tauri` 只做过垫片 `cargo check`，真机未编译验证。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 参数 | `SshOptions.jump: Option<Box<SshOptions>>` | 嵌套只允许一跳，否则返回 `Validation("只支持一跳跳板…")` |
| 握手主体 | `crates/core/src/ssh.rs` 泛型握手 | 接受任意 `AsyncRead + AsyncWrite + Unpin + Send` 流，纯 TCP 与跳板通道流共用同一套流程 |
| 入口 | `SshConnection::{connect_via, connect_via_async}` | 同步建连与带远程转发信箱的建连 |
| 命令层 | `SshStartRequest` / `SftpConnectRequest` / `TunnelStartRequest` | 各新增可选 `jump: Option<JumpRequest>`，缺省行为不变 |

`JumpRequest{host,port,username,auth,acceptUnknownHostKey}`。

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 只支持一跳 | 多跳要链式持有连接并逐层归属错误，先覆盖「办公网跳内网」这个主流场景 |
| 两层各校验一次指纹 | 跳板与被连主机是两条独立信任关系，known_hosts 按各自 `host:port` 记账 |
| 目标连接内部持有 `Arc<SshConnection>` 跳板 | 目标连接活着跳板就活着；断开时只有目标连接发 DISCONNECT，跳板靠引用计数回收 |
| 命令参数做成可选 | 老前端不传 `jump` 时行为与之前完全一致，后端先向后兼容 |

指纹归属：目标指纹未知时报 `HostKeyUnknown(info.port == 目标端口)`，跳板指纹未知时报 `info.port == 跳板端口`，前端据此提示是哪一层。

### 已知限制

- 前端类型与 UI 尚未接 `jump` 字段，目前只能从命令层直接调用。
- 嵌套跳板一律拒绝，错误文案固定，不支持两跳以上。
- 未与真实 OpenSSH 联调，仅对自建 russh 服务端验证；隧道转发本身未真机逐条复测（本轮真机构建通过，但只验证了终端与 SFTP 三项）。

## 十六、缺陷修复与已知缺口

真机体验收后修的五项（本轮真机构建 `BUILD_EXIT=0`，截图 5 张）：

| 缺陷 | 处理 |
| --- | --- |
| SFTP 首屏传空串必然报错 | 空路径 `OPENDIR` 在 OpenSSH 上直接失败，真机报错原文 `Sftp 错误: 列目录  失败：No such file: No such file`；改为 `SftpPanel.tsx` 的 `DEFAULT_DIR = "."`，首屏、刷新、回到默认目录、空输入回车统一传 `"."`，占位改 `~（默认目录，可输入绝对路径后回车）`，失败区同时给「输入绝对路径后回车」与「回到默认目录」 |
| 搜索条 Esc 关不掉 | 输入框 Esc 加 `stopPropagation`；终端侧 `attachCustomKeyEventHandler` 在搜索打开时吃掉 Esc |
| 会话校验不定位 | 四个校验分支加 `focus()` + `scrollIntoView({block:'center'})`，并补端口范围校验（1 到 65535） |
| `Ctrl+V` 在终端里粘不进内容 | 真缺陷：xterm 把 `Ctrl+V` 当 C0 输入编码成 `0x16` 发给远端，屏幕上出现 `^V`，只有 `Ctrl+Shift+V` 才粘贴。`TerminalPanel.tsx` 的 `attachCustomKeyEventHandler` 对 `Ctrl/Cmd + V`（带不带 Shift 一样）返回 `false` 且不 `preventDefault`，交给浏览器默认 paste 事件，仍走既有的「多行确认框 / 单行直写」，两个组合键现在都能用 |
| 多行粘贴出现字面量 `[200~` | 已排除，非缺陷：写入路径本来就是 `terminal.paste()`。远端启用 bracketed paste（`printf '\033[?2004h'`）时 `od -c` 输出 `033 [ 2 0 0 ~ a b c \n d e f \n 033 [ 2 0 1 ~`，ESC 完整存在未被丢弃；远端未启用时终端只收到裸文本，`cat -v` 看不到 `^[[200~`，属正常行为而非包裹丢失。上一轮把字面量当缺陷是误判 |

### 尚未完成

| 项 | 说明 |
| --- | --- |
| 前端 jump 字段与 UI 接线 | 后端已向后兼容，界面还没有跳板机入口 |
| SFTP 二进制与整目录传输 | 只有文本通道，二进制与递归传输未做 |
| 私钥口令凭据自动复用 | 需后端按 id 取明文，或 `SshAuthRequest` 支持口令凭据 |
| 隧道持久化 | 列表只在内存，重启失效 |
| 首帧闪一次暗色 | 主题在 React 挂载后才写到 `html[data-theme]`，`index.html` 里的内联预读未改，冷启动可能看到一帧暗色 |
| `app_setting` 无删除命令、无 CAS | 只做整值覆盖与 last-write-wins，缺省值由前端解释，目前没有删键需求 |
| 浅色图标未做专用调色板 | 靠 CSS `filter: brightness(0.62)` 压暗，实测 4.14 到 6.26:1，够用但不是设计值 |
| Menlo / Consolas 在 Linux 常缺失 | 字体栈回落到下一档，四档预设里这两档在 Linux 上通常不生效 |
| 快捷键仍硬编码 | 没有自定义入口，设置弹窗里也没有快捷键分区 |
| US-506 及以后 | 未开始 |

## 十七、设置、主题与外观

外观参数从「写死在样式和组件里」改成「落库 + 即时生效」，覆盖主题、界面缩放、终端字号、终端字体四项。主题从两套写死升级为「注册表 + 内置多套 + 导入自定义」。

**验证状态**：core 侧 59 个测试通过（设置 3 个、自定义主题 6 个）。真机（xiaojie `:10`）已验证：5 套内置主题切换生效、`theme=tokyonight` 写入 `app_setting`、设置弹窗三分区渲染、缩放与终端字号即时生效、重启后设置保留。`src-tauri` 命令层随 Linux 打包构建通过（BUILD_EXIT=0），也随 Windows 交叉构建通过。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 设置表 | `crates/core/src/store.rs` 的 `migrate()` | `app_setting(k TEXT PRIMARY KEY, v TEXT NOT NULL)` |
| 主题表 | 同上 | `custom_theme(id, name, kind, payload, created_at)`，payload 是原始主题 JSON |
| 读写 | `Store::settings_get_all` / `settings_set` | 按 key 升序返回，UPSERT 覆盖；key 不超过 64 字节、value 不超过 4096 字节 |
| 主题存取 | `Store::theme_list_custom` / `theme_save_custom` / `theme_delete_custom` | name 不超过 64 字节、payload 不超过 16384 字节且必须是 JSON 对象 |
| 命令层 | `src-tauri/src/commands/settings.rs`、`theme.rs` | `settings_get_all` / `settings_set` / `theme_list_custom` / `theme_save_custom` / `theme_delete_custom` |
| 主题注册表 | `src/lib/themes.ts` | `BUILTIN_THEMES`（5 套）、`applyTheme`、`themeToXterm`、首帧缓存 `cacheTheme` |
| 导入校验 | `src/lib/themeImport.ts` | `parseThemeFile` 纯函数，逐字段给出中文错误 |
| 设置界面 | `src/components/SettingsDialog.tsx` | 入口「设置 → 首选项…」，分区：外观 / 终端 / 文件面板 / 底部按钮 |
| 首帧预读 | `index.html` 内联脚本 | 读 `localStorage["rustmobax.themeCache"]` 先写 `data-theme` 与 11 个变量，避免浅色用户闪一帧暗色 |

### 设置键约定

| 键 | 取值 | 缺省 |
| --- | --- | --- |
| `theme` | `dark` / `light` / `island-dark` / `tokyonight` / `one-dark` / `system` / `custom:<uuid>` | `dark` |
| `theme.systemDark` | 任一暗色主题 id | `dark` |
| `theme.systemLight` | 任一浅色主题 id | `light` |
| `ui.scale` | 0.85 到 1.25 | `1` |
| `terminal.fontSize` | 10 到 24 | `13` |
| `terminal.fontFamily` | `default` / `menlo` / `consolas` / `mono` | `default` |
| `sidebar.defaultTab` | `sessions` / `tools` / `macros` / `sftp` | `sessions` |
| `sidebar.autoConnect` | `0` / `1` | `0` |
| `sftp.width` | 260 到 520 | `320` |
| `sftp.followCwd` | `0` / `1` | `1` |

`sftp.dock` 与 `sftp.autoOpen` 已废弃（改为十八的左侧栏分页与 `sidebar.*`），代码不再读取，库里旧值保留不动。

### 内置主题

| id | 名称 | 背景 | 面板 | 强调 | 正文 |
| --- | --- | --- | --- | --- | --- |
| `dark` | 暗色 | `#16181d` | `#1c1f26` | `#4a9eff` | `#d7dae0` |
| `light` | 浅色 | `#f5f6f8` | `#ffffff` | `#2563eb` | `#1b1f27` |
| `island-dark` | Island Dark | `#1e1f22` | `#2b2d30` | `#3574f0` | `#dfe1e5` |
| `tokyonight` | Tokyo Night | `#1a1b26` | `#1f2335` | `#7aa2f7` | `#c0caf5` |
| `one-dark` | One Dark | `#282c34` | `#2c313a` | `#61afef` | `#abb2bf` |

每套主题只给 11 个基础色，另约 20 个派生色由 `mix()` 按明暗方向算出；xterm 配色由 `themeToXterm` 从基础色加四个社区色种子派生，不手写 16 色。

### 自定义主题导入

```json
{
  "name": "我的主题",
  "kind": "dark",
  "colors": { "bg": "#16181d", "bgChrome": "#1b1e24", "bgPanel": "#1c1f26", "bgHover": "#23272f",
              "bgActive": "#2b3f57", "border": "#2a2e37", "borderStrong": "#3c424e",
              "text": "#d7dae0", "textDim": "#8b919d", "accent": "#4a9eff", "danger": "#ff6b6b" },
  "terminal": { "background": "#16181d", "foreground": "#d7dae0", "cursor": "#4a9eff" }
}
```

| 规则 | 行为 |
| --- | --- |
| 文件不超过 16 KB | 超出直接拒绝并给出字节数 |
| 最外层是 JSON 对象 | 否则报解析失败原文 |
| `colors` 的 11 个键必填 | 缺哪个报哪个，例如「缺少 colors.text」 |
| 颜色只接受 `#rgb` 或 `#rrggbb` | 非法值报出字段名与值 |
| `terminal` 可省略 | 省略时由 `colors` 派生，ansi 色用内置 `dark` 兜底 |
| 主题只是颜色数据 | 只做 `JSON.parse` 与正则校验，不 eval、不写 innerHTML，不携带脚本能力 |

导入入口在设置弹窗外观区「导入主题…」，选 `.json` 后读文件、校验、存库、立即应用并写入 `theme=custom:<uuid>`；自定义主题在列表里可删除，删除当前主题时回落到 `dark`。

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 设置进库，不进 localStorage | 要跨标签、跨重启一致；localStorage 只留主题首帧缓存 |
| 主题用运行时 CSS 变量而非 `:root[data-theme]` 枚举 | 自定义主题无法预先写进 CSS 文件，只能运行时 `setProperty` |
| 保留 `:root[data-theme]` 两套 | 首帧脚本在 React 之前就要有颜色可用 |
| 切主题只改 `term.options.theme` | 重建终端会丢滚动缓冲与连接状态 |
| 缩放只缩字号，间距保持 px | 间距一起缩会让行高与边框在 125% 下失真 |
| 主题 payload 存独立表 | `app_setting` 的 value 上限 4096 字节，装不下完整主题 JSON |

### 对比度（本机脚本按 WCAG 相对亮度计算）

| 主题 | 正文/背景 | 次要文字/背景 | 强调/背景 |
| --- | --- | --- | --- |
| dark | 12.68 | 5.61 | 6.45 |
| light | 15.27 | 6.95 | 5.10 |
| island-dark | 12.59 | 6.30 | 3.85 |
| tokyonight | 10.59 | 5.02 | 6.79 |
| one-dark | 6.57 | 3.73 | 5.92 |

### 已知限制

- One Dark 的 `--textDim`（官方色 `#7f848e`）对背景 3.73，低于 4.5；该色只用于提示与元信息，正文走 `--text`。
- 首帧缓存可能短暂过期：删除自定义主题后的第一帧沿用缓存色，随即被真实设置纠正。
- 自定义主题不做对比度强制，用户可导入 1:1 的配色。
- 浅色图标靠 `filter: brightness(0.62)` 压暗，没有浅色专用调色板。
- Menlo / Consolas 在 Linux 常缺失，回落到下一档。
- `app_setting` 与 `custom_theme` 都没有删除键/回收站语义，只做整值覆盖。
- 快捷键仍硬编码，没有自定义入口。

## 十八、左侧栏分页（Sessions / Tools / Macros / SFTP）

左侧栏改成 MobaXterm 形态：32px 竖排图标栏 + 内容区，四个分页切换，SFTP 作为其中一页常驻。

**验证状态**：真机已验证四分页渲染、工具页分组、宏页空态、SFTP 页在家目录下列出条目（`.cache/`、`.config/`、`snap/`、`.bash_logout`、`.bashrc`、`.profile`）且窄宽切紧凑布局、`⋯` 行操作可用；侧栏宽度写入 `sftp.width`。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 图标栏 | `src/components/SidebarRail.tsx` | 32px，四项，可 Tab 聚焦、上下方向键切换，选中态为背景 + 3px 强调条 + `aria-selected` |
| 宽度拖动 | `src/components/SidebarResizer.tsx` | 7px 拖动条，260 到 520，松手写 `sftp.width` |
| 会话页 | `src/App.tsx` | Quick connect、搜索、会话树、底部「远程监控 / 跟随终端目录」 |
| 工具页 | `src/components/ToolsPanel.tsx` | 分组列表，见下表 |
| 宏页 | `src/components/MacrosPanel.tsx` | 空态 + disabled「开始录制」 |
| 文件页 | `src/components/SftpPanel.tsx` | 沿用十三的目录浏览与文本编辑，绑当前活动会话 |

### 工具页条目

| 分组 | 条目 | 行为 |
| --- | --- | --- |
| 系统 | 凭据库… / 已知主机… / 首选项… | 打开对应弹窗 |
| 网络 | 端口转发（隧道）… / 打开文件面板 | 打开隧道弹窗 / 切到文件页 |
| 编辑器 | 内置文本编辑器 | 切到文件页并提示双击文件打开 |
| 未实现 | MobaDiff / ASCII 表 / 端口扫描 / 网络唤醒 | 灰显，第二行写「尚未实现」 |

### 文件页行为

- 绑定 `activeSession`：`ssh` 与 `sftp` 会话可连；`local` 或没有活动会话时显示空态「先在「会话」分页里打开一个 SSH 会话」，不建连接。
- 认证复用终端的 `sshAuths` / `sshTrusted` 与指纹三态流程；缺密码、私钥口令、凭据库锁定时分别给出对应入口。
- 分页切换不卸载：切到别的页只做 CSS 隐藏，回来不重连；活动会话变化才卸载旧面板（卸载时发 `sftp_close`），新会话重新懒加载。
- 窄宽适配：面板宽度小于 360px 时切紧凑表格，只保留名称列，行操作收进「⋯」，操作列固定 36px，不出现横向滚动条。

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 用侧栏分页，而不是标签内停靠 | 对齐 MobaXterm 的信息架构；文件面板与终端同屏，切页不打断终端；也避免每个会话标签各自维护停靠态 |
| SFTP 常驻且切页不卸载 | 隐藏不等于断开，重连要多付一次握手与指纹校验 |
| 宽度进设置表 | 换机器/重启后仍一致；localStorage 只做主题首帧缓存 |
| 活动会话变化才重连 | 按会话 id 作 `key`，旧面板卸载回收连接，不会在多会话间串用 |
| 未实现项也列出来但禁用 | 保持与 MobaXterm 同样的分组结构，同时不伪造可用功能 |

### 已知限制

- 每个活动会话一条独立 SFTP 连接，不与终端共享 TCP（原因见十三）。
- 窄宽下看不到大小与修改时间，需要加宽侧栏才能看到。
- 会话页底部「远程监控」仍是占位，未接真实 CPU/内存/网络数据。
- 分页默认值只对启动时生效一次，之后以用户当前选择为准。

## 十九、本机指标、可配置快捷键与二进制传输

补三块「日常会用但要看到真实数据」的能力：状态栏与侧栏的实时指标、可录制的快捷键、文本之外的二进制与整目录传输。

**验证状态**：core 侧 61 个测试通过（新增指标 2 个）。真机（xiaojie `:10`）已验证：状态栏 `CPU 20% / 内存 72% / 网络 ↑1.1MB/s ↓1.0KB/s` 实时刷新、设置弹窗「快捷键」分区列出 8 个动作与当前组合、录制与重置按钮可用。Windows 交叉构建的产物见下。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 采样器 | `crates/core/src/metrics.rs` | `MetricsSampler`：保留上次 CPU 时间片与网卡字节，按差值算速率 |
| 指标命令 | `src-tauri/src/commands/metrics.rs` | `system_metrics()`，采样器挂在 `AppState.metrics`（`Mutex`） |
| 前端轮询 | `src/App.tsx` | 每 2 秒采一次，窗口不可见时跳过；状态栏芯片与侧栏「远程监控」共用同一份 |
| 键位表 | `src/lib/shortcuts.ts` | 规范化、匹配、冲突检测、默认键位，纯函数 |
| 键位存储 | 设置键 `shortcuts` | 一段 JSON，动作 id 到组合；解析时非法值与重复绑定回落到缺省 |
| 键位分发 | `src/App.tsx` | window 捕获阶段监听，输入框/文本域聚焦时不拦，终端内搜索与粘贴交给终端 |
| 键位界面 | `SettingsDialog` 的「快捷键」分区 | 录制、单项重置、冲突标红 |
| 二进制通道 | `src-tauri/src/commands/{sftp,files}.rs` | `sftp_read_file_base64` / `sftp_write_file_base64` / `read_binary_file` / `write_binary_file` / `list_local_dir` |
| 传输界面 | `src/components/SftpPanel.tsx` | 上传、上传目录、下载、整目录下载，带进度文案 |

### 指标口径

| 项 | 口径 |
| --- | --- |
| CPU 使用率 | `/proc/stat` 第一行，`(total - idle)` 除以时间片差 |
| 内存使用率 | `MemTotal - MemAvailable`，比单看 `MemFree` 更贴近真实占用 |
| 网络速率 | `/proc/net/dev` 汇总非 `lo` 网卡的 rx/tx 字节，除以采样间隔 |
| 首次采样 | 没有基线，速率返回 0，不编造数据 |
| 非 Linux | `supported = false`，界面显示占位而不是假数字 |

### 快捷键默认值

| 动作 | 默认组合 | 说明 |
| --- | --- | --- |
| 新建会话 | Ctrl+N | 打开会话设置弹窗 |
| 新建分组 | Ctrl+Shift+N | 在当前分组下建子分组 |
| 导入会话 | Ctrl+O | 导入 MobaXterm 会话文件 |
| 关闭标签 | Ctrl+W | 关闭当前激活标签 |
| 终端内搜索 | Ctrl+F | 由终端处理，不进全局分发器 |
| 分屏 | Ctrl+\ | 当前终端标签左右分屏 |
| 显示/隐藏侧栏 | Ctrl+B | 收起或展开左侧栏 |
| 首选项 | Ctrl+, | 打开设置弹窗 |

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 采样器放后端、前端只轮询 | 差值需要基线，放前端等于每次重开会话都从 0 开始；后端常驻最省事 |
| 指标与快捷键都进设置表 | 与十七的其余外观项同一条链路，重启后一致 |
| 快捷键在捕获阶段拦 | 终端节点会吃掉按键，冒泡阶段拦不住；捕获阶段先判断焦点位置再决定 |
| 输入框一律不拦 | 快捷键在输入名称、路径时抢键是最烦人的体验问题 |
| 二进制走 base64 过 IPC | Tauri Channel 的 `Vec<u8>` 会被序列化成数字数组，体积膨胀数倍 |
| 单文件上限 32 MiB、远端读 8 MiB | base64 会放大约三分之一，再大就该改成分片，本轮先把边界写死 |

### 已知限制

- 整目录下载不会创建空目录（写文件时自动建父目录，空目录会被跳过）。
- 传输是单文件串行，没有并发与断点续传，进度只在面板顶部有一行文案。
- 二进制上限硬编码，超出时提示改写分片方案才能支持。
- 快捷键只覆盖 8 个动作，终端内的搜索与粘贴固定由终端处理，不可改。
- 非 Linux 平台的指标还未实现（Windows 需要 `GetSystemTimes` 等另一套采样）。

## 二十、指标改为远端口径，SFTP 明确远端身份

十九那版把 CPU / 内存 / 网速采在了**运行客户端的那台机器**上，和「会话相关的资源应该看远端」的直觉相反；SFTP 面板虽然一直列的是远端目录，但界面上没有一处写明，容易被当成本机文件。这一章把两处都纠正清楚。

**验证状态**：真机（xiaojie `:10`）已验证：连上 `127.0.0.1` 的 SSH 会话后，状态栏显示 `远程 127.0.0.1 · CPU 33% · 内存 72% · 网络 ↑0 KB/s ↓0 KB/s`，侧栏「远程监控」标题同样标 `远程 127.0.0.1`；切到 Windows 远端（chen-pc，无 `/proc`）时显示「远端不可用」，不回落本机数字；文件分页顶部显示 `[远端] rmxtest@127.0.0.1:~`。core 侧 70 个测试通过。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 远端 exec | `crates/core/src/ssh.rs` | `SshConnection::exec(command, max_bytes)`：在同一条已认证连接上开 exec 通道收 stdout/stderr，带 20 秒超时 |
| 连接可见性 | `TerminalControl::connection()` | 默认 None，SSH 会话返回 `Some`，本地 PTY 保持 None |
| 远端采样 | `crates/core/src/metrics_remote.rs` | `sample_remote(connection, host)`：一次 exec 跑 POSIX 脚本，脚本内双采样算 CPU 与网速 |
| 命令 | `src-tauri/src/commands/metrics.rs` | `remote_metrics(terminalId)` 返回 `{ metrics, host, scope }`；`system_metrics()` 保留为本机口径 |
| 前端取数 | `src/App.tsx` | 活动标签是 SSH 会话就取远端，否则取本机；切标签立刻重采 |
| 终端 id 映射 | `TerminalPanel` 的 `onTerminalId` 回调 | 后端注册表里的终端 id 是随机 UUID，不是会话 id，必须由面板上报 |
| 来源标注 | 状态栏 `metric-source` 与侧栏 `monitor-source` | `远端 <host>` / `本机` / `远端不可用`，文字与边框双重区分，不只靠颜色 |
| 远端标识 | `src/components/SftpPanel.tsx` | 顶部常驻 `[远端] user@host:路径`，上传下载按「本机 → 远端 / 远端 → 本机」表述 |

### 远端采样脚本（一次 exec，脚本内自己算差值）

```sh
if [ ! -r /proc/stat ] || [ ! -r /proc/meminfo ] || [ ! -r /proc/net/dev ]; then echo "PROC_UNAVAILABLE" >&2; exit 3; fi; s1=$(head -1 /proc/stat); n1=$(awk -F'[: ]+' '!/lo/ && NF>10 {rx+=$3; tx+=$11} END{print rx+0, tx+0}' /proc/net/dev); sleep 0.3; s2=$(head -1 /proc/stat); n2=$(awk -F'[: ]+' '!/lo/ && NF>10 {rx+=$3; tx+=$11} END{print rx+0, tx+0}' /proc/net/dev); awk -v a="$s1" -v b="$s2" 'BEGIN{...}'; awk '/MemTotal/{t=$2}/MemAvailable/{a=$2}END{...}' /proc/meminfo; awk -v a="$n1" -v b="$n2" 'BEGIN{...}'
```

输出固定格式 `CPU <百分比> MEM <总量kB> <可用kB> NET <下行KiB/s> <上行KiB/s>`，后端按标签解析，缺段或非数字直接报解析失败，不猜默认值。

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 远端失败不回落本机 | 回落会把本机数字伪装成远端数据，是比「没有数据」更糟的错；界面只显示不可用与原因 |
| 差值在远端脚本里算 | 这样后端不需要为每台主机维护基线，也不会因轮询间隔抖动错配基线 |
| 指标绑定终端 id 而非会话 id | 后端注册表用的是随机 UUID；会话 id 只在前端有意义，二者由 `onTerminalId` 显式对应 |
| 本地终端仍显示本机指标 | 本地 PTY 没有远端概念，硬套远端口径会误导；来源标注已写明「本机」 |
| SFTP 只加标注不动逻辑 | 面板本来就列远端目录，问题在可读性；改文案与表头比重写传输链路稳 |

### 已知限制

- Windows / macOS 远端没有 `/proc`，远端指标显示「远端不可用」，没有各平台的替代采样实现。
- 内核早于 3.14 无 `MemAvailable`，内存会被算成接近 100%。
- 极老 BusyBox 的 `sleep` 只支持整数秒，脚本会以非 0 退出并提示不可用。
- 每次轮询在远端固定占 0.3 秒并起 4 个进程，轮询间隔小于 2 秒或同主机多开终端会叠加开销。
- 网速除数固定 0.3，远端负载高时实际间隔偏长，速率会略偏低。
- 文件分页的上传下载仍是「本机文件选择器 ↔ 远端 SFTP」的单文件串行，没有断点续传与并发。

## 二十一、文件分页：磁盘占用、流式传输与拖拽上传

这一轮解决三个实际问题：面板文字太啰嗦、上传下载有看不见的大小上限、看不到远端磁盘还剩多少。

**验证状态**：core 侧 93 个测试通过（原 81 个；本轮新增断点续传/取消/临时文件/属性/符号链接 8 个、Windows 磁盘兜底 4 个）。真机（xiaojie `:10`）已验证：文件分页顶部显示 `[远端] rmxtest@127.0.0.1:~`，磁盘条显示 `27.6 GiB / 233.2 GiB · 12%`，底部说明压成一行，工具栏合并为「上传 ▾」。60 MiB 文件的界面端到端上传未跑（xrdp 下系统文件选择器不稳定），流式正确性由 5 MiB 往返单测保证。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 流式上传 | `SftpClient::upload_file_from_with(remote_path, local_path, chunk, resume, progress, cancel)` | 本地 `File` 分块读，远端先写 `<目标>.rmxpart`，成功后改名；返回 `TransferOutcome` |
| 流式下载 | `SftpClient::download_file_to_with(remote_path, local_path, chunk, resume, progress, cancel)` | 远端 `open` 流式读，本地先写 `<目标>.rmxpart`，成功后改名 |
| 薄封装 | `upload_file_from` / `download_file_to` | 固定「不续传、无进度、永不取消」，只为兼容旧调用点 |
| 断点续传 | `resume = true` + 残留 `.rmxpart` | 以残留大小为起点续写；取消保留残留，其它失败尽量清理 |
| 分块大小 | `DEFAULT_TRANSFER_CHUNK = 256 KiB` | 与 russh-sftp 单包上限对齐，一块一个请求 |
| 磁盘占用 | `disk_usage(connection, path) -> DiskUsage` | `df -Pk` 解析末行；df 失败时用 PowerShell 兜底（Windows 远端） |
| 命令 | `sftp_upload_local_file` / `sftp_download_remote_file` / `sftp_cancel_transfer` / `remote_disk` | 前两条返回 `{written,total,resumedFrom}` 并经 `Channel<TransferEvent>` 推进度，第三条按 `cancelToken` 取消，`remote_disk` 返回 `{usedBytes,totalBytes,freeBytes,percent,mount}` |
| 拖拽上传 | `src/components/SftpPanel.tsx` | Tauri 原生 `onDragDropEvent`，按 `devicePixelRatio` 换算坐标后 `elementFromPoint` 命中面板才接管 |
| 磁盘条 | 同上 | `已用 / 总量 · 百分比` + 4px 进度条，75% / 90% 两档换色，文字与水位词同时表达 |

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 传输改为后端按本地路径直读直写 | 走 base64 过 IPC 必然要把整文件塞进内存并设上限；直读直写后峰值内存与文件大小无关 |
| 峰值内存约 0.8 MiB（上传）/ 4.3 MiB（下载） | 256 KiB 缓冲 + 库内 16 个在途请求，千兆网与 5 GiB 文件都够用 |
| 传输不设总时长上限 | 设了就等于隐形大小上限；卡死由 russh-sftp 的 10 秒单请求超时兜底 |
| 磁盘百分比自己算 | `df` 的 Capacity 列四舍五入到整数，自己按 `used/total` 算能对上进度条 |
| 拿不到磁盘信息就整行隐藏 | 显示 0 或沿用旧值会让用户误判剩余空间 |
| 拖拽用原生事件 | WebView 里 HTML5 `drop` 拿不到真实本地路径，没法交给后端直接读；原生事件给绝对路径 |
| 命中判定用 `elementFromPoint` | 原生事件只给物理坐标，必须先换算再判断是否落在面板上，否则拖到终端区域也会被当成上传 |

### 界面口径

| 元素 | 改动 |
| --- | --- |
| 底部说明 | 两段压成一行「远端目录 · 双击进入，双击文件编辑 · 可直接拖入本机文件」，细节挪进按钮 `title` |
| 上传按钮 | 「上传 ▾」分体：上传文件 / 上传整个目录，工具栏不再挤成两行 |
| 远端标识 | `[远端] user@host:路径`，过长中间省略，`title` 给完整值 |
| 磁盘条 | `27.6 GiB / 233.2 GiB · 12%`，低于 75% 用强调色，75% 到 90% 用琥珀，超过 90% 用告警色 |
| 大小上限文案 | 面板内全部删除（编辑器仍保留 2 MiB，且只在编辑器里提示） |

### 已知限制

- 不支持断点续传：上传失败会在远端留下半截文件，下载失败会在本地留下半截文件（没有 `.part` 临时名与 rename 收口）。
- 没有分片级进度事件：后端只在文件完成时返回字节数，超大单文件传输期间进度文案不跳动，也没有取消入口。
- 整目录传输仍是前端串行遍历，几万个文件会很慢，中途失败即停。
- 上传不继承本地权限位与 mtime，使用服务端默认 umask。
- 远端符号链接会被跟随写入，未做拒绝。
- Windows 远端没有 `df`，磁盘条整行隐藏（界面按错误降级，不显示 0）。
- `df` 口径差异：部分文件系统 `Used + Available` 不等于总量（保留块、快照），`freeBytes` 与 `total-used` 可能对不上。

## 二十二、SFTP 断点续传、进度与取消

二十一那版把传输搬到后端直读直写，解决了「隐形大小上限」，但传输本身仍是全有全无：中断就在目标位置留半截文件，过程中既没有进度也没有取消入口。这一轮把传输补成可观测、可中止、可接着传——目标文件名下永远只出现完整文件，进度经 Tauri `Channel` 实时推送，取消后能凭残留的 `.rmxpart` 从断点续上。

**验证状态**：真机（xiaojie `:10`）端到端跑通 60 MiB 上传，最终 62,914,560 字节，md5 与源文件一致（`16c35c6efa981919052fc9aeb31e221e`），远端权限 664、mtime 与本机一致；把链路限速到 30mbit 后在 18.3 MiB 处取消，远端留下 19,398,656 字节的 `.rmxpart`，改用 `resume = true` 续传后最终仍是 62,914,560 字节、md5 不变。core 侧新增回归用例 `sftp::tests::upload_succeeds_inside_spawn_blocking`，把「阻塞线程里上传」这条命令层的真实路径固化下来。

### 分层

| 层 | 位置 | 说明 |
| --- | --- | --- |
| 续传上传 | `crates/core/src/sftp.rs` `upload_file_from_with(remote, local, chunk, resume, progress, cancel)` | 本地分块读 → 远端先写 `<目标>.rmxpart` → 全部写完才 RENAME 成目标名 |
| 续传下载 | 同文件 `download_file_to_with(...)` | 远端流式读 → 本地先写 `<目标>.rmxpart` → 写完再本地改名 |
| 临时文件名 | `PART_SUFFIX = ".rmxpart"` + `remote_part_path` / `local_part_path` | 与目标同目录，收尾的 RENAME 不跨目录；本地后缀在 `OsString` 上追加，非 UTF-8 路径不丢字符 |
| 返回值 | `TransferOutcome { written, total, resumed_from }` | `written` 是本次写入量，`resumed_from` 是本次起点，两者相加才是文件大小 |
| 薄封装 | `upload_file_from` / `download_file_to` | 固定「不续传、无进度、永不取消」，只为兼容旧调用点 |
| 分块 | `DEFAULT_TRANSFER_CHUNK = 256 KiB` | 与 russh-sftp 单包上限对齐，一块一个写请求 |
| 取消标志 | `AtomicBool`，每块写之前检查一次 | 命中后先 `close()` 把已写数据冲给服务端，再保留 `.rmxpart` 返回取消错误 |
| 进度通道 | `src-tauri/src/commands/sftp.rs` `Channel<TransferEvent>` | `Progress{bytes}` / `Done{bytes}` / `Error{message}`，按「1 MiB 或 200 ms」节流，结束事件不节流 |
| 取消命令 | `sftp_cancel_transfer(cancelToken)` + `AppState.transfers` | 前端每次传输生成令牌，命令把取消表里的标志置真；令牌已移除时什么也不做 |
| 属性继承 | `inherited_attrs` + `set_metadata` | 上传完成后把本地 mode 与秒级 mtime 写到远端目标；失败只记日志 |
| 符号链接 | 上传 `symlink_metadata` 判 `is_symlink`，下载 `LSTAT` 判 `is_symlink` | 命中直接报错，不做跟随 |
| 磁盘用量 | `crates/core/src/metrics_remote.rs` `disk_usage` | Unix 走 `df -Pk` 解析末行，失败再用 `Get-PSDrive` 兜底（Windows 无 df） |
| 目录并发 | `src/lib/pool.ts` `runPool` + `SftpPanel` 的 `TRANSFER_CONCURRENCY = 3` | 3 路并发池，失败即停止派发，等在途条目结束再收尾 |

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 先写临时文件、写完才 rename | 半截数据写在 `<目标>.rmxpart` 上，目标文件名下永远不会出现「看起来完整、实际残缺」的文件，用户不必靠大小猜 |
| 取消保留 `.rmxpart`，其它失败删除 | 只有用户主动中止才是「打算稍后续传」的信号；网络错误留下的残留用户没预期，留着只会在下次上传时变成难以解释的起点 |
| `resume` 由调用方显式传 bool | 后端不去猜残留是否可信；同一个 API 既能续传也能强制重传，语义由界面决定 |
| 残留比源文件还大就从头传 | 说明源文件在中断后变过，那半截数据与当前内容对不上，接着写只会得到一个混合文件 |
| 续传时远端要 `seek` 到断点 | SFTP 的写请求带显式偏移，仅仅不 `TRUNCATE` 不够，位置不移动会把续传段盖到文件头上 |
| 进度按 1 MiB / 200 ms 节流 | 256 KiB 一块逐块推会把 IPC 打满；加时间兜底后慢链路也能每 200 ms 有一次心跳，界面不会像卡死 |
| 取消用标志位轮询，不用 abort | 拷贝循环自己退出，句柄能正常 `close`，不会留下半开通道或未确认的写请求 |
| 分块固定 256 KiB | 与 russh-sftp 单包上限对齐，一块一个请求，不产生隐式拆包与额外往返 |
| 属性在所有数据落位后设置 | 先设权限再写内容会被后续写入请求打扰；且 `SETSTAT` 失败只打日志，不把已经成功落盘的传输判成失败 |
| 符号链接直接拒绝 | 跟随会把上传内容写进链接目标、把链接目标的内容当成本次下载对象，两头都错且没人察觉 |
| `percent` 由 `used/total` 自算 | `df` 的 Capacity 列是四舍五入到整数的百分比，进度条要的是连续值 |
| 目录并发固定 3 路 | 单文件传输已用满单请求带宽，再高只会让小文件在远端抢通知通道，收益不明显 |

### 踩坑：上传下载一律在 0 字节处失败

**现象。** 真机上任何上传/下载都在第一个字节写出之前失败，报「上传文件不能在异步上下文里调用，请放到阻塞线程里执行」，而命令层明明已经用 `spawn_blocking` 包了一层。

**根因。** `crates/core/src/sftp.rs` 的 `run_borrowed()` 曾用 `tokio::runtime::Handle::try_current().is_ok()` 判断「当前是否处于异步上下文」，命中就拒绝执行。但 tokio 的 `spawn_blocking` 工作线程在跑任务前会调用 `rt.enter()` 进入运行时上下文（见 tokio `runtime/blocking/pool.rs`），所以在这个守卫眼里阻塞线程和 async worker 毫无区别，判据在阻塞线程里**恒为真**——合法调用被全部拦下。又因为这条路径只在命令层走，纯单测不经过 `spawn_blocking`，缺陷在本地一直是绿的。

**修法。** 删掉该守卫与对应的错误分支，`run_borrowed()` 只保留一句 `runtime().block_on(future)`。真在 async worker 上误用会由 tokio 在 `block_on` 处直接 panic，属于开发期立刻能暴露的错误，比一条在生产路径上必然误伤的守卫可靠。

**回归测试。** 新增 `sftp::tests::upload_succeeds_inside_spawn_blocking`：在 `tokio::task::spawn_blocking` 内部上传 64 KiB 文件，断言传输成功、远端内容逐字节一致、且不残留 `.rmxpart`。它能复刻命令层的真实形态，在旧守卫下必然红，是这个缺陷的看门狗。

### 实测记录（2026-11-03，xiaojie `:10`）

| 场景 | 结果 |
| --- | --- |
| 上传 60 MiB | 最终 62,914,560 字节，md5 `16c35c6efa981919052fc9aeb31e221e` 与源文件一致，远端权限 664、mtime 与本机一致 |
| 限速 30mbit 后取消 | 在 18.3 MiB 处取消，远端 `.rmxpart` 为 19,398,656 字节，目标文件名下没有半截文件 |
| 续传 | `resume = true` 从 19,398,656 字节接着传，最终 62,914,560 字节，md5 仍为 `16c35c6efa981919052fc9aeb31e221e` |
| 阻塞线程回归 | `upload_succeeds_inside_spawn_blocking` 通过（内容一致、无 `.rmxpart` 残留） |

### 已知限制

- 没有 ETA / 剩余时间估算，进度只有「已传字节 + 百分比」。
- 目录递归传输的并发固定为 3，不可配置。
- 覆盖已存在文件的语义依赖服务端：远端 RENAME 能否覆盖旧文件由 SFTP 服务端实现决定，不同服务端行为可能不同（下载侧为绕开 Windows 的 rename 限制，改为先删本地目标再改名）。
- 跨文件系统 rename 可能失败；临时文件与目标同目录时不会发生，但服务端把两者映射到不同存储时仍可能拒绝。
- 取消与 rename 之间存在竞态窗口：取消标志在最后一块写完后才被置真时，传输可能已经完成改名，此时取消无效，也不会留下可续传的残留。
- 下载方向不回写远端文件的权限与 mtime，本地文件按本机 umask 与当前时间落盘。
- 续传只认残留长度，不比对内容：`.rmxpart` 若被外部改动过，续传不会察觉，结果是拼接文件。
- Windows 磁盘用量兜底不支持 UNC 路径：`Get-PSDrive` 按盘符前缀匹配，`\\server\share` 匹配不到，按不可用整行隐藏。

## 二十三、隧道持久化、目录跟随、系统字体与凭据回读

这一轮把四件「半成品」补成能用：

- 隧道只有运行句柄快照，进程一重启全丢，界面是「一个新建表单 + 运行中列表」，等于每次都得重新填一遍。
- 前端早就注册了 OSC 7 处理器、SFTP 也早就有 `followCwd`，但**没有任何代码让 shell 发 OSC 7**，所以目录跟随静默不生效。
- 终端字体只有四档硬编码预设，用户装了别的等宽字体也用不上。
- 密码/口令弹窗里「存入凭据库」勾选框其实已经写好，却被 `allowSaveToVault={vault.unlocked}` 硬隐藏——未解锁时用户根本看不到它；而且私钥口令存进去后没有任何代码读回，属于只存不用。

**验证状态**：字体与目录跟随有量化实测（见文末实测记录）；隧道的配置增删改查、启停、指纹弹窗与「编辑运行中配置不重启」的提示都在界面上手工走查过；凭据回读走的是「解锁 → 勾选入库 → 关掉重连不再问」这条路径。core 侧本轮新增的自动化用例是字体的 `system_fonts_are_non_empty_unique_and_sorted`、`empty_database_yields_empty_list`，以及 OSC 7 脚本的 `osc7_script_appends_bash_prompt_command_hook`、`osc7_script_uses_precmd_functions_for_zsh`、`osc7_script_skips_unsupported_shell`、`osc7_clear_sequence_grows_on_narrow_terminals`。隧道配置的存储层与命令层没有新增用例。

### 分层

四件事共用一个骨架：协议与凭据在 Rust 侧、前端只渲染与合并。

| 主题 | 层 | 位置 | 说明 |
| --- | --- | --- | --- |
| 隧道 | 表结构 | `crates/core/src/store.rs` 的 `DDL_TUNNEL_CONFIG` / `DDL_TUNNEL_INDEX` | `tunnel_config` 共 10 列（`id/name/session_id/kind/listen_host/listen_port/target_host/target_port/created_at/updated_at`），`session_id` 带 `REFERENCES session(id) ON DELETE CASCADE`，外加一条按 `session_id` 的索引；连接统一开了 `foreign_keys(true)`，级联是真生效的 |
| 隧道 | 模型 | `crates/core/src/model.rs` 的 `TunnelConfig` / `TunnelConfigInput` / `effective_name` | 入参字段全部 `#[serde(default)]`，前端少传一个转发字段不会整条请求反序列化失败；名称留空时由 core 生成 |
| 隧道 | 命名 | `model.rs` 的 `default_tunnel_name` | 本地/远程出「本地 8080 → db.internal:5432」，动态出「动态 SOCKS5 host:port」，端口 0 写作「自动」 |
| 隧道 | 存储 | `store.rs` 的 `tunnel_config_list/get/insert/update/delete` | 写入前先校验并确认会话存在；`update` 取 `max(now, 旧 updated_at + 1)` 保证时间戳严格递增（同毫秒连点也能看出顺序）；`delete` 只删记录 |
| 隧道 | 命令 | `src-tauri/src/commands/tunnel.rs` | 配置态 `tunnel_config_list/create/update/delete` 与运行态 `tunnel_start/stop/list` 分开；`tunnel_start` 已在跑就回现状，不叠第二条 SSH 连接 |
| 隧道 | 认证解析 | 同文件 `resolve_session_auth` | 配置只存 `session_id`，启动时按会话的 `auth_kind` 走 `resolve_auth` → `vault.reveal`；凭据库锁着给「凭据库未解锁」而不是混进连接失败 |
| 隧道 | 连接参数 | 同文件 `tunnel_start` 组装 `SshOptions` | host/port/username 取自会话，`accept_unknown_host_key = false`，`jump: None` |
| 隧道 | 运行态合并 | 同文件 `tunnel_list` + `info_of` | 监听端口取自句柄（自动分配时才是真端口），转发参数取自句柄内 spec；配置已被删但还在跑的项名字回退成 `(已删除配置)`，最后按名字排序，避免哈希表顺序让列表跳动 |
| 隧道 | 列表界面 | `src/components/TunnelDialog.tsx` | 状态圆点 / 名称 / 类型 / 监听 / 目标 / 行内启停+编辑+删除；工具栏是新建 / 全部启动 / 全部停止 / 刷新 |
| 隧道 | 配置弹窗 | `src/components/TunnelFormDialog.tsx` | 服务器下拉只列 SSH 会话，会话缺失时给「（会话已删除）」；编辑运行中的配置顶部提示「修改将在下次启动后生效」 |
| 隧道 | 指纹交互 | `TunnelDialog.tsx` 的 `startWithHostKey` + `App.tsx` 的 `HostKeyDialog` | 只有 `kind = "hostKeyUnknown"` 才升级成弹窗（与终端共用同一个组件）；信任后重试一次，取消回「已取消，未信任主机指纹」，重试仍失败只回后端原文、不二次弹窗 |
| 跟随 | 远端探测 | `crates/core/src/ssh.rs` 的 `SHELL_PROBE_COMMAND` / `detect_remote_shell` | 单独 exec 通道跑 `printf '%s\n' "$SHELL" "$0"`，1.5 秒超时、输出上限 256 字节；失败按「不支持」处理，探测本身不能成为连接失败的理由 |
| 跟随 | 远端注入 | 同文件 `osc7_setup_script` / `install_cwd_hook` | bash 追加 `PROMPT_COMMAND`，zsh 用 `precmd_functions=(__rmx_osc7 ${precmd_functions:#__rmx_osc7})` 去重追加；`Unsupported` 返回 `None`，一个字节都不发；脚本当成一行普通输入写进 PTY，不 `AcceptEnv`、不改远端文件 |
| 跟随 | 回显擦除 | 同文件 `osc7_clear_sequence` | 按脚本长度与列数估行数（夹到 3~8 行），`printf '\r\033[2K…'` 擦掉自身回显，再回车 |
| 跟随 | 本地 cwd | `crates/core/src/terminal.rs` 的 `TerminalSession::cwd` → `read_process_cwd` | pid 在包进 `Arc` 之前就记下；Linux 读 `/proc/<pid>/cwd` 并去掉目录被删后的 `" (deleted)"` 后缀，非 Linux 一律 `None` |
| 跟随 | 前端解析 | `src/components/TerminalPanel.tsx` 的 `registerOscHandler(7)` | 沿用旧链路，OSC 7 解析成目录后经 `onCwdChange` 上报 |
| 跟随 | 轮询 | 同文件 `pollLocalCwd` + `setInterval(…, 1200)` | 只有 local 会话轮询；SSH 会话不轮询，等远端自己报 |
| 跟随 | 分屏仲裁 | 同文件模块级 `activePaneToken` | 鼠标按下 / `focusin` / xterm textarea focus / 重新可见（`IntersectionObserver`）都会重新认领；非活动分屏不上报 |
| 跟随 | 消费端 | `src/components/SftpPanel.tsx` 的 `followCwd` | 设置项 `sftp.followCwd`，目录一变就 `loadDir` |
| 字体 | 枚举 | `crates/core/src/fonts.rs` `system_fonts()` → `collect_families` | fontdb 0.24（纯 Rust，不吃系统 libfontconfig）扫系统字体目录；同族多字重合并成一条、任一等宽即等宽；`BTreeMap` 的键序天然给出稳定升序 |
| 字体 | 命令 | `src-tauri/src/commands/fonts.rs` 的 `system_fonts` | `OnceLock` 缓存首次扫描结果，首次走 `spawn_blocking`；扫描失败或空表按「没读到字体」正常返回，不让设置页报错 |
| 字体 | 设置模型 | `src/api/settings.ts` 的 `terminalFontFamily: string` | 存族名（或迁移后的完整字体栈），不再存预设 id |
| 字体 | 旧值迁移 | 同文件 `parseFontFamily` + `FONT_STACKS` | 旧 id（`default/menlo/consolas/mono`）查表还原成字体栈，其余值原样保留、不做白名单——枚举不到系统字体时还得能手输 |
| 字体 | 组装 | 同文件 `resolveFontStack` | 空值回落默认 → 旧 id 查表 → 含逗号视为完整栈透传 → 其余当族名加引号并补 `ui-monospace, monospace` |
| 字体 | 选择控件 | `src/components/SettingsDialog.tsx` | 输入框过滤 + 可滚动候选列表，等宽字体带标记并排在前面，允许手输任意族名；枚举失败与「本机没字体」分开提示 |
| 凭据 | 弹窗 | `src/components/SecretPromptDialog.tsx` 的 `vaultReady` | 「存入凭据库」始终渲染；`allowSaveToVault` 语义改为「能不能入库」，不可用时置灰，并按 `vaultInitialized` 给出「设置凭据库」或「解锁」 |
| 凭据 | 解锁往返 | `src/App.tsx` 的 `pendingVaultOpen` | 点解锁时本弹窗不卸载，凭据库弹窗关闭后回到原弹窗，已输入的密码与凭据名都不丢 |
| 凭据 | 写入 | `App.tsx` 的 `persistSessionExtra` | 私钥口令的凭据引用写进会话 `extra` 的 `passphraseCredentialId` |
| 凭据 | 回读 | `src-tauri/src/commands/vault.rs` 的 `credential_reveal` + `src/api/vault.ts` | 复用 core 既有解密路径，要求凭据库已解锁；明文只交本次认证，不写日志、不落库 |
| 凭据 | 取用 | `App.tsx` 的 `revealStoredPassphrase` | 没有引用或库还锁着就直接返回 `null`，退回手输口令，不打搅用户 |

### 关键决策

| 决策 | 原因 |
| --- | --- |
| 隧道配置只存 `session_id`，不复制主机三要素 | host/port/username 与认证引用已经在会话里，再存一份必然出现「改了会话、隧道还用旧值」的双份真相；启动时按会话解析还顺带复用了 `resolve_auth` 的解锁与报错口径 |
| 配置态与运行态两套命令 | 配置能跨重启，句柄不能。`tunnel_config_list` 只答「用户定义了什么」，`tunnel_list` 只答「现在跑着什么」，合并（按 id）交给前端 |
| 转发参数从句柄回读 | `listen_port` 传 0 时真实端口由系统或远端分配，只有句柄知道；配置里的 0 不能当答案 |
| 名称留空由 core 生成 | 前端不传 `name` 也能用，列表里不会出现一列空名字；生成规则写在 `model.rs`，命令层与界面不各写一份 |
| 后端删除配置不动运行中的隧道，界面先停再删 | 后端 `tunnel_config_delete` 只删记录，避免底层调用顺手断业务；界面 `removeOne` 在二次确认之后主动先 `tunnelStop` 再 `tunnelConfigDelete`，因为用户点「删除」的预期是「这条隧道没了」，而不是「留一条再也编辑不了的连接」 |
| 指纹走前端弹窗，而不是后端「先信任再连」 | 隧道在后台启动，没有地方弹确定框；让它以 `hostKeyUnknown` 失败、前端复用终端那套 `HostKeyDialog`，信任后重试一次，语义与终端完全一致，用户不必再学一套 |
| shell 类型先探测再注入 | 注入靠 shell 自己解析，猜错等于往用户交互式输入里塞垃圾；探测失败或非 bash/zsh 时宁可不发 |
| 注入脚本自带擦行 | 脚本本身会回显在屏幕上，不擦掉的话一行 `PROMPT_COMMAND=…` 会永久留在用户终端顶部 |
| 擦除行数宁少勿多 | 估小只会留一截命令回显，估大却会把屏幕原有输出删掉 |
| 本地跟随靠轮询，远端靠 OSC 7 | 本地 shell 是用户自己的（bash/zsh/fish 都有），我们没有注入点；pid 的 cwd 是唯一不打扰用户的读法。远端有 SSH 通道可以注入，零轮询更省 |
| 分屏只让活动面板上报 | 两个分屏挂同一条会话时「后建连的赢」，目录会自己来回跳；用令牌把规则改成「最后被操作的那块赢」，切标签、点一下、聚焦都能夺回上报权 |
| 字体走 fontdb 而不是 libfontconfig | 纯 Rust，不引系统库依赖，交叉编译和容器镜像都不必装 fontconfig |
| 字体值存族名而不是预设 id | 四档之外用户还有几十种等宽可选；存族名后新增字体不用改代码，也不用再迁移一次 |
| 旧值迁移成字体栈而不是单族名 | 旧档位本身是一条栈（Menlo 之后还有 Monaco、Courier New 兜底），压成单族名会让原本能落到兜底的机器掉到无衬线回退 |
| 手输族名一律加引号 | 族名常带空格（`JetBrains Mono`），不加引号 CSS 会按空格切成多个非法标识符，整条声明失效 |
| 枚举失败不报错，退化成手输 | 容器里没装字体是常态，不是用户错误；空表让界面给出「可直接输入字体名」的提示即可 |
| 勾选框常显、入口随状态变 | 功能看不见等于不存在；把「不可用」表达成置灰 + 解锁入口，用户才知道有这件事、也知道怎么打开 |
| `credential_reveal` 只服务内存中的一次认证 | 明文过 IPC 后立即用于握手，不落库、不写日志；凭据库锁定即拒绝，密钥不会因为「顺手回读」而多一条泄漏路径 |

### 踩坑：阻塞线程也被当成异步上下文

**现象。** 本轮新增的 `system_fonts` 与 `tunnel_start` 同样把重活丢进 `spawn_blocking`，而第二十二章记过的同一类缺陷正好在这条路径上复现：任何「判断自己是否处于异步上下文」的守卫都会误伤阻塞线程。

**根因。** tokio 的 `spawn_blocking` 工作线程在跑任务前会调用 `rt.enter()` 进入运行时上下文，所以 `Handle::try_current().is_ok()` 在阻塞线程里**恒为真**。core 的 `run_borrowed()` 曾用它判断异步上下文并拒绝执行，结果所有上传下载都在 0 字节处失败（详见第二十二章）。

**修法。** 删掉该守卫，`run_borrowed()` 只保留 `runtime().block_on(future)`。真在 async worker 上误用会由 tokio 在 `block_on` 处直接 panic，属于开发期立刻能暴露的错误，比一条在生产路径上必然误伤的守卫可靠。本轮新增的两条命令都照此处理：只 `spawn_blocking` 包一层，不再自己判断上下文。

### 踩坑：SFTP v3 的 RENAME 不保证覆盖，重新上传同名文件永远失败

**现象。** 往已存在的远端文件上传，每次都在收尾报「改名为目标失败」，但目标文件其实完好，`.rmxpart` 也留着——看起来像权限问题，其实是协议语义问题。

**根因。** SFTP v3 的 RENAME 只要求「新名字可用」，不保证覆盖已存在的目标；OpenSSH 的 `sftp-server` 在目标已存在时直接回 `SSH_FX_FAILURE`。而 russh-sftp 3.0.1 的 `SftpSession` 没有暴露 `posix-rename@openssh.com`（那个扩展才有覆盖语义），所以只能自己兜底。

**修法。** `rename_part_into_place()`：第一次 RENAME 失败后先看目标是否存在——不存在说明失败另有原因（权限、跨设备、父目录没了），原样报回，别用删除掩盖真实错误；存在则先确认 `.rmxpart` 大小等于期望的总大小（临时文件不完整就绝不删旧文件，宁可让用户续传补齐），再删目标、重试一次改名。任一步失败都保留 `.rmxpart`，续传语义不受影响。代价是「删除 + 改名」之间有一个非原子窗口，这期间目标名短暂不存在。

**回归测试。** `overwrites_existing_remote_file_on_upload`：测试服务端还原 OpenSSH 的 RENAME 语义（目标已存在就回 `SSH_FX_FAILURE`），先传内容 A，再在 `spawn_blocking` 里把内容 B 传到同一路径（复刻生产路径），断言第二次成功、远端内容为 B、无 `.rmxpart` 残留，并且服务端确实拒绝过一次 RENAME——证明走的是兜底路径，而不是恰好没触发覆盖。

### 踩坑：`table-layout: fixed` 下 `width: auto` 的列被 WebKitGTK 压成 0

**现象。** 隧道列表的名称列只显示一个字符，其它列都正常。

**根因。** 表格是 `table-layout: fixed`，名称列写的是 `width: auto`。这种写法靠「表格宽度减去定宽列」来分配剩余空间，而 Tauri 窗口用的 WebKit 在容器偏窄、表格又被 `min-width` 撑开时，会把这份剩余算成 0 —— 名称列最后只剩内边距，被压成一两个字。

**修法。** 名称列改成 `width: calc(100% - 680px)`（定宽列合计 680px：状态 92 + 类型 60 + 监听 172 + 目标 164 + 操作 192），每个定宽列都按本列最长文案（含「启动中…」这种过渡态）给足；超长名字用 `text-overflow: ellipsis` 截断，全名挂在单元格内 `span` 的 `title` 上。`calc` 让名称列在任何引擎里都等于「表格宽度 - 定宽之和」，不再依赖引擎对剩余宽度的解释。

### 实测记录（2026-11-03）

| 场景 | 结果 |
| --- | --- |
| 字体枚举 | 本机 `fc-list` 387 面 / 去重 182 族 / 等宽 12；设置页候选约 182 条 |
| 字体实时生效 | 选 `Mitra` 后终端 Welcome 行像素宽 784px → 659px |
| 字体持久化 | 重启应用后仍是 `Mitra` |
| 远端目录跟随 | 远端 bash 下 `cd /home/rmxtest/web/sub`、`cd /tmp`，SFTP 面板 2~3 秒内跟随到位 |
| 默认栈观感 | `Liberation Mono` 与「系统等宽（默认）」渲染逐像素一致——默认栈里的 `"Courier New"` 被 fontconfig 映射到 Liberation Mono，不是缺陷 |

### 已知限制

- **隧道没有自动重连与退避。** `crates/core/src/tunnel.rs` 的健康检查每 5 秒看一次 `connection.is_closed()`，命中就 `break` 退出监听循环并释放本地端口/远端转发，不会重连。这与 `docs/architecture.md` 里「supervisor 持有 cancel token 与指数退避状态」的描述不符，**以本文为准**：断线后隧道就是停了，要用户手动再启动。
- **隧道配置强耦合会话。** `session_id` 是外键且 `ON DELETE CASCADE`，删会话会连带删掉它的隧道配置；正在跑的隧道不受影响，但会退化成「(已删除配置)」的孤儿行，且再也无法编辑。
- **SSH Agent 认证的会话无法启动隧道。** core 的 `SshAuth` 只有 `Password` / `PrivateKey` 两种，`resolve_session_auth` 对 `AuthKind::Agent` 直接报「隧道暂不支持」。
- **跳板机固定为空。** 会话里没有跳板配置入口，`tunnel_start` 固定 `jump: None`，隧道永远直连，走跳板机的目标连不上。
- **编辑运行中的配置不会自动重启。** 表单里只有一行提示，改完要自己停止再启动才生效。
- **`hostKeyMismatch`（主机密钥变更）在隧道场景没有专用处理。** 终端会弹阻断弹窗并说明原因，隧道只把后端原文显示在提示条里。
- **目录跟随在非 bash/zsh 远端不生效。** 探测不到或不在支持列表里就不注入，SFTP 面板停在原目录。
- **`.bashrc` 重设 `PROMPT_COMMAND`、或 p10k / oh-my-zsh 重写 `precmd`，会冲掉钩子**，跟随随即失效（注入发生在 shell 启动后，晚于这些配置的执行时机只能靠用户自己保留钩子）。
- **bash 5.1 的数组形式 `PROMPT_COMMAND` 会被改写成字符串**，可能丢掉用户原有的其它钩子。
- **注入回显的擦除是估算的。** shell 还在跑重 `.bashrc` 时，屏幕顶部可能残留约 3 行注入命令的回显。
- **本地目录跟随仅 Linux。** 非 Linux 平台 `read_process_cwd` 一律返回 `None`，目录不跟随，也不报错。
- **凭据口令复用依赖会话标记。** 只有会话 `extra.keyHasPassphrase === true` 时才会去按 `passphraseCredentialId` 取明文；未勾选该标记的会话仍走「无口令私钥」分支，口令存了也不会被用上。
