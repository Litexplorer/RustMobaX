use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::{CoreError, CoreResult};
use crate::store::Store;

/// Argon2id 内存开销（KiB）。
const ARGON_MEMORY_KIB: u32 = 19 * 1024;
/// Argon2id 迭代轮数。
const ARGON_ITERATIONS: u32 = 2;
/// 盐长度。
pub const SALT_LEN: usize = 16;
/// AES-GCM nonce 长度。
pub const NONCE_LEN: usize = 12;
/// 派生密钥长度（AES-256）。
pub const KEY_LEN: usize = 32;

/// vault_meta 中盐的键名。
const META_SALT: &str = "salt";
/// vault_meta 中校验块 nonce 的键名。
const META_CHECK_NONCE: &str = "check_nonce";
/// vault_meta 中校验块密文的键名。
const META_CHECK_CIPHERTEXT: &str = "check_ciphertext";
/// 校验块明文：能解开即证明主密码正确。
const CHECK_PLAINTEXT: &[u8] = b"rustmobax-vault-check";

/// 生成密码学随机字节。
///
/// 参数：len 为需要的字节数
/// 核心流程：从系统熵源取随机数，用于盐与 nonce，保证每次加密都不重复。
pub fn random_bytes(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    OsRng.fill_bytes(&mut buf);
    buf
}

/// 由主密码与盐派生 256 位密钥。
///
/// 参数：password 为主密码，salt 为库中保存的盐
/// 核心流程：走 Argon2id 慢哈希，让暴力破解在内存与时间上都被抬高；
/// 返回值用 Zeroizing 包裹，离开作用域自动擦除。
pub fn derive_key(password: &str, salt: &[u8]) -> CoreResult<Zeroizing<Vec<u8>>> {
    let params = Params::new(ARGON_MEMORY_KIB, ARGON_ITERATIONS, 1, Some(KEY_LEN))
        .map_err(|err| CoreError::Vault(format!("密钥参数不可用：{err}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new(vec![0u8; KEY_LEN]);
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|err| CoreError::Vault(format!("密钥派生失败：{err}")))?;
    Ok(key)
}

/// 加密明文。
///
/// 参数：key 为 32 字节密钥，plaintext 为待加密内容
/// 核心流程：每次生成新 nonce → AES-256-GCM 加密 → 返回 (nonce, 密文)，
/// nonce 随密文一起落库，不需要保密但绝不能复用。
pub fn encrypt(key: &[u8], plaintext: &[u8]) -> CoreResult<(Vec<u8>, Vec<u8>)> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|err| CoreError::Vault(format!("密钥长度不正确：{err}")))?;
    let nonce = random_bytes(NONCE_LEN);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|err| CoreError::Vault(format!("加密失败：{err}")))?;
    Ok((nonce, ciphertext))
}

/// 解密密文。
///
/// 参数：key 为 32 字节密钥，nonce 为加密时生成的随机数，ciphertext 为密文
/// 核心流程：AES-256-GCM 解密并校验认证标记，密钥不对或数据被篡改都会失败。
pub fn decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> CoreResult<Zeroizing<Vec<u8>>> {
    if nonce.len() != NONCE_LEN {
        return Err(CoreError::Vault("密文头长度不正确".into()));
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|err| CoreError::Vault(format!("密钥长度不正确：{err}")))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| CoreError::Vault("解密失败：主密码错误或数据已损坏".into()))?;
    Ok(Zeroizing::new(plaintext))
}

/// 凭据库。
///
/// 无参数
/// 核心流程：持有会话库与内存中的派生密钥。密钥只在 unlock 之后存在，
/// lock 或进程退出即消失，因此密文离开本机也无法解密。
pub struct Vault {
    store: Store,
    key: Mutex<Option<Zeroizing<Vec<u8>>>>,
}

impl Vault {
    /// 绑定一个会话库。
    ///
    /// 参数：store 为底层存储
    /// 核心流程：初始为锁定态，密钥为空，需显式 unlock 才能读写凭据。
    pub fn new(store: Store) -> Self {
        Self {
            store,
            key: Mutex::new(None),
        }
    }

    /// 判断是否已经设置过主密码。
    ///
    /// 参数：无
    /// 核心流程：以库里是否存在盐为准，首次使用引导"设置主密码"。
    pub async fn is_initialized(&self) -> CoreResult<bool> {
        Ok(self.store.vault_meta_get(META_SALT).await?.is_some())
    }

    /// 判断当前是否已解锁。
    ///
    /// 参数：无
    /// 核心流程：只看内存里有没有密钥，不碰数据库。
    pub fn is_unlocked(&self) -> bool {
        self.key.lock().map(|slot| slot.is_some()).unwrap_or(false)
    }

    /// 首次设置主密码并解锁。
    ///
    /// 参数：password 为用户设置的主密码
    /// 核心流程：生成随机盐 → Argon2id 派生密钥 → 加密校验块并落库 →
    /// 密钥留在内存，后续读写凭据都复用它。
    pub async fn setup(&self, password: &str) -> CoreResult<()> {
        if password.is_empty() {
            return Err(CoreError::Vault("主密码不能为空".into()));
        }
        if self.is_initialized().await? {
            return Err(CoreError::Vault("主密码已设置过".into()));
        }
        let salt = random_bytes(SALT_LEN);
        let key = derive_key(password, &salt)?;
        let (nonce, ciphertext) = encrypt(&key, CHECK_PLAINTEXT)?;
        self.store.vault_meta_set(META_SALT, &BASE64.encode(&salt)).await?;
        self.store
            .vault_meta_set(META_CHECK_NONCE, &BASE64.encode(&nonce))
            .await?;
        self.store
            .vault_meta_set(META_CHECK_CIPHERTEXT, &BASE64.encode(&ciphertext))
            .await?;
        self.install(key)?;
        Ok(())
    }

    /// 用主密码解锁。
    ///
    /// 参数：password 为用户输入的主密码
    /// 核心流程：读出盐 → 重新派生密钥 → 试解校验块；
    /// 解得开说明密码正确，解不开就停在锁定态。
    pub async fn unlock(&self, password: &str) -> CoreResult<()> {
        let salt = self
            .store
            .vault_meta_get(META_SALT)
            .await?
            .ok_or_else(|| CoreError::Vault("尚未设置主密码".into()))?;
        let salt = BASE64
            .decode(salt)
            .map_err(|_| CoreError::Vault("盐数据已损坏".into()))?;
        let nonce = self
            .store
            .vault_meta_get(META_CHECK_NONCE)
            .await?
            .ok_or_else(|| CoreError::Vault("校验块缺失".into()))?;
        let ciphertext = self
            .store
            .vault_meta_get(META_CHECK_CIPHERTEXT)
            .await?
            .ok_or_else(|| CoreError::Vault("校验块缺失".into()))?;
        let nonce = BASE64
            .decode(nonce)
            .map_err(|_| CoreError::Vault("校验块已损坏".into()))?;
        let ciphertext = BASE64
            .decode(ciphertext)
            .map_err(|_| CoreError::Vault("校验块已损坏".into()))?;

        let key = derive_key(password, &salt)?;
        decrypt(&key, &nonce, &ciphertext)
            .map_err(|_| CoreError::Vault("主密码错误".into()))?;
        self.install(key)?;
        Ok(())
    }

    /// 锁定凭据库。
    ///
    /// 参数：无
    /// 核心流程：丢弃内存密钥，之后任何读写都会要求重新解锁。
    pub fn lock(&self) -> CoreResult<()> {
        let mut slot = self
            .key
            .lock()
            .map_err(|_| CoreError::Vault("凭据库状态不可用".into()))?;
        *slot = None;
        Ok(())
    }

    /// 保存一条凭据。
    ///
    /// 参数：kind 为用途分类，label 为展示名，secret 为明文
    /// 核心流程：要求已解锁 → 每次生成新 nonce 加密 → 只把密文写库 → 返回凭据标识。
    pub async fn save_secret(&self, kind: &str, label: &str, secret: &str) -> CoreResult<String> {
        if secret.is_empty() {
            return Err(CoreError::Vault("凭据内容不能为空".into()));
        }
        let key = self.require_key()?;
        let id = Uuid::new_v4().to_string();
        let (nonce, ciphertext) = encrypt(&key, secret.as_bytes())?;
        self.store
            .credential_insert(&id, kind, label, &nonce, &ciphertext)
            .await?;
        Ok(id)
    }

    /// 取出凭据明文。
    ///
    /// 参数：id 为凭据标识
    /// 核心流程：要求已解锁 → 读密文 → 解密 → 转成字符串交给连接层，
    /// 明文只在这一层之上短暂存在，不落库、不打日志。
    pub async fn reveal(&self, id: &str) -> CoreResult<String> {
        let stored = self
            .store
            .credential_secret(id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("凭据 {id} 不存在")))?;
        let key = self.require_key()?;
        let plaintext = decrypt(&key, &stored.nonce, &stored.ciphertext)?;
        String::from_utf8(plaintext.to_vec())
            .map_err(|_| CoreError::Vault("凭据内容不是合法文本".into()))
    }

    /// 写入内存密钥。
    ///
    /// 参数：key 为派生出的 32 字节密钥
    /// 核心流程：加锁替换旧值，覆盖时旧密钥随 Zeroizing 自动擦除。
    fn install(&self, key: Zeroizing<Vec<u8>>) -> CoreResult<()> {
        let mut slot = self
            .key
            .lock()
            .map_err(|_| CoreError::Vault("凭据库状态不可用".into()))?;
        *slot = Some(key);
        Ok(())
    }

    /// 取出内存密钥，未解锁时报错。
    ///
    /// 参数：无
    /// 核心流程：克隆一份交给加密调用，调用结束即擦除，避免长时间持有。
    fn require_key(&self) -> CoreResult<Zeroizing<Vec<u8>>> {
        let slot = self
            .key
            .lock()
            .map_err(|_| CoreError::Vault("凭据库状态不可用".into()))?;
        slot.as_ref()
            .cloned()
            .ok_or_else(|| CoreError::Vault("凭据库已锁定，请先解锁".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_then_decrypt_returns_original() {
        let key = derive_key("master-pass", &[7u8; SALT_LEN]).unwrap();
        let (nonce, ciphertext) = encrypt(&key, "s3cret-密码".as_bytes()).unwrap();
        let plaintext = decrypt(&key, &nonce, &ciphertext).unwrap();
        assert_eq!(&plaintext[..], "s3cret-密码".as_bytes());
    }

    #[test]
    fn wrong_password_cannot_decrypt() {
        let salt = [3u8; SALT_LEN];
        let good = derive_key("master-pass", &salt).unwrap();
        let bad = derive_key("master-pass2", &salt).unwrap();
        let (nonce, ciphertext) = encrypt(&good, b"payload").unwrap();
        assert!(decrypt(&bad, &nonce, &ciphertext).is_err());
    }

    #[test]
    fn same_password_with_different_salt_derives_different_key() {
        let first = derive_key("master-pass", &[1u8; SALT_LEN]).unwrap();
        let second = derive_key("master-pass", &[2u8; SALT_LEN]).unwrap();
        assert_ne!(&first[..], &second[..]);
    }

    #[test]
    fn encrypting_twice_uses_different_nonce() {
        let key = derive_key("master-pass", &[9u8; SALT_LEN]).unwrap();
        let (first, _) = encrypt(&key, b"same").unwrap();
        let (second, _) = encrypt(&key, b"same").unwrap();
        assert_ne!(first, second);
    }

    /// 建立已解锁的内存凭据库。
    ///
    /// 参数：无
    /// 核心流程：内存库 + 固定主密码，供各用例验证读写链路。
    async fn unlocked_vault() -> Vault {
        let vault = Vault::new(Store::open_in_memory().await.unwrap());
        vault.setup("master-pass").await.unwrap();
        vault
    }

    #[tokio::test]
    async fn saves_and_reveals_secret_after_setup() {
        let vault = unlocked_vault().await;
        assert!(vault.is_initialized().await.unwrap());
        let id = vault.save_secret("password", "生产库", "s3cret").await.unwrap();
        assert_eq!(vault.reveal(&id).await.unwrap(), "s3cret");
    }

    #[tokio::test]
    async fn locked_vault_refuses_to_reveal() {
        let vault = unlocked_vault().await;
        let id = vault.save_secret("password", "生产库", "s3cret").await.unwrap();
        vault.lock().unwrap();
        assert!(!vault.is_unlocked());
        assert!(vault.reveal(&id).await.is_err());
    }

    /// credential_reveal 的锁定分支：未解锁时必须给出明确错误，而不是空串或 panic。
    #[tokio::test]
    async fn reveal_reports_locked_error_with_message() {
        let vault = unlocked_vault().await;
        let id = vault
            .save_secret("key_passphrase", "构建机私钥", "key-pass")
            .await
            .unwrap();
        vault.lock().unwrap();
        let err = vault.reveal(&id).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Vault(ref message) if message.contains("已锁定")),
            "未解锁时应报锁定错误，实际：{err:?}"
        );
    }

    /// credential_reveal 的正常分支：解锁后取回的明文与保存时完全一致（含私钥口令场景）。
    #[tokio::test]
    async fn reveal_returns_saved_plaintext_after_relock_and_unlock() {
        let vault = unlocked_vault().await;
        let id = vault
            .save_secret("key_passphrase", "构建机私钥", "key-pass-口令")
            .await
            .unwrap();
        vault.lock().unwrap();
        vault.unlock("master-pass").await.unwrap();
        assert_eq!(vault.reveal(&id).await.unwrap(), "key-pass-口令");
    }

    /// credential_reveal 的缺失分支：id 不存在时报 NotFound，前端据此退回手动输入。
    #[tokio::test]
    async fn reveal_unknown_id_reports_not_found() {
        let vault = unlocked_vault().await;
        let err = vault.reveal("no-such-credential").await.unwrap_err();
        assert!(
            matches!(err, CoreError::NotFound(_)),
            "不存在的 id 应报 NotFound，实际：{err:?}"
        );
    }

    #[tokio::test]
    async fn wrong_password_keeps_vault_locked() {
        let vault = unlocked_vault().await;
        vault.lock().unwrap();
        assert!(vault.unlock("wrong-pass").await.is_err());
        assert!(!vault.is_unlocked());
        vault.unlock("master-pass").await.unwrap();
        assert!(vault.is_unlocked());
    }
}
