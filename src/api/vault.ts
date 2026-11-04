import { invoke } from "@tauri-apps/api/core";

export type VaultStatus = {
  /** 是否已经设置过主密码。 */
  initialized: boolean;
  /** 本次运行是否已解锁。 */
  unlocked: boolean;
};

export type Credential = {
  id: string;
  kind: string;
  label: string;
  updatedAt: number;
};

/**
 * 查询凭据库状态。
 *
 * 参数：无
 * 核心流程：一次拿到"是否设置过"与"是否已解锁"，前端据此决定弹哪个界面。
 */
export function vaultStatus(): Promise<VaultStatus> {
  return invoke<VaultStatus>("vault_status");
}

/**
 * 设置主密码。
 *
 * 参数：password 为用户设置的主密码
 * 核心流程：后端生成盐与校验块落库，成功后立即处于解锁态。
 */
export function vaultSetup(password: string): Promise<void> {
  return invoke("vault_setup", { password });
}

/**
 * 解锁凭据库。
 *
 * 参数：password 为用户输入的主密码
 * 核心流程：主密码错误时后端直接报错，前端保持锁定界面。
 */
export function vaultUnlock(password: string): Promise<void> {
  return invoke("vault_unlock", { password });
}

/**
 * 锁定凭据库。
 *
 * 参数：无
 * 核心流程：丢弃后端内存中的密钥，之后读写凭据都需要重新解锁。
 */
export function vaultLock(): Promise<void> {
  return invoke("vault_lock");
}

/**
 * 保存一条凭据。
 *
 * 参数：kind 为用途分类，label 为展示名，secret 为明文
 * 核心流程：明文只在本次调用里出现，后端加密后只存密文，返回凭据标识。
 */
export function credentialSave(kind: string, label: string, secret: string): Promise<string> {
  return invoke<string>("credential_save", { kind, label, secret });
}

/**
 * 取出单条凭据的明文。
 *
 * 参数：id 为凭据标识
 * 核心流程：要求凭据库已解锁；返回的明文只交给本次认证使用（私钥口令），
 * 调用方不得写日志、不得持久化；未解锁或 id 不存在时后端直接报错。
 */
export function credentialReveal(id: string): Promise<string> {
  return invoke<string>("credential_reveal", { id });
}

/**
 * 列出全部凭据。
 *
 * 参数：无
 * 核心流程：只取元信息用于选择列表，密文不经过 IPC。
 */
export function credentialList(): Promise<Credential[]> {
  return invoke<Credential[]>("credential_list");
}

/**
 * 删除一条凭据。
 *
 * 参数：id 为凭据标识
 * 核心流程：返回受影响行数，引用它的会话会退回手动输入密码。
 */
export function credentialDelete(id: string): Promise<number> {
  return invoke<number>("credential_delete", { id });
}
