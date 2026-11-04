import type { ReactNode } from "react";

export type ToolbarAction =
  | {
      kind: "button";
      icon: ReactNode;
      label: string;
      onClick: () => void;
      disabled?: boolean;
      title?: string;
      /** 开关类按钮（如分屏）的按下态 */
      active?: boolean;
    }
  | { kind: "divider" };

type Props = { actions: ToolbarAction[] };

/**
 * 大图标工具栏。
 *
 * 参数：actions 为按钮与分隔符序列
 * 核心流程：按顺序渲染「图标在上、小字在下」的双排按钮，分隔符渲染成竖线，
 * 尾部留白让按钮组靠左对齐；hover 只改背景与边框，不改变尺寸避免抖动。
 */
export function Toolbar({ actions }: Props) {
  return (
    <div className="toolbar-strip" role="toolbar" aria-label="主工具栏">
      {actions.map((action, index) =>
        action.kind === "divider" ? (
          <div key={`divider-${index}`} className="toolbar-divider" aria-hidden="true" />
        ) : (
          <button
            key={action.label}
            type="button"
            className={`toolbar-button ${action.active ? "is-active" : ""}`}
            title={action.title ?? action.label}
            aria-pressed={action.active}
            disabled={action.disabled}
            onClick={action.onClick}
          >
            <span className="toolbar-icon">{action.icon}</span>
            <span className="toolbar-label">{action.label}</span>
          </button>
        ),
      )}
      <div className="toolbar-spacer" aria-hidden="true" />
    </div>
  );
}
