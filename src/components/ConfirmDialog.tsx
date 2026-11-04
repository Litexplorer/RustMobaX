import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  message: string;
  onConfirm: () => void;
  onCancel: () => void;
};

/**
 * 通用确认弹窗。
 *
 * 参数：见 Props
 * 核心流程：只负责展示与回传选择，确认后的动作由调用方提供，
 * 避免把业务判断塞进组件里。
 */
export function ConfirmDialog(props: Props) {
  const { message, onConfirm, onCancel } = props;
  // 确认类弹窗：Esc 一律等于「取消」，绝不等于「确认」
  useEscapeClose(onCancel);
  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-small" role="dialog" aria-label="确认操作">
        <h2>请确认</h2>
        <p className="confirm-message">{message}</p>
        <div className="modal-actions">
          <button type="button" onClick={onCancel}>
            取消
          </button>
          <button type="button" className="danger" onClick={onConfirm}>
            确认
          </button>
        </div>
      </div>
    </div>
  );
}
