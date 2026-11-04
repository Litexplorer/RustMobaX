import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  /** 待确认的剪贴板原文 */
  text: string;
  onConfirm: () => void;
  onCancel: () => void;
};

/**
 * 把剪贴板文本切成行。
 *
 * 参数：text 为剪贴板原文
 * 核心流程：先把 CRLF 与 CR 统一成 LF 再切分，最后去掉末尾换行产生的空行，
 * 这样"一行内容带结尾换行"仍算单行，不会误触发确认。
 */
export function splitPasteLines(text: string): string[] {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const lines = normalized.split("\n");
  while (lines.length > 1 && lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines;
}

/**
 * 多行粘贴确认弹窗。
 *
 * 参数：text 为待写入终端的剪贴板内容，onConfirm/onCancel 为两个分支
 * 核心流程：算出总行数 -> 只展示前 3 行预览 -> 用户确认后才真正写入终端，
 * 避免误粘贴多行命令被 shell 立即执行。
 */
export function PasteConfirmDialog({ text, onConfirm, onCancel }: Props) {
  const lines = splitPasteLines(text);
  const preview = lines.slice(0, 3);

  // 确认类弹窗：Esc = 取消粘贴，内容不会写进终端
  useEscapeClose(onCancel);

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal" role="dialog" aria-label="粘贴确认">
        <h2>粘贴多行内容？</h2>
        <p className="confirm-message">
          剪贴板共 {lines.length} 行，确认后会一次性写入终端（可能被立即执行）。
        </p>
        <pre className="paste-preview">{preview.join("\n")}</pre>
        {lines.length > preview.length && (
          <p className="paste-more">其余 {lines.length - preview.length} 行未显示</p>
        )}
        <div className="modal-actions">
          <button type="button" onClick={onCancel}>
            取消
          </button>
          <button type="button" className="danger" onClick={onConfirm}>
            粘贴
          </button>
        </div>
      </div>
    </div>
  );
}
