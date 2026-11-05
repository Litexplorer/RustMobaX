import { useEffect, useRef, useState } from "react";
import { IconMultiExec } from "./icons";

/** 一条广播目标：sessionId 用于回查终端 id，name 只用于界面展示 */
export type MultiExecTarget = { sessionId: string; name: string };

type Props = {
  /** 当前所有可广播的终端会话（已打开的终端标签） */
  targets: MultiExecTarget[];
  /** 已勾选的 sessionId */
  selected: string[];
  onToggle: (sessionId: string) => void;
  onSelectAll: () => void;
  onClear: () => void;
  onClose: () => void;
  onSend: (text: string) => Promise<{ ok: number; failed: number }>;
};

/**
 * 主区底部的多执行输入条。
 *
 * 参数：targets 为可广播的终端会话，selected 为已勾选的会话 id，
 * onToggle/onSelectAll/onClear 改勾选，onClose 收起输入条，
 * onSend 由上层实现真正的写入并回报成功/失败条数。
 * 核心流程：回车发送 -> 发送中禁用按钮防重入 -> 发完清空输入并保持焦点；
 * 这里不碰任何 Tauri API，也不抢 Esc（Esc 留给终端）。
 */
export function MultiExecBar(props: Props) {
  const { targets, selected, onToggle, onSelectAll, onClear, onClose, onSend } = props;
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  /** 上一次发送的结果，null 表示还没发过 */
  const [result, setResult] = useState<{ ok: number; failed: number } | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // 打开即聚焦：用户点开输入条就能直接敲命令
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const send = async () => {
    if (sending || selected.length === 0 || text === "") return;
    setSending(true);
    try {
      const outcome = await onSend(text);
      setResult(outcome);
      setText("");
    } finally {
      setSending(false);
      // 连续发命令时焦点不能丢：输入条自己没有任何卸载风险，这里同步还给输入框
      inputRef.current?.focus();
    }
  };

  const status = (() => {
    if (selected.length === 0) return { text: "请至少选择 1 个会话", tone: "is-warn" };
    if (result === null) return null;
    if (result.failed === 0) return { text: `已发送到 ${result.ok} 个会话`, tone: "is-ok" };
    if (result.ok === 0) return { text: `${result.failed} 个会话发送失败`, tone: "is-error" };
    return {
      text: `已发送到 ${result.ok} 个会话 · ${result.failed} 个会话发送失败`,
      tone: "is-error",
    };
  })();

  return (
    <section className="multiexec-bar" aria-label="多执行">
      <div className="multiexec-head">
        <span className="multiexec-icon">
          <IconMultiExec size={12} />
        </span>
        <span className="multiexec-title">多执行</span>
        <span className="multiexec-count">
          已选 {selected.length}/{targets.length}
        </span>
        <button
          type="button"
          className="multiexec-btn"
          onClick={onSelectAll}
          disabled={targets.length === 0}
        >
          全部
        </button>
        <button
          type="button"
          className="multiexec-btn"
          onClick={onClear}
          disabled={selected.length === 0}
        >
          全不选
        </button>
        <button
          type="button"
          className="multiexec-btn multiexec-close"
          onClick={onClose}
          title="收起多执行输入条"
          aria-label="收起多执行输入条"
        >
          ×
        </button>
      </div>

      <div className="multiexec-targets">
        {targets.length === 0 ? (
          <span className="multiexec-empty">还没有已打开的终端会话</span>
        ) : (
          targets.map((target) => {
            const on = selected.includes(target.sessionId);
            return (
              <button
                key={target.sessionId}
                type="button"
                className={`multiexec-tag ${on ? "is-on" : "is-off"}`}
                aria-pressed={on}
                title={on ? `取消广播到 ${target.name}` : `广播到 ${target.name}`}
                onClick={() => onToggle(target.sessionId)}
              >
                {target.name}
              </button>
            );
          })
        )}
      </div>

      <div className="multiexec-input-row">
        <input
          ref={inputRef}
          className="multiexec-input"
          value={text}
          placeholder="输入命令，回车发送到已选会话（Shift+Enter 换行由终端决定）"
          aria-label="多执行命令"
          disabled={sending}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            // 只认不带 Shift 的回车：Shift+Enter 不进发送，避免误发半条命令
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              event.stopPropagation();
              void send();
            }
          }}
        />
        {status !== null && <span className={`multiexec-status ${status.tone}`}>{status.text}</span>}
        <button
          type="button"
          className="multiexec-send"
          onClick={() => void send()}
          disabled={sending || selected.length === 0 || text === ""}
        >
          {sending ? "发送中…" : "发送"}
        </button>
      </div>
    </section>
  );
}
