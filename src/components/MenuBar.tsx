import { useEffect, useState } from "react";
import { IconCheck } from "./icons";

export type MenuEntry =
  | {
      kind: "item";
      label: string;
      onSelect: () => void;
      disabled?: boolean;
      /** true 时在条目左侧显示勾选标记，用于视图/设置类开关 */
      checked?: boolean;
    }
  | { kind: "separator" };

export type MenuGroup = { label: string; entries: MenuEntry[] };

type Props = { groups: MenuGroup[] };

/**
 * 顶部菜单栏。
 *
 * 参数：groups 为菜单分组，每组含若干条目或分隔符
 * 核心流程：点击菜单名展开该项 → 点击条目执行动作并收起 → 点击窗口其它位置
 * 或按 Esc 收起；disabled 条目只占位不响应，checked 条目显示勾选态。
 */
export function MenuBar({ groups }: Props) {
  const [openLabel, setOpenLabel] = useState<string | null>(null);

  // 点击空白处收起 -> 避免菜单常驻遮挡内容
  useEffect(() => {
    if (openLabel === null) return;
    const close = () => setOpenLabel(null);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [openLabel]);

  // Esc 收起 -> 键盘用户不必去找空白处点击
  useEffect(() => {
    if (openLabel === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenLabel(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [openLabel]);

  return (
    <nav className="menubar" role="menubar" aria-label="主菜单">
      {groups.map((group) => (
        <div key={group.label} className="menu-root">
          <button
            type="button"
            className={`menu-title ${openLabel === group.label ? "is-open" : ""}`}
            aria-haspopup="menu"
            aria-expanded={openLabel === group.label}
            onClick={(event) => {
              event.stopPropagation();
              setOpenLabel((prev) => (prev === group.label ? null : group.label));
            }}
          >
            {group.label}
          </button>
          {openLabel === group.label && (
            <div
              className="menu-dropdown"
              role="menu"
              aria-label={group.label}
              onClick={(event) => event.stopPropagation()}
            >
              {group.entries.map((entry, index) =>
                entry.kind === "separator" ? (
                  <div key={`sep-${index}`} className="menu-separator" role="separator" />
                ) : (
                  <button
                    key={entry.label}
                    type="button"
                    className="menu-item"
                    role={entry.checked === undefined ? "menuitem" : "menuitemcheckbox"}
                    aria-checked={entry.checked}
                    disabled={entry.disabled}
                    onClick={() => {
                      setOpenLabel(null);
                      entry.onSelect();
                    }}
                  >
                    <span className="menu-check">
                      {entry.checked ? <IconCheck size={12} /> : null}
                    </span>
                    <span className="menu-item-label">{entry.label}</span>
                  </button>
                ),
              )}
            </div>
          )}
        </div>
      ))}
      <div className="menubar-spacer" />
      <span className="menubar-brand">RustMobaX</span>
    </nav>
  );
}
