import { useState } from "react";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  title: string;
  label: string;
  initialValue: string;
  busy: boolean;
  onSubmit: (value: string) => void;
  onCancel: () => void;
};

/**
 * 单输入框弹窗，用于新建与重命名分组。
 *
 * 参数：见 Props
 * 核心流程：本地维护输入值 → 空值直接拦下 → 非空交给父层提交。
 */
export function GroupPromptDialog(props: Props) {
  const { title, label, initialValue, busy, onSubmit, onCancel } = props;
  const [value, setValue] = useState(initialValue);
  const [error, setError] = useState<string | null>(null);

  // Esc = 取消；保存中「取消」按钮是禁用的，Esc 也一并失效
  useEscapeClose(onCancel, !busy);

  /**
   * 提交输入值。
   *
   * 参数：无
   * 核心流程：去空白后为空则提示，否则上交父层。
   */
  const submit = () => {
    const trimmed = value.trim();
    if (trimmed === "") {
      setError("分组名称不能为空");
      return;
    }
    onSubmit(trimmed);
  };

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-small" role="dialog" aria-label={title}>
        <h2>{title}</h2>
        <label className="field">
          <span>{label}</span>
          <input
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
        {error && <p className="form-error">{error}</p>}
        <div className="modal-actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button type="button" className="primary" onClick={submit} disabled={busy}>
            {busy ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
