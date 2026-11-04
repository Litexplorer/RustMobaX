import type { ReactNode } from "react";
import {
  IconActivity,
  IconFolder,
  IconGrid,
  IconKey,
  IconServer,
  IconServers,
  IconSplit,
  IconTerminal,
  IconTunnel,
} from "./icons";

/** 工具分页里能打开的弹窗类型，与 App 的 DialogState.kind 对齐 */
export type ToolDialogKind = "vault" | "knownHosts" | "settings" | "tunnel";

type ToolItem = {
  key: string;
  label: string;
  /** 第二行的灰色说明，写清这一项做什么或为什么不可用 */
  hint: string;
  icon: ReactNode;
  /** 未实现的条目一律禁用，不放假按钮 */
  disabled?: boolean;
  onSelect?: () => void;
};

type ToolGroup = { title: string; items: ToolItem[] };

type Props = {
  /** 打开弹窗类操作，直接复用 App 的 setDialog */
  onOpenDialog: (kind: ToolDialogKind) => void;
  /** 切到「文件」分页 */
  onOpenSftp: () => void;
};

/**
 * 工具分页：分组列表。
 *
 * 参数：onOpenDialog 打开弹窗，onOpenSftp 切到文件分页
 * 核心流程：只登记已经实现的能力，未实现的统一放进「未实现」分组并禁用；
 * 每行是「图标 + 名称 + 第二行灰色说明」，整行都可点（按钮铺满行宽），
 * hover 只换底色不换尺寸，避免鼠标扫过时列表抖动。
 */
export function ToolsPanel({ onOpenDialog, onOpenSftp }: Props) {
  const groups: ToolGroup[] = [
    {
      title: "系统",
      items: [
        {
          key: "vault",
          label: "凭据库…",
          hint: "加密保存密码与私钥口令，解锁后可被会话引用",
          icon: <IconKey size={15} />,
          onSelect: () => onOpenDialog("vault"),
        },
        {
          key: "knownHosts",
          label: "已知主机…",
          hint: "查看与删除已确认过的主机指纹",
          icon: <IconServer size={15} />,
          onSelect: () => onOpenDialog("knownHosts"),
        },
        {
          key: "settings",
          label: "首选项…",
          hint: "主题、界面缩放、终端字体与侧栏",
          icon: <IconGrid size={15} />,
          onSelect: () => onOpenDialog("settings"),
        },
      ],
    },
    {
      title: "网络",
      items: [
        {
          key: "tunnel",
          label: "端口转发（隧道）…",
          hint: "本地与远程端口转发，可随时启停",
          icon: <IconTunnel size={15} />,
          onSelect: () => onOpenDialog("tunnel"),
        },
        {
          key: "sftp",
          label: "打开文件面板",
          hint: "切到「文件」分页，浏览当前会话的远端目录",
          icon: <IconFolder size={15} />,
          onSelect: onOpenSftp,
        },
      ],
    },
    {
      title: "编辑器",
      items: [
        {
          key: "editor",
          label: "内置文本编辑器",
          hint: "在「文件」分页双击文件即可打开，这里只做入口",
          icon: <IconTerminal size={15} />,
          onSelect: onOpenSftp,
        },
      ],
    },
    {
      title: "未实现",
      items: [
        {
          key: "diff",
          label: "MobaDiff",
          hint: "尚未实现",
          icon: <IconSplit size={15} />,
          disabled: true,
        },
        {
          key: "ascii",
          label: "ASCII 表",
          hint: "尚未实现",
          icon: <IconGrid size={15} />,
          disabled: true,
        },
        {
          key: "scan",
          label: "端口扫描",
          hint: "尚未实现",
          icon: <IconActivity size={15} />,
          disabled: true,
        },
        {
          key: "wol",
          label: "网络唤醒",
          hint: "尚未实现",
          icon: <IconServers size={15} />,
          disabled: true,
        },
      ],
    },
  ];

  return (
    <div className="sidebar-page-scroll">
      {groups.map((group) => (
        <section key={group.title} className="tool-group">
          <h4 className="tool-group-title">{group.title}</h4>
          {group.items.map((item) => (
            <button
              key={item.key}
              type="button"
              className="tool-row"
              disabled={item.disabled}
              onClick={item.onSelect}
              title={item.disabled ? `${item.label}：${item.hint}` : item.hint}
            >
              <span className="tool-row-icon">{item.icon}</span>
              <span className="tool-row-text">
                <span className="tool-row-label">{item.label}</span>
                <span className="tool-row-hint">{item.hint}</span>
              </span>
            </button>
          ))}
        </section>
      ))}
    </div>
  );
}
