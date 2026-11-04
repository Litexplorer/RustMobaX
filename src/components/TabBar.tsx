import type { Protocol } from "../api/session";
import { IconChevronLeft, IconChevronRight, IconClose, IconFolder, IconServer, IconTerminal } from "./icons";

export type TabItem = {
  id: string;
  title: string;
  subtitle: string;
  protocol: Protocol;
};

type Props = {
  tabs: TabItem[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  /** 侧栏是否展开，决定右侧按钮显示 ◀ 还是 ▶ */
  sidebarVisible: boolean;
  onToggleSidebar: () => void;
};

/**
 * 按协议取标签图标。
 *
 * 参数：protocol 为会话协议
 * 核心流程：本地终端、SFTP、SSH 各用一套图标，便于在多标签下快速分辨。
 */
function tabIcon(protocol: Protocol) {
  if (protocol === "local") return <IconTerminal size={14} />;
  if (protocol === "sftp") return <IconFolder size={14} />;
  return <IconServer size={14} />;
}

/**
 * 右侧会话标签栏。
 *
 * 参数：见 Props
 * 核心流程：渲染已打开的会话标签（协议图标 + 标题 + 关闭），当前项高亮，
 * 关闭只收起标签不删会话；最右侧固定侧栏折叠按钮，没有标签时也保留，
 * 否则侧栏收起后就没有入口再展开了。
 */
export function TabBar(props: Props) {
  const { tabs, activeId, onSelect, onClose, sidebarVisible, onToggleSidebar } = props;

  return (
    <div className="tabbar">
      <div className="tabbar-tabs">
        {tabs.length === 0 && <span className="tabbar-empty">未打开会话</span>}
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`tab ${activeId === tab.id ? "is-active" : ""}`}
            title={tab.subtitle ? `${tab.title} · ${tab.subtitle}` : tab.title}
            onClick={() => onSelect(tab.id)}
          >
            <span className={`tab-icon protocol-${tab.protocol}`}>{tabIcon(tab.protocol)}</span>
            <span className="tab-title">{tab.title}</span>
            {tab.subtitle && <span className="tab-subtitle">{tab.subtitle}</span>}
            <button
              type="button"
              className="tab-close"
              aria-label={`关闭 ${tab.title}`}
              title={`关闭 ${tab.title}`}
              onClick={(event) => {
                event.stopPropagation();
                onClose(tab.id);
              }}
            >
              <IconClose size={11} />
            </button>
          </div>
        ))}
      </div>
      <button
        type="button"
        className="tabbar-toggle"
        aria-pressed={sidebarVisible}
        aria-label={sidebarVisible ? "隐藏左侧栏" : "显示左侧栏"}
        title={sidebarVisible ? "隐藏左侧栏" : "显示左侧栏"}
        onClick={onToggleSidebar}
      >
        {sidebarVisible ? <IconChevronLeft size={14} /> : <IconChevronRight size={14} />}
      </button>
    </div>
  );
}
