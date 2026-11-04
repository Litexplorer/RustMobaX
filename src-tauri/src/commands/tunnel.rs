use std::sync::Arc;

use rustmobax_core::{
    AuthKind, CoreError, ErrorPayload, Session, SshAuth, SshOptions, TunnelConfig,
    TunnelConfigInput, TunnelHandle, TunnelKind, TunnelSpec, Vault,
};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::terminal::{resolve_auth, SshAuthRequest};
use crate::state::AppState;

/// 配置已被删除但隧道仍在运行时使用的名字。
const DELETED_CONFIG_NAME: &str = "(已删除配置)";

/// 返回给前端的隧道运行态信息。
///
/// 无参数
/// 核心流程：id 就是隧道配置的标识，所以列表与配置能一一对应；
/// listen_port 取自运行中的句柄（配置写 0 时这里是系统真实分配的端口）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelInfo {
    /// 等于配置 id。
    pub id: String,
    pub name: String,
    /// local / remote / dynamic。
    pub kind: String,
    pub listen_host: String,
    /// 实际监听端口（配置写 0 时是系统或远端分配的真实端口）。
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub running: bool,
}

/// 会话 extra 里与认证相关的引用。
///
/// 无参数
/// 核心流程：字段与前端 SessionDialog 写入的键一一对应（camelCase）；
/// 这里只做读取，明文密码与口令都不在这一层。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionExtra {
    #[serde(default)]
    private_key_path: Option<String>,
    #[serde(default)]
    key_has_passphrase: bool,
    #[serde(default)]
    credential_id: Option<String>,
    #[serde(default)]
    passphrase_credential_id: Option<String>,
}

/// 解析会话的高级配置。
///
/// 参数：raw 为会话 extra 的原始字符串
/// 核心流程：解析失败或不是对象时按空配置处理，脏数据不该挡住启动隧道。
fn parse_extra(raw: Option<&str>) -> SessionExtra {
    raw.and_then(|value| serde_json::from_str::<SessionExtra>(value).ok())
        .unwrap_or_default()
}

/// 把会话里保存的认证方式解成真正的凭据。
///
/// 参数：vault 为凭据库，session 为隧道所属会话
/// 核心流程：与终端一致——密码与私钥口令都存在凭据库里，只有这里（真正要连
/// 远端时）才 reveal 成明文；凭据库锁着时直接给出明确错误而不是静默失败；
/// SSH Agent 认证核心层还没实现，明确报不支持而不是退化成空密码。
async fn resolve_session_auth(vault: &Vault, session: &Session) -> Result<SshAuth, ErrorPayload> {
    let extra = parse_extra(session.extra.as_deref());
    match session.auth_kind {
        AuthKind::Password => {
            let credential_id = extra
                .credential_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ErrorPayload::from(CoreError::Validation(
                        "会话没有保存密码凭据，无法启动隧道；请先在会话里保存密码".into(),
                    ))
                })?;
            if !vault.is_unlocked() {
                return Err(vault_locked());
            }
            resolve_auth(
                vault,
                &SshAuthRequest::Credential {
                    id: credential_id.to_string(),
                },
            )
            .await
        }
        AuthKind::Key => {
            let path = extra
                .private_key_path
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ErrorPayload::from(CoreError::Validation(
                        "会话没有保存私钥路径，无法启动隧道".into(),
                    ))
                })?
                .to_string();
            let passphrase = if extra.key_has_passphrase {
                let credential_id = extra
                    .passphrase_credential_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ErrorPayload::from(CoreError::Validation(
                            "会话的私钥带口令但没有保存口令凭据，无法启动隧道".into(),
                        ))
                    })?;
                if !vault.is_unlocked() {
                    return Err(vault_locked());
                }
                Some(
                    vault
                        .reveal(credential_id)
                        .await
                        .map_err(ErrorPayload::from)?,
                )
            } else {
                None
            };
            Ok(SshAuth::PrivateKey { path, passphrase })
        }
        AuthKind::Agent => Err(ErrorPayload::from(CoreError::Validation(
            "该会话使用 SSH Agent 认证，隧道暂不支持；请改用密码或私钥认证".into(),
        ))),
    }
}

/// 凭据库未解锁时的统一错误。
///
/// 参数：无
/// 核心流程：隧道在后台启动，前端无法在此时弹输入框，所以必须明确告诉
/// 用户先解锁，而不是把"已锁定"混进连接失败里。
fn vault_locked() -> ErrorPayload {
    ErrorPayload::from(CoreError::Vault("凭据库未解锁，无法启动隧道".into()))
}

/// 把配置与句柄拼成前端结构。
///
/// 参数：id 为配置标识，name 为展示名，handle 为运行中的隧道句柄
/// 核心流程：转发方向、监听点与目标取自句柄内的 spec（与真正建连的参数一致），
/// 监听端口取自句柄（自动分配时才是真实端口），运行状态实时问句柄。
fn info_of(id: &str, name: &str, handle: &TunnelHandle) -> TunnelInfo {
    let spec = handle.spec();
    TunnelInfo {
        id: id.to_string(),
        name: name.to_string(),
        kind: spec.kind.as_str().to_string(),
        listen_host: spec.listen_host.clone(),
        listen_port: handle.listen_port(),
        target_host: spec.target_host.clone(),
        target_port: spec.target_port,
        running: handle.is_running(),
    }
}

/// 列出全部隧道配置。
///
/// 参数：无
/// 核心流程：直接走会话库，返回持久化后的配置（含未启动的），
/// 前端据此渲染配置列表，运行状态另由 tunnel_list 提供。
#[tauri::command]
pub async fn tunnel_config_list(
    state: State<'_, AppState>,
) -> Result<Vec<TunnelConfig>, ErrorPayload> {
    state
        .store
        .tunnel_config_list()
        .await
        .map_err(ErrorPayload::from)
}

/// 新建一条隧道配置。
///
/// 参数：input 为前端填写的转发参数
/// 核心流程：校验与名称兜底都在核心层完成 → 返回落库后的完整配置（含 id 与时间戳）。
#[tauri::command]
pub async fn tunnel_config_create(
    state: State<'_, AppState>,
    input: TunnelConfigInput,
) -> Result<TunnelConfig, ErrorPayload> {
    state
        .store
        .tunnel_config_insert(&input)
        .await
        .map_err(ErrorPayload::from)
}

/// 更新一条隧道配置。
///
/// 参数：id 为配置标识，input 为新内容
/// 核心流程：覆盖可编辑字段并刷新 updated_at；配置已被删除时报未找到。
#[tauri::command]
pub async fn tunnel_config_update(
    state: State<'_, AppState>,
    id: String,
    input: TunnelConfigInput,
) -> Result<TunnelConfig, ErrorPayload> {
    state
        .store
        .tunnel_config_update(&id, &input)
        .await
        .map_err(ErrorPayload::from)
}

/// 删除一条隧道配置。
///
/// 参数：id 为配置标识
/// 核心流程：只删配置；正在运行的隧道不动，避免"删配置顺手断业务"，
/// 它以 "(已删除配置)" 的名字继续出现在运行列表里，由用户自己停。
#[tauri::command]
pub async fn tunnel_config_delete(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), ErrorPayload> {
    state
        .store
        .tunnel_config_delete(&id)
        .await
        .map_err(ErrorPayload::from)
}

/// 启动一条隧道。
///
/// 参数：id 为隧道配置标识
/// 核心流程：读配置 → 读它挂靠的会话 → 用会话的 host/port/username 与
/// extra 里的认证引用解出连接参数（凭据仍是走 resolve_auth 那一套）→
/// 组装 TunnelSpec → 在阻塞线程里建连并开始监听 → 登记进注册表并返回运行态；
/// 已在运行则直接返回现状，不重复建连。
#[tauri::command]
pub async fn tunnel_start(
    state: State<'_, AppState>,
    id: String,
) -> Result<TunnelInfo, ErrorPayload> {
    let config = state
        .store
        .tunnel_config_get(&id)
        .await
        .map_err(ErrorPayload::from)?
        .ok_or_else(|| ErrorPayload::from(CoreError::NotFound(format!("隧道配置 {id}"))))?;

    // 已经在跑 -> 直接回现状，避免同一条配置叠出第二条 SSH 连接
    if state.tunnels.contains(&id) {
        let handle = state
            .tunnels
            .get(&id)
            .ok_or_else(|| ErrorPayload::from(CoreError::Tunnel("隧道注册表不可用".into())))?;
        return Ok(info_of(&config.id, &config.name, &handle));
    }

    let session = state
        .store
        .get_session(&config.session_id)
        .await
        .map_err(ErrorPayload::from)?;
    let host = session
        .host
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ErrorPayload::from(CoreError::Validation("会话未填写主机，无法启动隧道".into()))
        })?
        .to_string();

    let auth = resolve_session_auth(&state.vault, &session).await?;
    // 隧道不建 PTY，尺寸与 term 填终端默认值即可；跳板机在会话里没有配置入口，固定直连
    let options = SshOptions {
        host,
        port: session.port.unwrap_or(22),
        username: session.username.clone().unwrap_or_default(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        // 会话已经连过时指纹已在 known_host 里；未确认的主机由前端走指纹弹窗
        // （错误 kind=hostKeyUnknown 带 detail）后调 trust_host_key 再重试
        accept_unknown_host_key: false,
        jump: None,
    };

    let kind = TunnelKind::parse(&config.kind).ok_or_else(|| {
        ErrorPayload::from(CoreError::Validation(format!(
            "未知的隧道类型：{}",
            config.kind
        )))
    })?;
    let spec = TunnelSpec {
        kind,
        listen_host: if config.listen_host.trim().is_empty() {
            "127.0.0.1".to_string()
        } else {
            config.listen_host.trim().to_string()
        },
        listen_port: config.listen_port,
        target_host: config.target_host.trim().to_string(),
        target_port: config.target_port,
    };

    let store = state.store.clone();
    // 建隧道会阻塞在内部运行时上 -> 放到阻塞线程池，避免占住 async 工作线程
    let handle =
        tauri::async_runtime::spawn_blocking(move || TunnelHandle::start(&spec, &store, &options))
            .await
            .map_err(|err| ErrorPayload::from(CoreError::Tunnel(format!("任务异常：{err}"))))?
            .map_err(ErrorPayload::from)?;

    let info = info_of(&config.id, &config.name, &handle);
    if !state.tunnels.insert(config.id.clone(), Arc::new(handle)) {
        return Err(ErrorPayload::from(CoreError::Tunnel(
            "隧道注册表不可用".into(),
        )));
    }
    Ok(info)
}

/// 停止并移除一条隧道。
///
/// 参数：id 为隧道配置标识
/// 核心流程：先从注册表摘下（避免停止过程中还被 list 看到）→
/// 在阻塞线程里等它真正停掉（远端撤销转发 + 断开连接）；未命中直接返回成功，
/// 前端重复点击或配置已被删除都不会报错。
#[tauri::command]
pub async fn tunnel_stop(state: State<'_, AppState>, id: String) -> Result<(), ErrorPayload> {
    if let Some(handle) = state.tunnels.remove(&id) {
        tauri::async_runtime::spawn_blocking(move || handle.stop())
            .await
            .map_err(|err| ErrorPayload::from(CoreError::Tunnel(format!("任务异常：{err}"))))?
            .map_err(ErrorPayload::from)?;
    }
    Ok(())
}

/// 列出全部运行中的隧道。
///
/// 参数：无
/// 核心流程：配置表读一次做名字与描述来源，再遍历运行注册表拼出结果；
/// 配置已被删除但隧道还在跑的项也能出现（名字回退成 "(已删除配置)"，
/// 转发参数取自句柄自身，不会因为查不到配置而 panic）；最后按名字排序，
/// 避免哈希表顺序让前端列表跳动。
#[tauri::command]
pub async fn tunnel_list(state: State<'_, AppState>) -> Result<Vec<TunnelInfo>, ErrorPayload> {
    let configs = state
        .store
        .tunnel_config_list()
        .await
        .map_err(ErrorPayload::from)?;
    let mut list: Vec<TunnelInfo> = state
        .tunnels
        .list()
        .iter()
        .map(
            |(id, handle)| match configs.iter().find(|config| &config.id == id) {
                Some(config) => info_of(id, &config.name, handle),
                None => info_of(id, DELETED_CONFIG_NAME, handle),
            },
        )
        .collect();
    list.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(list)
}
