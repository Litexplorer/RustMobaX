import type { ImportPreview } from "../api/session";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  preview: ImportPreview;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
};

/**
 * 导入前的预览与确认弹窗。
 *
 * 参数：见 Props
 * 核心流程：展示将导入的会话数量、涉及的分组结构与被跳过条目，
 * 用户确认后才真正写库，避免"选了文件就直接改库"。
 */
export function ImportPreviewDialog(props: Props) {
  const { preview, busy, onConfirm, onCancel } = props;
  // Esc = 取消；导入进行中「取消」按钮禁用，Esc 也不生效
  useEscapeClose(onCancel, !busy);
  const groupCount = new Set(
    preview.sessions.flatMap((session) => session.groupPath.join(" / ")).filter(Boolean),
  ).size;

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal" role="dialog" aria-label="导入预览">
        <h2>导入预览</h2>

        <dl className="summary">
          <dt>可导入会话</dt>
          <dd>{preview.sessions.length}</dd>
          <dt>涉及分组</dt>
          <dd>{groupCount}</dd>
          <dt>跳过条目</dt>
          <dd>{preview.skipped.length}</dd>
        </dl>

        {preview.sessions.length > 0 && (
          <div className="preview-block">
            <h3>将要导入</h3>
            <ul className="preview-list">
              {preview.sessions.slice(0, 12).map((session, index) => (
                <li key={`${session.name}-${index}`}>
                  <span className={`protocol-tag protocol-${session.protocol}`}>
                    {session.protocol.toUpperCase()}
                  </span>
                  {session.name}
                  {session.host ? ` · ${session.host}` : ""}
                  {session.groupPath.length > 0
                    ? ` · ${session.groupPath.join(" / ")}`
                    : ""}
                </li>
              ))}
              {preview.sessions.length > 12 && (
                <li className="more">… 其余 {preview.sessions.length - 12} 条</li>
              )}
            </ul>
          </div>
        )}

        {preview.skipped.length > 0 && (
          <div className="preview-block">
            <h3>将被跳过</h3>
            <ul className="preview-list skipped">
              {preview.skipped.map((entry, index) => (
                <li key={`${entry.section}-${index}`}>
                  {entry.section}：{entry.reason}
                </li>
              ))}
            </ul>
          </div>
        )}

        <div className="modal-actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="primary"
            onClick={onConfirm}
            disabled={busy || preview.sessions.length === 0}
          >
            {busy ? "导入中…" : `导入 ${preview.sessions.length} 条`}
          </button>
        </div>
      </div>
    </div>
  );
}
