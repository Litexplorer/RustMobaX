//! MCP 服务的启停控制器：不依赖 Tauri，可被单测直接覆盖。
//!
//! 只做三件事：按配置起停服务、改端口时先停旧的再起新的、把最后一次失败原因留在
//! 状态里。端口与令牌都由调用方给，这一层不读设置表也不生成令牌。

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
};

use tokio::task::JoinHandle;

use crate::{Host, McpAudit, McpServerConfig, transport::serve_on};

/// 正在运行的服务。
///
/// 无参数
/// 核心流程：记下实际监听地址与后台任务句柄，停服务时先 abort 再等它结束，
/// 这样端口一定已经释放，紧接着重新绑定不会撞上 TIME_WAIT 或旧 socket。
struct Running {
    addr: SocketAddr,
    handle: JoinHandle<()>,
}

/// 控制器内部的槽位。
///
/// 无参数
/// 核心流程：运行中的服务与最近一次失败原因放在一起，读取状态时一把锁就能取全。
#[derive(Default)]
struct Slot {
    running: Option<Running>,
    last_error: Option<String>,
}

/// MCP 服务的启停状态快照。
///
/// 无参数
/// 核心流程：对外只暴露结论，不暴露句柄；`bind_addr` 是内核分配之后的真实地址，
/// 端口填 0 时它才有意义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStatus {
    /// 当前是否在监听。
    pub running: bool,
    /// 实际监听地址，未运行时为空。
    pub bind_addr: Option<SocketAddr>,
    /// 最近一次启动失败的原因，成功后清空。
    pub last_error: Option<String>,
}

/// MCP 服务控制器。
///
/// 无参数
/// 核心流程：内部只有一个槽位，所有操作先摘旧句柄再起新句柄，同一时刻最多一个监听。
/// 锁只保护摘取与写入，等待服务启动的过程不持锁，避免把状态查询也堵住。
pub struct McpControl {
    slot: Mutex<Slot>,
}

impl Default for McpControl {
    /// 构造空控制器。
    ///
    /// 参数：无
    /// 核心流程：等价于 [`McpControl::new`]，让管理状态可以直接用 `Default` 表达。
    fn default() -> Self {
        Self::new()
    }
}

impl McpControl {
    /// 构造一个空控制器。
    ///
    /// 参数：无
    /// 核心流程：槽位为空，不绑定端口也不起任务。
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(Slot::default()),
        }
    }

    /// 幂等应用配置：先停旧服务，再按开关决定是否起新的。
    ///
    /// 参数：host 为宿主能力；audit 为工具调用审计；enabled 为是否监听；
    /// port 为监听端口，填 0 表示让内核分配；token 为客户端必须携带的 Bearer 令牌
    /// 核心流程：摘下旧句柄并等它真正结束，确保端口已释放；关闭时到此为止；开启时按回环
    /// 地址重新绑定，成功就把新句柄放回槽位，失败只把原因写进状态而不抛错，调用方通过
    /// `last_error` 拿到中文原因。
    pub async fn apply(
        &self,
        host: Arc<dyn Host>,
        audit: Arc<dyn McpAudit>,
        enabled: bool,
        port: u16,
        token: &str,
    ) -> McpStatus {
        let previous = {
            let mut slot = self.slot();
            slot.last_error = None;
            slot.running.take()
        };
        if let Some(running) = previous {
            shutdown(running).await;
        }

        if !enabled {
            return self.status();
        }

        let config = McpServerConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], port)),
            token: token.to_string(),
        };
        let started = serve_on(host, audit, config).await;

        let mut slot = self.slot();
        match started {
            Ok((addr, handle)) => slot.running = Some(Running { addr, handle }),
            Err(error) => slot.last_error = Some(format!("MCP 服务启动失败：{error}")),
        }
        snapshot(&slot)
    }

    /// 只停不启。
    ///
    /// 参数：无
    /// 核心流程：摘下句柄并等它结束，随后把失败原因一并清空，返回最新状态。
    /// 本来就没运行时是一次空操作。
    pub async fn stop(&self) -> McpStatus {
        let previous = {
            let mut slot = self.slot();
            slot.last_error = None;
            slot.running.take()
        };
        if let Some(running) = previous {
            shutdown(running).await;
        }
        self.status()
    }

    /// 读取当前状态。
    ///
    /// 参数：无
    /// 核心流程：只读槽位做快照，不等待任何服务操作，供命令层随时调用。
    pub fn status(&self) -> McpStatus {
        snapshot(&self.slot())
    }

    /// 取槽位锁。
    ///
    /// 参数：无
    /// 核心流程：锁中毒说明别的线程在持锁时 panic，这里按约定直接 panic 并在文案里
    /// 写明原因，不做静默降级。
    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().expect("MCP 控制器锁被污染")
    }
}

/// 停掉一个运行中的服务并等它真正结束。
///
/// 参数：running 为要停的服务
/// 核心流程：先 abort 再 await 句柄；await 保证任务已被丢弃，listener 随之关闭，
/// 这样调用方立刻重新绑定同一端口不会失败。
async fn shutdown(running: Running) {
    running.handle.abort();
    let _ = running.handle.await;
}

/// 把槽位转成对外状态。
///
/// 参数：slot 为已加锁的槽位
/// 核心流程：只搬运结论，句柄不外泄。
fn snapshot(slot: &Slot) -> McpStatus {
    McpStatus {
        running: slot.running.is_some(),
        bind_addr: slot.running.as_ref().map(|running| running.addr),
        last_error: slot.last_error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use rustmobax_core::Store;

    use super::*;
    use crate::{NoopAudit, StoreHost};

    /// 构造一个架在内存库上的宿主。
    ///
    /// 参数：无
    /// 核心流程：开内存库并包成宿主，测试不需要预置数据。
    async fn host() -> Arc<dyn Host> {
        let store = Store::open_in_memory().await.expect("内存库应能打开");
        Arc::new(StoreHost::new(store))
    }

    /// 造一个不落盘的审计实现。
    ///
    /// 参数：无
    /// 核心流程：只需要满足控制器参数，测试不检查审计内容。
    fn audit() -> Arc<dyn McpAudit> {
        Arc::new(NoopAudit)
    }

    #[tokio::test]
    async fn apply_enabled_starts_service() {
        let control = McpControl::new();

        let status = control.apply(host().await, audit(), true, 0, "token").await;

        assert!(status.running, "开关打开后应在监听");
        let addr = status.bind_addr.expect("运行时应带回实际监听地址");
        assert!(addr.ip().is_loopback(), "只应监听回环地址，实际为：{addr}");
        assert!(status.last_error.is_none(), "成功启动不应带错误");
        control.stop().await;
    }

    #[tokio::test]
    async fn apply_disabled_releases_port() {
        let control = McpControl::new();
        let started = control.apply(host().await, audit(), true, 0, "token").await;
        let addr = started.bind_addr.expect("运行时应带回实际监听地址");

        let stopped = control.apply(host().await, audit(), false, 0, "token").await;

        assert!(!stopped.running, "关闭后不应再监听");
        assert!(stopped.bind_addr.is_none(), "关闭后不应再报地址");
        let rebound = TcpListener::bind(addr).expect("停服务后端口应能被立刻重新绑定");
        drop(rebound);
    }

    #[tokio::test]
    async fn second_instance_on_same_port_reports_error() {
        let first = McpControl::new();
        let started = first.apply(host().await, audit(), true, 0, "token").await;
        let addr = started.bind_addr.expect("运行时应带回实际监听地址");

        let second = McpControl::new();
        let failed = second.apply(host().await, audit(), true, addr.port(), "token").await;

        assert!(!failed.running, "端口已被占用时不应报告运行中");
        let reason = failed.last_error.expect("端口占用应留下错误原因");
        assert!(reason.contains("启动失败"), "错误原因应说明启动失败：{reason}");
        first.stop().await;
    }

    #[tokio::test]
    async fn apply_with_new_port_switches_binding() {
        let control = McpControl::new();
        let first = control.apply(host().await, audit(), true, 47821, "token").await;
        assert_eq!(
            first.bind_addr.map(|addr| addr.port()),
            Some(47821),
            "第一次应绑在 47821"
        );

        let second = control.apply(host().await, audit(), true, 47822, "token").await;

        assert_eq!(
            second.bind_addr.map(|addr| addr.port()),
            Some(47822),
            "改端口后应绑在 47822，实际为：{:?}",
            second.bind_addr
        );
        control.stop().await;
    }
}
