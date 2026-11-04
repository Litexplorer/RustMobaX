/**
 * 图标系统：接近 MobaXterm 视觉语言的彩色小字形。
 *
 * 统一规格：24 网格、1.8 描边、圆头圆角、不用渐变/阴影/滤镜；
 * 关键图标内置固定 5 色调色板，功能性小图标保持单色线框。
 */

/** 调色板：固定色值，不跟随 currentColor，保证跨主题一致 */
const P = {
  blue: "#4a9eff",
  green: "#4ac26b",
  amber: "#e8c07a",
  violet: "#a78bfa",
  neutral: "#c3c9d4",
} as const;

/** color 为彩色字形，mono 全部退回 currentColor（供密集列表复用） */
export type IconTone = "color" | "mono";

export type IconProps = {
  /** 边长，沿用调用点现有取值，不改变布局槽大小 */
  size?: number;
  /** 渲染模式，默认 color */
  tone?: IconTone;
  /** 附加类名 */
  className?: string;
};

/**
 * 取色器。
 *
 * 参数：tone 为渲染模式
 * 核心流程：mono 模式一律返回 currentColor，彩色模式返回调色板固定色值。
 */
function ink(tone: IconTone | undefined) {
  return (color: string) => (tone === "mono" ? "currentColor" : color);
}

/**
 * 统一图标外壳。
 *
 * 参数：size 边长，className 附加类名，children 图形内容
 * 核心流程：固定 24 网格与 1.8 描边，圆头圆角，避免每个图标各写一套属性；
 * 不使用渐变、阴影、滤镜，保证 Tauri WebView 下渲染开销低。
 */
function Icon({ size = 16, className, children }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}

/** 新增：会话（三层叠放的终端窗口，蓝 + 中性） */
export function IconSessions(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <rect x="2.6" y="10.8" width="12" height="8.2" rx="1.6" stroke={c(P.blue)} />
      <path d="m5.2 13 1.7 1.7-1.7 1.7" stroke={c(P.neutral)} />
      <path d="M9.4 16.4h2.6" stroke={c(P.neutral)} />
      <path d="M9.4 8.2h8.4a1.6 1.6 0 0 1 1.6 1.6v2.2" stroke={c(P.neutral)} />
      <path d="M12.2 5.4h7.2a1.6 1.6 0 0 1 1.6 1.6v2.2" stroke={c(P.neutral)} />
    </Icon>
  );
}

/** 新增：服务器（两台显示器并排，紫） */
export function IconServers(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <rect x="2.8" y="6.4" width="8.4" height="7" rx="1.6" stroke={c(P.violet)} />
      <rect x="12.8" y="6.4" width="8.4" height="7" rx="1.6" stroke={c(P.violet)} />
      <path d="M7 13.4v2.8" stroke={c(P.neutral)} />
      <path d="M17 13.4v2.8" stroke={c(P.neutral)} />
      <path d="M4.8 16.2h4.4" stroke={c(P.neutral)} />
      <path d="M14.8 16.2h4.4" stroke={c(P.neutral)} />
    </Icon>
  );
}

/** 新增：工具（扳手 + 螺丝刀交叉，紫 / 中性） */
export function IconTools(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <path d="M17 3.2a3.9 3.9 0 1 1-3.6 3.6" stroke={c(P.violet)} />
      <path d="M14.5 9.9 5.3 19.1" stroke={c(P.violet)} />
      <path d="M9.6 8.2 8.2 9.6 3.2 4.6 4.6 3.2z" stroke={c(P.neutral)} />
      <path d="M9.4 9.4 19.4 19.4" stroke={c(P.neutral)} />
    </Icon>
  );
}

/** 新增：宏（两个顺时针循环箭头，绿） */
export function IconMacros(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <path d="M6.4 9.4a5.6 5.6 0 0 1 11.2 0" stroke={c(P.green)} />
      <path d="M15 6.8 17.6 9.4 20.2 6.8" stroke={c(P.green)} />
      <path d="M17.6 14.6a5.6 5.6 0 0 1-11.2 0" stroke={c(P.green)} />
      <path d="M3.8 17.2 6.4 14.6 9 17.2" stroke={c(P.green)} />
    </Icon>
  );
}

/** 新增：多执行（2x2 终端格，蓝） */
export function IconMultiExec(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <rect x="3.2" y="3.2" width="7.6" height="7.6" rx="1.5" stroke={c(P.blue)} />
      <rect x="13.2" y="3.2" width="7.6" height="7.6" rx="1.5" stroke={c(P.blue)} />
      <rect x="3.2" y="13.2" width="7.6" height="7.6" rx="1.5" stroke={c(P.blue)} />
      <rect x="13.2" y="13.2" width="7.6" height="7.6" rx="1.5" stroke={c(P.blue)} />
      <path d="m15.8 16.3 1.4 1.4-1.4 1.4" stroke={c(P.neutral)} />
    </Icon>
  );
}

/** 隧道（管道 + 穿过的双向箭头，蓝） */
export function IconTunnel(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <path d="M3.6 5.8h16.8" stroke={c(P.neutral)} />
      <path d="M3.6 18.2h16.8" stroke={c(P.neutral)} />
      <path d="M8 12h8" stroke={c(P.blue)} />
      <path d="M10.4 9.6 12.8 12l-2.4 2.4" stroke={c(P.blue)} />
      <path d="M13.6 9.6 16 12l-2.4 2.4" stroke={c(P.blue)} />
    </Icon>
  );
}

/** 分屏（竖分栏两块面板，蓝 + 中性） */
export function IconSplit(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <rect x="3" y="4.6" width="8.2" height="14.8" rx="1.8" stroke={c(P.blue)} />
      <rect x="12.8" y="4.6" width="8.2" height="14.8" rx="1.8" stroke={c(P.neutral)} />
    </Icon>
  );
}

/** 凭据库（钥匙，琥珀） */
export function IconKey(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <circle cx="7.2" cy="16.8" r="3.2" stroke={c(P.amber)} />
      <path d="M9.5 14.5 19 5" stroke={c(P.amber)} />
      <path d="M15.4 8.7 17.6 10.9" stroke={c(P.amber)} />
      <path d="M17.5 6.6 19.7 8.8" stroke={c(P.amber)} />
    </Icon>
  );
}

/** 本地终端（显示器 + 提示符 >_，蓝） */
export function IconTerminal(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <rect x="2.6" y="4.8" width="18.8" height="13.2" rx="2" stroke={c(P.blue)} />
      <path d="m7 10.2 2.2 2.2-2.2 2.2" stroke={c(P.neutral)} />
      <path d="M11.6 14.6h3.2" stroke={c(P.neutral)} />
    </Icon>
  );
}

/**
 * SSH（左侧显示器 + 右侧网络插头，蓝 + 紫）
 *
 * 与 IconTerminal 靠轮廓区分：本图标非居中对称（屏幕偏左、右侧挂插头），
 * 缩小到 12px 也能看出「远程」的连线，而不是只靠一个紫点。
 */
export function IconServer(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <rect x="1.8" y="4.8" width="11.6" height="10" rx="1.8" stroke={c(P.blue)} />
      <path d="M13.4 9.8h2" stroke={c(P.neutral)} />
      <rect x="15.4" y="7.4" width="6.2" height="4.8" rx="1.2" stroke={c(P.violet)} />
      <path d="M17.6 7.4v4.8" stroke={c(P.violet)} />
    </Icon>
  );
}

/** SFTP / 分组（文件夹，琥珀） */
export function IconFolder(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <path
        d="M3 7.6a2.2 2.2 0 0 1 2.2-2.2h3.3l2.1 2.5h8.2A2.2 2.2 0 0 1 21 10v6.4a2.2 2.2 0 0 1-2.2 2.2H5.2A2.2 2.2 0 0 1 3 16.4z"
        stroke={c(P.amber)}
      />
    </Icon>
  );
}

/** 全部（九宫格，中性） */
export function IconGrid(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <rect x="3.6" y="3.6" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="9.7" y="3.6" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="15.8" y="3.6" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="3.6" y="9.7" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="9.7" y="9.7" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="15.8" y="9.7" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="3.6" y="15.8" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="9.7" y="15.8" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
      <rect x="15.8" y="15.8" width="4.6" height="4.6" rx="1.2" stroke={c(P.neutral)} />
    </Icon>
  );
}

/** 导入（箭头进入托盘，蓝） */
export function IconImport(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <path d="M12 4v8.2" stroke={c(P.blue)} />
      <path d="M8.4 8.6 12 12.2 15.6 8.6" stroke={c(P.blue)} />
      <path
        d="M3.6 15v3A2.4 2.4 0 0 0 6 20.4h12A2.4 2.4 0 0 0 20.4 18v-3"
        stroke={c(P.neutral)}
      />
    </Icon>
  );
}

/** 导出（箭头离开托盘，绿） */
export function IconExport(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <path d="M12 12.2V4" stroke={c(P.green)} />
      <path d="M8.4 7.4 12 3.8 15.6 7.4" stroke={c(P.green)} />
      <path
        d="M3.6 15v3A2.4 2.4 0 0 0 6 20.4h12A2.4 2.4 0 0 0 20.4 18v-3"
        stroke={c(P.neutral)}
      />
    </Icon>
  );
}

/** 信息（圆形 i，中性） */
export function IconInfo(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <circle cx="12" cy="12" r="8.4" stroke={c(P.neutral)} />
      <path d="M12 11.2v5.2" stroke={c(P.neutral)} />
      <path d="M12 7.9v.2" stroke={c(P.neutral)} />
    </Icon>
  );
}

/** 远程监控（心电图，绿） */
export function IconActivity(props: IconProps) {
  const c = ink(props.tone);
  return (
    <Icon {...props}>
      <path d="M2.8 12.2h3.4l2.2-5 3.3 9.8 2.2-4.8h7.3" stroke={c(P.green)} />
    </Icon>
  );
}

/** 新建（单色线框） */
export function IconPlus(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 4.6v14.8" />
      <path d="M4.6 12h14.8" />
    </Icon>
  );
}

/** 关闭（单色线框） */
export function IconClose(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m6.4 6.4 11.2 11.2" />
      <path d="m17.6 6.4-11.2 11.2" />
    </Icon>
  );
}

/** 搜索（单色线框） */
export function IconSearch(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="11" cy="11" r="6.6" />
      <path d="m16 16 4.6 4.6" />
    </Icon>
  );
}

/** 勾选（单色线框） */
export function IconCheck(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m4.8 12.6 4.6 4.6 9.8-9.8" />
    </Icon>
  );
}

/**
 * 全部展开（单色线框）。
 *
 * 参数：沿用 IconProps（size/tone/className）
 * 核心流程：只留一个大方向箭头（向下）+ 一条底部基线，2.2 描边；
 * 15px 下也不会糊成装饰符号，且基线在下与「折叠」互为正反，不会被读成关闭。
 */
export function IconExpandAll(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 4.4v11.6" strokeWidth={2.2} />
      <path d="m6.6 10.8 5.4 5.4 5.4-5.4" strokeWidth={2.2} />
      <path d="M4.4 20.4h15.2" strokeWidth={2.2} />
    </Icon>
  );
}

/**
 * 全部折叠（单色线框）。
 *
 * 参数：沿用 IconProps（size/tone/className）
 * 核心流程：与展开完全镜像，箭头向上、基线在上，同样只有一笔箭头 + 一条线。
 */
export function IconCollapseAll(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 19.6V8" strokeWidth={2.2} />
      <path d="m6.6 13.2 5.4-5.4 5.4 5.4" strokeWidth={2.2} />
      <path d="M4.4 3.6h15.2" strokeWidth={2.2} />
    </Icon>
  );
}

/** 左箭头（单色线框） */
export function IconChevronLeft(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m14.6 5.6-6.4 6.4 6.4 6.4" />
    </Icon>
  );
}

/** 右箭头（单色线框） */
export function IconChevronRight(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m9.4 5.6 6.4 6.4-6.4 6.4" />
    </Icon>
  );
}

/** 下箭头（单色线框） */
export function IconChevronDown(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m5.6 9.4 6.4 6.4 6.4-6.4" />
    </Icon>
  );
}
