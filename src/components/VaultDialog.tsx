import { useState } from "react";
import type { Credential, VaultStatus } from "../api/vault";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  status: VaultStatus;
  credentials: Credential[];
  busy: boolean;
  /** 后端返回的失败原因（主密码错误、已锁定等）。 */
  error: string | null;
  onSetup: (password: string) => void;
  onUnlock: (password: string) => void;
  onLock: () => void;
  onSave: (label: string, secret: string) => void;
  onDelete: (id: string) => void;
  onClose: () => void;
};

/**
 * 凭据库弹窗。
 *
 * 参数：见 Props
 * 核心流程：按状态分三段：未设置主密码时设置、已设置未解锁时解锁、
 * 已解锁时管理凭据；明文只在输入框里短暂存在，提交后由后端加密落库。
 */
export function VaultDialog(props: Props) {
  const { status, credentials, busy, error, onSetup, onUnlock, onSave, onDelete, onLock, onClose } =
    props;
  // Esc = 关闭（与标题栏 × 一致）；请求在飞时底部的「关闭」是禁用的，Esc 也一并失效
  useEscapeClose(onClose, !busy);
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [label, setLabel] = useState("");
  const [secret, setSecret] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);

  const settingUp = !status.initialized;

  /**
   * 提交主密码。
   *
   * 参数：无
   * 核心流程：设置时需要两次一致且不短于 6 位；解锁时只校验非空，
   * 真正的比对交给后端。
   */
  const submitPassword = () => {
    if (password === "") {
      setLocalError("请输入主密码");
      return;
    }
    if (settingUp && password !== confirmPassword) {
      setLocalError("两次输入的主密码不一致");
      return;
    }
    if (settingUp && password.length < 6) {
      setLocalError("主密码至少 6 位");
      return;
    }
    setLocalError(null);
    if (settingUp) onSetup(password);
    else onUnlock(password);
  };

  /**
   * 保存新凭据。
   *
   * 参数：无
   * 核心流程：名称与内容都必填，提交后清空输入框，避免明文留在界面上。
   */
  const submitCredential = () => {
    if (label.trim() === "") {
      setLocalError("请填写凭据名称");
      return;
    }
    if (secret === "") {
      setLocalError("请填写凭据内容");
      return;
    }
    setLocalError(null);
    onSave(label.trim(), secret);
    setLabel("");
    setSecret("");
  };

  const shownError = localError ?? error;

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal" role="dialog" aria-label="凭据库">
        <div className="dialog-header">
          <h2>凭据库</h2>
          <button type="button" className="dialog-close" aria-label="关闭" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="dialog-body">
          <p className="detail-empty">
            凭据用主密码派生的密钥加密后保存在本地库里，库里只有密文；
            主密码本身不落盘，忘记后只能删除旧凭据重新录入。
          </p>

          {status.initialized && (
            <p className="detail-empty">
              状态：{status.unlocked ? "已解锁" : "已锁定"}
              {status.unlocked && ` · ${credentials.length} 条凭据`}
            </p>
          )}

          {!status.unlocked && (
            <section className="settings-group">
              <h3>{settingUp ? "设置主密码" : "解锁凭据库"}</h3>
              <div className="setting-row">
                <label className="setting-label" htmlFor="vault-password">
                  主密码
                </label>
                <input
                  id="vault-password"
                  type="password"
                  value={password}
                  autoFocus
                  onChange={(event) => {
                    setPassword(event.target.value);
                    setLocalError(null);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") submitPassword();
                  }}
                />
              </div>
              {settingUp && (
                <div className="setting-row">
                  <label className="setting-label" htmlFor="vault-password-again">
                    确认主密码
                  </label>
                  <input
                    id="vault-password-again"
                    type="password"
                    value={confirmPassword}
                    onChange={(event) => {
                      setConfirmPassword(event.target.value);
                      setLocalError(null);
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") submitPassword();
                    }}
                  />
                </div>
              )}
            </section>
          )}

          {status.unlocked && (
            <>
              <section className="settings-group">
                <h3>新增凭据</h3>
                <div className="setting-row">
                  <label className="setting-label" htmlFor="vault-label">
                    名称
                  </label>
                  <input
                    id="vault-label"
                    value={label}
                    placeholder="例如 生产-web-01 密码"
                    onChange={(event) => {
                      setLabel(event.target.value);
                      setLocalError(null);
                    }}
                  />
                </div>
                <div className="setting-row">
                  <label className="setting-label" htmlFor="vault-secret">
                    密码
                  </label>
                  <input
                    id="vault-secret"
                    type="password"
                    value={secret}
                    onChange={(event) => {
                      setSecret(event.target.value);
                      setLocalError(null);
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") submitCredential();
                    }}
                  />
                </div>
                <div className="modal-actions">
                  <button type="button" onClick={submitCredential} disabled={busy}>
                    保存凭据
                  </button>
                </div>
              </section>

              <section className="settings-group">
                <h3>已保存凭据</h3>
                {credentials.length === 0 ? (
                  <p className="detail-empty">还没有保存任何凭据</p>
                ) : (
                  <ul className="credential-list">
                    {credentials.map((item) => (
                      <li key={item.id} className="credential-row">
                        <span className="credential-label">{item.label}</span>
                        <button
                          type="button"
                          className="danger"
                          disabled={busy}
                          onClick={() => onDelete(item.id)}
                        >
                          删除
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            </>
          )}

          {shownError && <p className="form-error">{shownError}</p>}
        </div>

        <div className="modal-actions">
          {status.unlocked && (
            <button type="button" onClick={onLock} disabled={busy}>
              锁定
            </button>
          )}
          {!status.unlocked && (
            <button type="button" className="primary" onClick={submitPassword} disabled={busy}>
              {busy ? "处理中…" : settingUp ? "设置并解锁" : "解锁"}
            </button>
          )}
          <button type="button" onClick={onClose} disabled={busy}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
