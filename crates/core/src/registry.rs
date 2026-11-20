//! 终端注册表：按会话标识存放运行中的终端句柄。
//!
//! 桌面壳与 MCP 宿主共用这一份实现，注册表本身只做增删查，
//! 不关心底层是本地 PTY、SSH、Telnet 还是串口。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::terminal::TerminalControl;

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
