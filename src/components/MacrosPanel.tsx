import { IconMacros } from "./icons";

/**
 * 宏分页：只有空态说明。
 *
 * 无参数
 * 核心流程：录制与回放都还没实现，这里只给一句说明加一个禁用的按钮；
 * 不伪造宏列表，避免用户以为已经有录制好的宏可以运行。
 */
export function MacrosPanel() {
  return (
    <div className="sidebar-page-scroll">
      <div className="sidebar-empty">
        <span className="sidebar-empty-icon">
          <IconMacros size={26} />
        </span>
        <p className="sidebar-empty-title">宏录制</p>
        <p className="sidebar-empty-text">录制与回放尚未实现，当前版本不支持。</p>
        <button
          type="button"
          className="sidebar-empty-action"
          disabled
          title="宏录制尚未实现，按钮仅为占位"
        >
          开始录制
        </button>
      </div>
    </div>
  );
}
