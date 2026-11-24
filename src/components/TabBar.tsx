import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Protocol } from "../api/session";
import { IconChevronLeft, IconChevronRight, IconClose, IconFolder, IconServer, IconTerminal } from "./icons";

export type TabItem = {
  /** 标签实例 id：同一会话可以开多个标签，每个各有自己的实例 id */
  id: string;
  /** 标签指向的会话 id：找会话、登记内存态都用它 */
  sessionId: string;
  title: string;
  subtitle: string;
  protocol: Protocol;
};

type Props = {
  tabs: TabItem[];
  /** 当前活动的标签实例 id */
  activeId: string | null;
  /** 参数为标签实例 id */
  onSelect: (id: string) => void;
  /** 参数为标签实例 id */
  onClose: (id: string) => void;
  /** 参数为标签实例 id：把该实例指向的会话再开一个实例 */
  onDuplicate: (id: string) => void;
  /** 参数为标签实例 id：保留该实例，关掉其余标签 */
  onCloseOthers: (id: string) => void;
  /** 关闭全部标签 */
  onCloseAll: () => void;
  /** 侧栏是否展开，决定右侧按钮显示 ◀ 还是 ▶ */
  sidebarVisible: boolean;
  onToggleSidebar: () => void;
};

/** 右键菜单的目标标签与落点：x / y 是右键时的视口坐标 */
type TabMenuState = { tabId: string; x: number; y: number };

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
 * 右键标签弹自定义菜单并拦掉 WebView 默认菜单：菜单用 fixed 定位跟随落点，
 * 渲染后按视口尺寸翻转，避免贴着右下角时溢出到窗口外。
 */
export function TabBar(props: Props) {
  const {
    tabs,
    activeId,
    onSelect,
    onClose,
    onDuplicate,
    onCloseOthers,
    onCloseAll,
    sidebarVisible,
    onToggleSidebar,
  } = props;
  /** 当前打开的右键菜单；null 表示没有菜单 */
  const [menu, setMenu] = useState<TabMenuState | null>(null);
  /** 菜单修正后的左上角坐标：先按落点渲染，再按实际尺寸夹进视口 */
  const [menuPos, setMenuPos] = useState<{ left: number; top: number } | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // 菜单渲染出来后才能量到真实宽高：越过视口右/下边就整体翻到落点左侧/上方，
  // 翻转后仍然贴边的再夹回 4px，保证任何窗口尺寸下都完整可见
  useLayoutEffect(() => {
    const element = menuRef.current;
    if (!menu || !element) return;
    const rect = element.getBoundingClientRect();
    const edge = 4;
    const maxLeft = window.innerWidth - rect.width - edge;
    const maxTop = window.innerHeight - rect.height - edge;
    const left = menu.x > maxLeft ? Math.max(edge, menu.x - rect.width) : menu.x;
    const top = menu.y > maxTop ? Math.max(edge, menu.y - rect.height) : menu.y;
    setMenuPos({ left, top });
  }, [menu]);

  // 关菜单的时机：点击菜单外部、Esc、窗口失焦、滚动。
  // pointerdown 只负责「判断是否点在菜单里」，菜单项自身仍用 onClick 触发：
  // 若用 mousedown 关菜单，点击会先把菜单卸掉，click 就落空了
  useEffect(() => {
    if (!menu) return;
    const handlePointerDown = (event: PointerEvent) => {
      const element = menuRef.current;
      if (element && event.target instanceof Node && element.contains(event.target)) return;
      setMenu(null);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenu(null);
    };
    const handleClose = () => setMenu(null);
    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("blur", handleClose);
    // 捕获阶段监听：标签栏自己横向滚动时也要收掉菜单
    window.addEventListener("scroll", handleClose, true);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("blur", handleClose);
      window.removeEventListener("scroll", handleClose, true);
    };
  }, [menu]);

  /** 执行菜单项：先收菜单再干活，避免动作里改标签后菜单还挂在旧坐标上 */
  const runMenuAction = (action: () => void) => {
    setMenu(null);
    setMenuPos(null);
    action();
  };

  return (
    <>
      <div className="tabbar">
        <div className="tabbar-tabs">
          {tabs.length === 0 && <span className="tabbar-empty">未打开会话</span>}
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`tab ${activeId === tab.id ? "is-active" : ""}`}
              title={tab.subtitle ? `${tab.title} · ${tab.subtitle}` : tab.title}
              onClick={() => onSelect(tab.id)}
              onContextMenu={(event) => {
                // 拦掉 WebView 默认菜单，只弹下面这一套
                event.preventDefault();
                setMenu({ tabId: tab.id, x: event.clientX, y: event.clientY });
                setMenuPos(null);
              }}
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
      {menu && (
        <div
          className="tab-menu"
          ref={menuRef}
          style={{ left: menuPos?.left ?? menu.x, top: menuPos?.top ?? menu.y }}
          // 菜单自己也不给默认右键菜单留口子
          onContextMenu={(event) => event.preventDefault()}
        >
          <button
            type="button"
            className="tab-menu-item"
            onClick={() => runMenuAction(() => onDuplicate(menu.tabId))}
          >
            复制标签
          </button>
          <button
            type="button"
            className="tab-menu-item"
            onClick={() => runMenuAction(() => onClose(menu.tabId))}
          >
            关闭
          </button>
          <button
            type="button"
            className="tab-menu-item"
            disabled={tabs.length <= 1}
            onClick={() => runMenuAction(() => onCloseOthers(menu.tabId))}
          >
            关闭其他
          </button>
          <button
            type="button"
            className="tab-menu-item"
            disabled={tabs.length <= 1}
            onClick={() => runMenuAction(() => onCloseAll())}
          >
            全部关闭
          </button>
        </div>
      )}
    </>
  );
}
