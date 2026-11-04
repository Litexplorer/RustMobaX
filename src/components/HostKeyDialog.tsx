import type { HostKeyInfo } from "../api/terminal";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  info: HostKeyInfo;
  /** mismatch 表示库里的指纹和这次观察到的不一致，必须硬阻断。 */
  mode: "unknown" | "mismatch";
  busy: boolean;
  onTrust: () => void;
  onForget: () => void;
  onCancel: () => void;
};

/**
 * 主机指纹确认弹窗。
 *
 * 参数：见 Props
 * 核心流程：未知主机时展示两种指纹请用户核对后信任；
 * 指纹变更时只提供"删除旧记录"这条危险操作，不允许直接信任。
 */
export function HostKeyDialog(props: Props) {
  const { info, mode, busy, onTrust, onForget, onCancel } = props;
  const isMismatch = mode === "mismatch";

  // Esc = 取消（信任 / 删除旧记录都必须用户明确点按钮）；
  // 处理中「取消」按钮禁用，Esc 也一并失效，避免中途掐断连接流程
  useEscapeClose(onCancel, !busy);

  return (
    <div className="modal-backdrop" role="presentation">
      <div
        className={`modal modal-small ${isMismatch ? "is-danger" : ""}`}
        role="dialog"
        aria-label="主机指纹确认"
      >
        <h2>{isMismatch ? "主机密钥已变更" : "未知主机"}</h2>

        {isMismatch && (
          <p className="form-error">
            这台主机的指纹与本地记录不一致，可能存在中间人攻击，已阻断连接。
            只有在你确认服务器确实重装或换了密钥时，才应删除旧记录。
          </p>
        )}

        <dl className="summary">
          <dt>主机</dt>
          <dd>
            {info.host}:{info.port}
          </dd>
          <dt>密钥类型</dt>
          <dd>{info.keyType}</dd>
          <dt>SHA256</dt>
          <dd className="fingerprint">{info.sha256}</dd>
          <dt>MD5</dt>
          <dd className="fingerprint">{info.md5}</dd>
        </dl>

        <div className="modal-actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          {isMismatch ? (
            <button type="button" className="danger" onClick={onForget} disabled={busy}>
              {busy ? "处理中…" : "删除旧记录并重连"}
            </button>
          ) : (
            <button type="button" className="primary" onClick={onTrust} disabled={busy}>
              {busy ? "连接中…" : "信任并连接"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
