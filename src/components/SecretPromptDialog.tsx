import { useState } from "react";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  title: string;
  label: string;
  hint?: string;
  busy: boolean;
  /**
   * 凭据库当前是否可用（已解锁才为 true）。
   * 语义是「能不能入库」，不再是「要不要显示勾选框」：
   * 不可用时勾选框照常出现但置灰，旁边给出解锁/初始化入口。
   */
  allowSaveToVault?: boolean;
  /** 凭据库是否已经设置过主密码 -> 决定提示按钮叫「解锁」还是「设置凭据库」 */
  vaultInitialized?: boolean;
  /** 勾选存入凭据库时名称输入框的默认值 */
  defaultVaultLabel?: string;
  /** 点「解锁 / 设置凭据库」：交给父层走既有的凭据库弹窗流程，本弹窗保持挂载 */
  onRequestVault?: () => void;
  /** 不涉及入库时的提交回调 */
  onSubmit?: (value: string) => void;
  /** 提供后接管提交：save 表示用户是否勾选入库，label 为用户确认过的凭据名称 */
  onSubmitWithSave?: (value: string, save: boolean, label: string) => void;
  onCancel: () => void;
};

/**
 * 密码 / 口令输入弹窗。
 *
 * 参数：见 Props
 * 核心流程：输入值只在内存里传递，不落库、不打日志；
 * 「存入凭据库」勾选框始终渲染，凭据库未解锁或未初始化时置灰并给出入口按钮，
 * 点按钮时本组件不卸载（父层不关弹窗），所以已输入的密码与凭据名称都不会丢；
 * 提交时把是否入库一并交给父层决定。
 */
export function SecretPromptDialog(props: Props) {
  const {
    title,
    label,
    hint,
    busy,
    allowSaveToVault,
    vaultInitialized,
    defaultVaultLabel,
    onRequestVault,
    onSubmit,
    onSubmitWithSave,
    onCancel,
  } = props;
  const [value, setValue] = useState("");
  const [saveToVault, setSaveToVault] = useState(false);
  const [vaultLabel, setVaultLabel] = useState(defaultVaultLabel ?? "");
  const [error, setError] = useState<string | null>(null);

  // Esc = 取消：与「取消」按钮同一条路径，父层把运行态置 null 后本组件卸载，
  // 已输入的密码 / 勾选 / 凭据名称随组件一起丢弃，不会残留
  useEscapeClose(onCancel, !busy);

  /** 勾选框能否真正勾上：既要有入库回调，也要凭据库已解锁 */
  const vaultReady = allowSaveToVault === true && onSubmitWithSave !== undefined;
  /** 未设置主密码 -> 引导初始化；已设置但锁着 -> 引导解锁 */
  const vaultActionLabel = vaultInitialized === false ? "设置凭据库" : "解锁";
  const vaultHint =
    vaultInitialized === false ? "凭据库未初始化" : "凭据库未解锁";

  /**
   * 提交输入值。
   *
   * 参数：无
   * 核心流程：空值先拦下；勾选入库时再校验凭据名称；
   * 有入库能力时把「是否勾选」交给父层，否则走普通提交。
   */
  const submit = () => {
    if (value === "") {
      setError("请输入内容");
      return;
    }
    if (vaultReady && saveToVault && vaultLabel.trim() === "") {
      setError("请填写凭据名称");
      return;
    }
    if (onSubmitWithSave) {
      onSubmitWithSave(value, vaultReady && saveToVault, vaultLabel.trim());
      return;
    }
    onSubmit?.(value);
  };

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-small" role="dialog" aria-label={title}>
        <h2>{title}</h2>
        <label className="field">
          <span>{label}</span>
          <input
            type="password"
            value={value}
            autoFocus
            onChange={(event) => {
              setValue(event.target.value);
              setError(null);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") submit();
            }}
          />
        </label>
        {hint && <p className="detail-empty">{hint}</p>}

        <section className="settings-group">
          <div className="setting-row">
            <label className="setting-label">
              <input
                type="checkbox"
                className="inline-check"
                checked={vaultReady && saveToVault}
                disabled={!vaultReady || busy}
                onChange={(event) => {
                  setSaveToVault(event.target.checked);
                  setError(null);
                }}
              />
              存入凭据库
            </label>
            <span className="setting-note">
              {vaultReady ? "加密保存后，下次连接不再询问" : vaultHint}
            </span>
          </div>
          {!vaultReady && (
            <div className="setting-row">
              <label className="setting-label">凭据库</label>
              <button
                type="button"
                style={{ justifySelf: "start" }}
                onClick={onRequestVault}
                disabled={busy || !onRequestVault}
              >
                {vaultActionLabel}
              </button>
            </div>
          )}
          {(saveToVault || !vaultReady) && (
            <div className="setting-row">
              <label className="setting-label" htmlFor="secret-vault-label">
                凭据名称
              </label>
              <input
                id="secret-vault-label"
                value={vaultLabel}
                disabled={!vaultReady}
                onChange={(event) => {
                  setVaultLabel(event.target.value);
                  setError(null);
                }}
              />
            </div>
          )}
        </section>

        {error && <p className="form-error">{error}</p>}
        <div className="modal-actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button type="button" className="primary" onClick={submit} disabled={busy}>
            {busy ? "连接中…" : "连接"}
          </button>
        </div>
      </div>
    </div>
  );
}
