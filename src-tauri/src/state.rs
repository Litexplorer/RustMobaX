use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustmobax_core::{MetricsSampler, SftpClient, Store, TerminalControl, TunnelHandle, Vault};

/// 本地终端注册表。
///
/// 无参数
/// 核心流程：按会话标识存放运行中的 PTY，命令层通过它读写与回收。
#[derive(Default)]
pub struct TerminalRegistry {
    sessions: Mutex<HashMap<String, Arc<dyn TerminalControl>>>,
}

impl TerminalRegistry {
    /// 登记一个新终端。
    ///
    /// 参数：id 为会话标识，session 为终端句柄
    /// 核心流程：加锁写入，重复 id 覆盖旧值。
    pub fn insert(&self, id: String, session: Arc<dyn TerminalControl>) -> bool {
        match self.sessions.lock() {
            Ok(mut map) => {
                map.insert(id, session);
                true
            }
            Err(_) => false,
        }
    }

    /// 取一个终端句柄。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：命中即克隆 Arc 返回，调用方无需持锁。
    pub fn get(&self, id: &str) -> Option<Arc<dyn TerminalControl>> {
        self.sessions.lock().ok()?.get(id).cloned()
    }

    /// 移除并返回终端句柄。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：先摘下再交给调用方去 kill，避免锁内做阻塞操作。
    pub fn remove(&self, id: &str) -> Option<Arc<dyn TerminalControl>> {
        self.sessions.lock().ok()?.remove(id)
    }
}

/// SFTP 会话注册表。
///
/// 无参数
/// 核心流程：按前端拿到的标识存放 SFTP 会话（每条会话独占一条 SSH 连接），
/// 与终端注册表分离，关闭文件面板不会误伤正在跑命令的 shell。
#[derive(Default)]
pub struct SftpRegistry {
    clients: Mutex<HashMap<String, Arc<SftpClient>>>,
}

impl SftpRegistry {
    /// 登记一个 SFTP 会话。
    ///
    /// 参数：id 为会话标识，client 为 SFTP 客户端
    /// 核心流程：加锁写入，重复 id 覆盖旧值。
    pub fn insert(&self, id: String, client: Arc<SftpClient>) -> bool {
        match self.clients.lock() {
            Ok(mut map) => {
                map.insert(id, client);
                true
            }
            Err(_) => false,
        }
    }

    /// 取一个 SFTP 客户端。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：命中即克隆 Arc 返回，调用方无需持锁。
    pub fn get(&self, id: &str) -> Option<Arc<SftpClient>> {
        self.clients.lock().ok()?.get(id).cloned()
    }

    /// 移除并返回 SFTP 客户端。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：先摘下再交给调用方去关连接，避免锁内做阻塞操作。
    pub fn remove(&self, id: &str) -> Option<Arc<SftpClient>> {
        self.clients.lock().ok()?.remove(id)
    }
}

/// 传输取消标志注册表。
///
/// 无参数
/// 核心流程：按前端生成的 cancelToken 存放取消标志，
/// 命令开始时登记、结束时移除；取消命令只需要把对应标志置真。
/// 与 SFTP 注册表分离，取消一次传输不会碰到别的东西。
#[derive(Default, Clone)]
pub struct Transfers {
    flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl Transfers {
    /// 登记一次传输并拿到它的取消标志。
    ///
    /// 参数：token 为前端生成的取消令牌
    /// 核心流程：造一个新标志写进表里；同名字令重复登记时覆盖旧值，
    /// 旧传输若还在跑，它的标志已经没人能碰到，也就不会被误取消。
    /// 锁不可用时返回一个无人共享的标志，传输照常进行（取消会失效，但不阻断）。
    pub fn register(&self, token: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut flags) = self.flags.lock() {
            flags.insert(token.to_string(), Arc::clone(&flag));
        }
        flag
    }

    /// 置真一次传输的取消标志。
    ///
    /// 参数：token 为取消令牌
    /// 核心流程：命中就置真；未命中（已经结束或压根没开始）什么都不做，
    /// 所以前端重复取消、或取消一个已完成的传输都不算错误。
    pub fn cancel(&self, token: &str) {
        if let Ok(flags) = self.flags.lock() {
            if let Some(flag) = flags.get(token) {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }

    /// 传输结束后移除取消标志。
    ///
    /// 参数：token 为取消令牌
    /// 核心流程：成功、失败、取消都走这里，避免注册表随传输次数无限增长。
    pub fn finish(&self, token: &str) {
        if let Ok(mut flags) = self.flags.lock() {
            flags.remove(token);
        }
    }
}

/// 隧道注册表。
///
/// 无参数
/// 核心流程：按隧道配置的标识存放运行中的隧道（每条隧道独占一条 SSH 连接），
/// 与终端、SFTP 注册表分离，停隧道不会误伤正在跑的 shell 或文件面板。
#[derive(Default)]
pub struct TunnelRegistry {
    tunnels: Mutex<HashMap<String, Arc<TunnelHandle>>>,
}

impl TunnelRegistry {
    /// 登记一条隧道。
    ///
    /// 参数：id 为隧道配置的标识，handle 为隧道句柄
    /// 核心流程：加锁写入，重复 id 覆盖旧值。
    pub fn insert(&self, id: String, handle: Arc<TunnelHandle>) -> bool {
        match self.tunnels.lock() {
            Ok(mut map) => {
                map.insert(id, handle);
                true
            }
            Err(_) => false,
        }
    }

    /// 判断某条配置是否正在运行。
    ///
    /// 参数：key 为隧道配置的标识
    /// 核心流程：只查键在不在，用来避免同一条配置被启动两次；
    /// 锁中毒（不应发生）按“不在运行”处理，后续启动失败会给出明确错误。
    pub fn contains(&self, key: &str) -> bool {
        match self.tunnels.lock() {
            Ok(map) => map.contains_key(key),
            Err(_) => false,
        }
    }

    /// 取一条运行中的隧道句柄。
    ///
    /// 参数：key 为隧道配置的标识
    /// 核心流程：命中即克隆 Arc 返回，调用方无需持锁。
    pub fn get(&self, key: &str) -> Option<Arc<TunnelHandle>> {
        self.tunnels.lock().ok()?.get(key).cloned()
    }

    /// 移除并返回隧道句柄。
    ///
    /// 参数：id 为隧道配置的标识
    /// 核心流程：先摘下再交给调用方去停，避免锁内做阻塞操作。
    pub fn remove(&self, id: &str) -> Option<Arc<TunnelHandle>> {
        self.tunnels.lock().ok()?.remove(id)
    }

    /// 导出全部隧道。
    ///
    /// 参数：无
    /// 核心流程：克隆一份 (id, 句柄) 列表后立即释放锁，
    /// 后续读取运行状态不需要再持锁。
    pub fn list(&self) -> Vec<(String, Arc<TunnelHandle>)> {
        match self.tunnels.lock() {
            Ok(map) => map
                .iter()
                .map(|(id, handle)| (id.clone(), Arc::clone(handle)))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// 应用级共享状态。
///
/// 无参数
/// 核心流程：持有会话库、凭据库、终端注册表、SFTP 注册表、隧道注册表与传输取消表，
/// 所有命令通过它访问运行时资源。
pub struct AppState {
    pub store: Store,
    pub vault: Vault,
    pub terminals: TerminalRegistry,
    pub sftp: SftpRegistry,
    pub tunnels: TunnelRegistry,
    /// 正在跑的传输的取消标志，按前端的 cancelToken 索引
    pub transfers: Transfers,
    /// 本机指标采样器，保留上次基线用于算差值
    pub metrics: Mutex<MetricsSampler>,
}
