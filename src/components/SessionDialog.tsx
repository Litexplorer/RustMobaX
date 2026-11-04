import { useEffect, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type { Protocol, Session, SessionGroup, SessionInput } from "../api/session";
import type { Credential } from "../api/vault";
import type { SerialPortInfo } from "../api/terminal";
import { serialListPorts, wslListDistros } from "../api/terminal";
import { SERIAL_BAUD_RATES, SERIAL_DEFAULTS, SERIAL_MAX_BAUD_RATE } from "../api/terminal";
import { flattenGroups } from "../lib/tree";
import { useEscapeClose } from "../lib/useEscapeClose";
import { isRemoteDesktop, isSshLike } from "../lib/protocols";
import {
  IconActivity,
  IconClose,
  IconFolder,
  IconGrid,
  IconMultiExec,
  IconServer,
  IconSplit,
  IconTerminal,
} from "./icons";

type Props = {
  mode: "create" | "edit";
  initial: Session | null;
  groups: SessionGroup[];
  credentials: Credential[];
  defaultGroupId: string | null;
  draft?: Partial<SessionInput> | null;
  busy: boolean;
  onSubmit: (input: SessionInput) => void;
  onCancel: () => void;
};

/**
 * 会话的协议私有配置。
 *
 * 无参数
 * 核心流程：整体序列化进 session.extra，避免为每个高级项单独加表列。
 */
type SshExtra = {
  privateKeyPath?: string;
  /** 私钥是否带口令 -> 只存布尔，口令本身绝不落库 */
  keyHasPassphrase?: boolean;
  credentialId?: string;
  /**
   * 私钥口令对应的凭据标识：只是引用，不含明文。
   * 前端没有读凭据明文的命令，目前仅做保留，编辑会话时不能被清掉。
   */
  passphraseCredentialId?: string;
  x11Forwarding?: boolean;
  executeCommand?: string;
  /**
   * WSL 发行版名；空串或缺省表示用 WSL 默认发行版。
   * WSL 是本地子系统，没有主机与端口，连接信息只靠这一个字段。
   */
  wslDistro?: string;
  /**
   * 串口设备路径（例如 /dev/ttyUSB0）；串口没有主机与端口。
   */
  serialPort?: string;
  /** 波特率，缺省 9600 */
  serialBaud?: number;
  /** 数据位：5 / 6 / 7 / 8，缺省 8 */
  serialDataBits?: number;
  /** 停止位：1 / 1.5 / 2，缺省 1（1.5 当前平台不支持，后端会明确报错） */
  serialStopBits?: number;
  /** 校验：none / even / odd，缺省 none */
  serialParity?: string;
  /** 流控：none / software / hardware，缺省 none */
  serialFlow?: string;
};

/**
 * 读取 extra 字段。
 *
 * 参数：raw 为库里的 JSON 字符串
 * 核心流程：解析失败或不是对象时退回空配置，不让脏数据阻塞编辑。
 */
function parseExtra(raw: string | null): SshExtra {
  if (!raw) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed === "object" && parsed !== null) {
      return parsed as SshExtra;
    }
    return {};
  } catch {
    return {};
  }
}

/**
 * 序列化 extra 字段。
 *
 * 参数：extra 为当前表单里的高级项
 * 核心流程：丢掉空值，全部为空时返回 null，避免库里出现空对象字符串。
 */
function buildExtra(extra: SshExtra): string | null {
  const compact: SshExtra = {};
  const key = extra.privateKeyPath?.trim();
  if (key) {
    compact.privateKeyPath = key;
    // 没有私钥路径时这个标记没有意义，一并丢弃
    if (extra.keyHasPassphrase) compact.keyHasPassphrase = true;
    // 口令凭据引用只跟着「私钥 + 有口令」存在，避免留下指向不明身份
    const passphraseCredential = extra.passphraseCredentialId?.trim();
    if (extra.keyHasPassphrase && passphraseCredential) {
      compact.passphraseCredentialId = passphraseCredential;
    }
  }
  const credential = extra.credentialId?.trim();
  if (credential) compact.credentialId = credential;
  if (extra.x11Forwarding) compact.x11Forwarding = true;
  const command = extra.executeCommand?.trim();
  if (command) compact.executeCommand = command;
  // 发行版只在选了具体发行版时才落库 -> 「默认发行版」不写这个键
  const distro = extra.wslDistro?.trim();
  if (distro) compact.wslDistro = distro;
  // 设备路径只在选了串口协议时才落库
  const serialPort = extra.serialPort?.trim();
  if (serialPort) compact.serialPort = serialPort;
  // 线路参数跟着设备路径一起落库；波特率为 0 表示「不是串口会话」，整组丢掉
  if (extra.serialBaud && extra.serialBaud > 0) {
    compact.serialBaud = extra.serialBaud;
    if (extra.serialDataBits) compact.serialDataBits = extra.serialDataBits;
    if (extra.serialStopBits) compact.serialStopBits = extra.serialStopBits;
    if (extra.serialParity) compact.serialParity = extra.serialParity;
    if (extra.serialFlow) compact.serialFlow = extra.serialFlow;
  }
  return Object.keys(compact).length > 0 ? JSON.stringify(compact) : null;
}

const PROTOCOL_OPTIONS: { value: Protocol; label: string; icon: React.ReactNode }[] = [
  { value: "ssh", label: "SSH", icon: <IconServer size={20} /> },
  { value: "sftp", label: "SFTP", icon: <IconFolder size={20} /> },
  { value: "telnet", label: "Telnet", icon: <IconTerminal size={20} /> },
  { value: "local", label: "Shell", icon: <IconTerminal size={20} /> },
  // WSL 是「本机里的 Linux」，用多终端格图标与单窗口的 Shell 区分
  { value: "wsl", label: "WSL", icon: <IconMultiExec size={20} /> },
  // 串口是硬件线路上的字节流，用信号波形图标与网络类协议区分
  { value: "serial", label: "串口", icon: <IconActivity size={20} /> },
  // RDP / VNC 本期不自研渲染 -> 只登记会话，连接时交给系统客户端。
  // 图标复用现成的：桌面用网格、屏幕共享用双屏。
  { value: "rdp", label: "RDP", icon: <IconGrid size={20} /> },
  { value: "vnc", label: "VNC", icon: <IconSplit size={20} /> },
];

/** 各协议在「基本设置」标题里显示的名字。 */
const PROTOCOL_LABEL: Record<Protocol, string> = {
  ssh: "SSH",
  sftp: "SFTP",
  telnet: "Telnet",
  local: "本地",
  rdp: "RDP",
  vnc: "VNC",
  serial: "串口",
  wsl: "WSL",
};

/** 串口设备的粗分类标签，取不到分类（空串）时只显示「串口」。 */
const SERIAL_KIND_LABEL: Record<string, string> = {
  usb: "USB 转串口",
  pci: "板载串口",
  bluetooth: "蓝牙串口",
};

/** 各协议的默认端口，与核心层 `effective_port()` 保持一致。 */
const DEFAULT_PORT_BY_PROTOCOL: Partial<Record<Protocol, number>> = {
  ssh: 22,
  sftp: 22,
  telnet: 23,
  rdp: 3389,
  vnc: 5900,
};

/** 取协议的默认端口，没有约定的一律按 22 兜底。 */
const defaultPortFor = (protocol: Protocol | null): number =>
  (protocol && DEFAULT_PORT_BY_PROTOCOL[protocol]) || 22;

/** 串口数据位可选值，与核心层的映射一致。 */
const SERIAL_DATA_BIT_OPTIONS = [5, 6, 7, 8];
/** 串口停止位可选值：1.5 保留在列表里，选中后提示当前平台不支持。 */
const SERIAL_STOP_BIT_OPTIONS = [1, 1.5, 2];
/** 校验方式，value 直接落库，label 给人看。 */
const SERIAL_PARITY_OPTIONS = [
  { value: "none", label: "无 (none)" },
  { value: "even", label: "偶 (even)" },
  { value: "odd", label: "奇 (odd)" },
];
/** 流控方式，value 直接落库。 */
const SERIAL_FLOW_OPTIONS = [
  { value: "none", label: "无 (none)" },
  { value: "software", label: "软件 (XON/XOFF)" },
  { value: "hardware", label: "硬件 (RTS/CTS)" },
];
/** 平台没有 1.5 停止位对应的底层枚举 -> 与后端 `parse_stop_bits` 的文案一致。 */
const STOP_BITS_UNSUPPORTED_HINT = "当前平台不支持停止位 1.5";

/**
 * 校验手工填写的波特率。
 *
 * 参数：raw 为输入框里的原始文本
 * 核心流程：必须是纯数字的正整数且不超过上限（与核心层 `MAX_BAUD_RATE` 对齐）；
 * 不合格就回一条错误文案，合格回 null。放在提交前挡一层，减少一次无效连接。
 */
function baudRateErrorOf(raw: string): string | null {
  const text = raw.trim();
  if (text === "") return "波特率不能为空";
  if (!/^\d+$/.test(text)) return "波特率必须是正整数";
  const value = Number(text);
  if (value < 1 || value > SERIAL_MAX_BAUD_RATE) {
    return `波特率需在 1 到 ${SERIAL_MAX_BAUD_RATE} 之间`;
  }
  return null;
}

/**
 * 新建或编辑会话的弹窗。
 *
 * 参数：见 Props
 * 核心流程：先选协议类型 → 按类型展示基本与高级设置 → 提交前做必填校验；
 * 后端仍会再校验一次，这里只把明显错误挡在提交前。
 */
export function SessionDialog(props: Props) {
  const { mode, initial, groups, credentials, defaultGroupId, draft, busy, onSubmit, onCancel } =
    props;
  // Esc = 关闭（等价于点「取消」/「×」）；保存中不生效，避免把正在提交的请求晾在半路
  useEscapeClose(onCancel, !busy);

  const initialExtra = parseExtra(initial?.extra ?? null);
  const [protocol, setProtocol] = useState<Protocol | null>(
    initial?.protocol ?? (draft?.protocol as Protocol | undefined) ?? null,
  );
  const [name, setName] = useState(initial?.name ?? draft?.name ?? "");
  const [host, setHost] = useState(initial?.host ?? draft?.host ?? "");
  const [port, setPort] = useState<number>(initial?.port ?? draft?.port ?? 22);
  const [specifyUsername, setSpecifyUsername] = useState(
    initial ? initial.username !== null : Boolean(draft?.username),
  );
  const [username, setUsername] = useState(initial?.username ?? draft?.username ?? "");
  const [groupId, setGroupId] = useState<string | null>(
    initial?.groupId ?? draft?.groupId ?? defaultGroupId,
  );
  const [privateKeyPath, setPrivateKeyPath] = useState(initialExtra.privateKeyPath ?? "");
  const [keyHasPassphrase, setKeyHasPassphrase] = useState(initialExtra.keyHasPassphrase === true);
  const [credentialId, setCredentialId] = useState(initialExtra.credentialId ?? "");
  const [x11Forwarding, setX11Forwarding] = useState(initialExtra.x11Forwarding === true);
  const [executeCommand, setExecuteCommand] = useState(initialExtra.executeCommand ?? "");
  const [wslDistro, setWslDistro] = useState(initialExtra.wslDistro ?? "");
  /** 已安装的 WSL 发行版；null 表示还没查回来（加载中） */
  const [wslDistros, setWslDistros] = useState<string[] | null>(null);
  const [serialPort, setSerialPort] = useState(initialExtra.serialPort ?? "");
  /**
   * 串口线路参数。
   *
   * 老会话的 extra 里没有这几个键 -> 一律回落到 9600-8-N-1 无流控，
   * 不会因为缺键就报错。波特率这里存字符串：用户可能手填，需要把
   * 「空串 / 非数字」这类中间状态原样留住，提交前再校验。
   */
  const [serialBaud, setSerialBaud] = useState(
    String(initialExtra.serialBaud ?? SERIAL_DEFAULTS.baudRate),
  );
  const [serialDataBits, setSerialDataBits] = useState<number>(
    initialExtra.serialDataBits ?? SERIAL_DEFAULTS.dataBits,
  );
  const [serialStopBits, setSerialStopBits] = useState<number>(
    initialExtra.serialStopBits ?? SERIAL_DEFAULTS.stopBits,
  );
  const [serialParity, setSerialParity] = useState(
    initialExtra.serialParity ?? SERIAL_DEFAULTS.parity,
  );
  const [serialFlow, setSerialFlow] = useState(initialExtra.serialFlow ?? SERIAL_DEFAULTS.flowControl);
  /** 本机串口设备；null 表示还没查回来（加载中） */
  const [serialPorts, setSerialPorts] = useState<SerialPortInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pickingKey, setPickingKey] = useState(false);
  /** 校验失败时用来把焦点与视口挪到出错字段 */
  const protocolRef = useRef<HTMLDivElement | null>(null);
  const hostRef = useRef<HTMLInputElement | null>(null);
  const portRef = useRef<HTMLInputElement | null>(null);
  const baudRef = useRef<HTMLInputElement | null>(null);
  const serialPortRef = useRef<HTMLInputElement | null>(null);
  const nameRef = useRef<HTMLInputElement | null>(null);

  const groupOptions = flattenGroups(groups);
  // 连远端才要主机与端口：SSH 家族与远程桌面（后者只是把连接交给系统客户端）
  // 都算，外加网络型的 Telnet；本地 / WSL / 串口不需要。
  const needsHost =
    protocol !== null &&
    (isSshLike(protocol) || isRemoteDesktop(protocol) || protocol === "telnet");
  /** 串口波特率的手填校验结果；非串口协议不参与判断 */
  const baudRateError = protocol === "serial" ? baudRateErrorOf(serialBaud) : null;
  /**
   * 串口设备路径必填。
   *
   * 只判断「填没填」，不判断「此刻存不存在」：PTY、软链这类路径后端本来就枚举不到，
   * 设备也可能稍后才插上，提前报「不存在」会把本来能用的会话挡在门外。
   */
  const serialPortError =
    protocol === "serial" && serialPort.trim() === "" ? "请填写串口设备路径" : null;
  /** 1.5 停止位当前平台没有对应枚举 -> 选中后给出与后端一致的提示 */
  const stopBitsUnsupported = protocol === "serial" && serialStopBits === 1.5;

  // 选中 WSL 时才去枚举发行版 -> 其它协议不必为一次外部进程付出开销。
  // 失败一律当作「没装」处理：后端对「未安装」也是回空列表而不是报错，
  // 这里再兜一层，避免把引导文案变成一条红色错误。
  useEffect(() => {
    if (protocol !== "wsl" || wslDistros !== null) return undefined;
    let cancelled = false;
    void wslListDistros()
      .then((list) => {
        if (!cancelled) setWslDistros(list);
      })
      .catch(() => {
        if (!cancelled) setWslDistros([]);
      });
    return () => {
      cancelled = true;
    };
  }, [protocol, wslDistros]);

  // 选中串口时才枚举本机设备 -> 其它协议不必为一次系统调用付出开销。
  // 失败一律当作「没有设备」：后端对「查不到」也是回空列表而不是报错。
  useEffect(() => {
    if (protocol !== "serial" || serialPorts !== null) return undefined;
    let cancelled = false;
    void serialListPorts()
      .then((list) => {
        if (!cancelled) setSerialPorts(list);
      })
      .catch(() => {
        if (!cancelled) setSerialPorts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [protocol, serialPorts]);

  /**
   * 切换协议类型。
   *
   * 参数：next 为目标协议
   * 核心流程：切换后清掉上一次的校验提示，避免旧错误误导当前选择。
   */
  const selectProtocol = (next: Protocol) => {
    // 端口跟着协议走 -> 只有当用户没改过当前协议的默认值时才替换，
    // 免得把他手填的端口冲掉
    setPort((current) =>
      current === defaultPortFor(protocol) ? defaultPortFor(next) : current,
    );
    setProtocol(next);
    setError(null);
  };

  /**
   * 选择私钥文件。
   *
   * 参数：无
   * 核心流程：调系统文件对话框取绝对路径；无桌面集成（如 xrdp 无 portal）时
   * 对话框可能一直不返回，这里用超时兜底提示用户改为直接粘贴路径。
   */
  const pickPrivateKey = async () => {
    setPickingKey(true);
    setError(null);
    const timer = window.setTimeout(() => {
      setError("文件选择器未响应，可直接在输入框粘贴私钥路径");
      setPickingKey(false);
    }, 5000);
    try {
      const picked = await openDialog({ multiple: false, directory: false });
      window.clearTimeout(timer);
      if (typeof picked === "string") {
        setPrivateKeyPath(picked);
        setError(null);
      }
    } catch (err) {
      window.clearTimeout(timer);
      setError(`无法打开文件选择器：${String(err)}，可直接粘贴路径`);
    } finally {
      setPickingKey(false);
    }
  };

  /**
   * 把焦点和视口移到出错的字段。
   *
   * 参数：target 为出错字段的节点
   * 核心流程：先聚焦（阻止浏览器自己滚动）再居中滚动，
   * 因为字段在弹窗自己的滚动容器里，不居中用户看不到红字提示。
   */
  const focusField = (target: HTMLElement | null) => {
    if (!target) return;
    target.focus({ preventScroll: true });
    target.scrollIntoView({ block: "center" });
  };

  /**
   * 提交表单。
   *
   * 参数：无
   * 核心流程：未选协议 -> 名称 -> 主机 -> 端口 -> 串口设备 -> 串口波特率 依次校验，
   * 失败时定位到对应字段；通过后拼装 SessionInput，认证方式由是否填了私钥推导。
   */
  const submit = () => {
    if (protocol === null) {
      setError("请先选择会话类型");
      focusField(protocolRef.current?.querySelector<HTMLElement>("button") ?? protocolRef.current);
      return;
    }
    if (name.trim() === "") {
      setError("会话名称不能为空");
      focusField(nameRef.current);
      return;
    }
    if (needsHost && host.trim() === "") {
      setError("该协议必须填写主机");
      focusField(hostRef.current);
      return;
    }
    if (needsHost && (!Number.isInteger(port) || port < 1 || port > 65535)) {
      setError("端口需为 1 到 65535 之间的整数");
      focusField(portRef.current);
      return;
    }
    // 设备路径是串口会话的必填项 -> 空路径到后端也开不了端口，就地拦下
    if (serialPortError !== null) {
      setError(serialPortError);
      focusField(serialPortRef.current);
      return;
    }
    // 串口波特率可以手填 -> 非正整数/超范围在提交前拦下，并定位到该字段
    if (baudRateError !== null) {
      setError(baudRateError);
      focusField(baudRef.current);
      return;
    }
    const keyPath = privateKeyPath.trim();
    onSubmit({
      name: name.trim(),
      groupId,
      protocol,
      host: needsHost ? host.trim() : null,
      port: needsHost ? port : null,
      username: specifyUsername && username.trim() !== "" ? username.trim() : null,
      authKind: keyPath === "" ? "password" : "key",
      extra: buildExtra({
        privateKeyPath,
        keyHasPassphrase,
        credentialId,
        // 编辑时原样带回去，避免一次保存就把口令凭据引用抹掉
        passphraseCredentialId: initialExtra.passphraseCredentialId,
        x11Forwarding,
        executeCommand,
        // 只有 WSL 会话才落发行版；其它协议的残留值一并丢掉
        wslDistro: protocol === "wsl" ? wslDistro : "",
        // 只有串口会话才落设备路径
        serialPort: protocol === "serial" ? serialPort : "",
        // 线路参数同理：非串口协议传 0 / 空串，交给 buildExtra 整组丢掉
        serialBaud: protocol === "serial" ? Number(serialBaud) || 0 : 0,
        serialDataBits: protocol === "serial" ? serialDataBits : 0,
        serialStopBits: protocol === "serial" ? serialStopBits : 0,
        serialParity: protocol === "serial" ? serialParity : "",
        serialFlow: protocol === "serial" ? serialFlow : "",
      }),
    });
  };

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-wide" role="dialog" aria-label="会话设置">
        <div className="dialog-header">
          <h2>会话设置</h2>
          <button type="button" className="dialog-close" aria-label="关闭" onClick={onCancel}>
            <IconClose size={14} />
          </button>
        </div>

        <div className="protocol-strip" ref={protocolRef}>
          {PROTOCOL_OPTIONS.map((option) => (
            <button
              key={option.value}
              type="button"
              className={`protocol-button ${protocol === option.value ? "is-active" : ""}`}
              onClick={() => selectProtocol(option.value)}
            >
              <span className="protocol-button-icon">{option.icon}</span>
              <span className="protocol-button-caption">{option.label}</span>
            </button>
          ))}
        </div>

        <div className="dialog-body">
          {protocol === null ? (
            <p className="protocol-hint">选择会话类型…</p>
          ) : (
            <>
              {needsHost && (
                <section className="settings-group">
                  <h3>基本 {PROTOCOL_LABEL[protocol]} 设置</h3>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-host">
                      远程主机 <span className="required">*</span>
                    </label>
                    <input
                      id="session-host"
                      ref={hostRef}
                      value={host}
                      placeholder="例如 10.0.0.1"
                      onChange={(event) => {
                        setHost(event.target.value);
                        setError(null);
                      }}
                    />
                  </div>

                  <div className="setting-row">
                    <label className="setting-label">
                      <input
                        type="checkbox"
                        className="inline-check"
                        checked={specifyUsername}
                        onChange={(event) => {
                          setSpecifyUsername(event.target.checked);
                          setError(null);
                        }}
                      />
                      指定用户名
                    </label>
                    {/* 不再用 disabled：点不动的输入框会被当成界面坏了，用户还可能把用户名
                        填进「远程主机」。直接聚焦/输入即视为「指定用户名」，勾选框只是回显；
                        焦点留在输入框里，点一下就能打字 */}
                    <input
                      value={username}
                      placeholder="留空则沿用远端登录名"
                      onFocus={() => {
                        if (specifyUsername) return;
                        setSpecifyUsername(true);
                        setError(null);
                      }}
                      onChange={(event) => {
                        setUsername(event.target.value);
                        // 键盘输入不一定先有 focus 事件（例如输入法直填）-> 有内容就启用
                        if (event.target.value !== "") setSpecifyUsername(true);
                      }}
                    />
                  </div>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-port">
                      端口
                    </label>
                    <input
                      id="session-port"
                      ref={portRef}
                      className="input-narrow"
                      type="number"
                      value={port}
                      onChange={(event) =>
                        setPort(Number(event.target.value) || defaultPortFor(protocol))
                      }
                    />
                  </div>
                </section>
              )}

              {/* WSL 是本地子系统 -> 没有主机/端口，只有「用哪个发行版」 */}
              {protocol === "wsl" && (
                <section className="settings-group">
                  <h3>WSL 设置</h3>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-wsl-distro">
                      发行版
                    </label>
                    {wslDistros === null ? (
                      <span className="setting-note">正在检测已安装的 WSL 发行版…</span>
                    ) : wslDistros.length === 0 ? (
                      <div>
                        <div className="setting-note">未检测到 WSL 发行版，请先安装 WSL</div>
                        <div className="setting-note">
                          在 PowerShell 里执行 wsl --install 安装完成后再打开本窗口即可选中发行版；
                          也可以先保存为「默认发行版」，等安装好后直接连接。
                        </div>
                      </div>
                    ) : (
                      <select
                        id="session-wsl-distro"
                        value={wslDistro}
                        onChange={(event) => {
                          setWslDistro(event.target.value);
                          setError(null);
                        }}
                      >
                        <option value="">默认发行版</option>
                        {wslDistro !== "" && !wslDistros.includes(wslDistro) && (
                          <option value={wslDistro}>已卸载的发行版</option>
                        )}
                        {wslDistros.map((name) => (
                          <option key={name} value={name}>
                            {name}
                          </option>
                        ))}
                      </select>
                    )}
                  </div>
                </section>
              )}

              {/* 串口是本地硬件 -> 没有主机/端口，只有「用哪个设备」 */}
              {protocol === "serial" && (
                <section className="settings-group">
                  <h3>串口设置</h3>

                  {/*
                    串口设备：下拉只是「已枚举到的设备」的快捷方式，真值永远取自旁边
                    那个可编辑输入框 —— 后端在 Linux 无 udev 时只扫 /sys/class/tty，
                    PTY（/dev/pts/N）与 /tmp 软链都枚举不到，枚举为空也必须能填。
                  */}
                  <div className="setting-row setting-row-tall">
                    <label className="setting-label" htmlFor="session-serial-port">
                      串口设备 <span className="required">*</span>
                    </label>
                    <div className="serial-port-picker">
                      {serialPorts === null ? (
                        <span className="setting-note">正在检测本机串口设备…</span>
                      ) : serialPorts.length === 0 ? (
                        <span className="setting-note">
                          未检测到可自动识别的设备，可直接填写路径，例如 /dev/ttyUSB0 或 /dev/pts/3。
                        </span>
                      ) : (
                        <select
                          aria-label="从已检测到的串口设备中选择"
                          // 自定义路径不在列表里时下拉回到占位项 -> 不显示错位的旧设备
                          value={
                            serialPorts.some((port) => port.path === serialPort) ? serialPort : ""
                          }
                          onChange={(event) => {
                            if (event.target.value === "") return;
                            setSerialPort(event.target.value);
                            setError(null);
                          }}
                        >
                          <option value="">从已检测到的设备中选择…</option>
                          {serialPorts.map((port) => (
                            <option key={port.path} value={port.path}>
                              {SERIAL_KIND_LABEL[port.kind] ?? "串口"} · {port.path}
                            </option>
                          ))}
                        </select>
                      )}
                      <input
                        id="session-serial-port"
                        ref={serialPortRef}
                        value={serialPort}
                        placeholder="/dev/ttyUSB0 或 /dev/pts/3"
                        onChange={(event) => {
                          setSerialPort(event.target.value);
                          setError(null);
                        }}
                      />
                    </div>
                  </div>
                  {serialPortError !== null && <p className="setting-error">{serialPortError}</p>}

                  {/* 线路参数：波特率可手填（下拉只是常用值），其余四项是固定档位 */}
                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-serial-baud">
                      波特率
                    </label>
                    <input
                      id="session-serial-baud"
                      ref={baudRef}
                      className="input-narrow"
                      list="session-serial-baud-options"
                      inputMode="numeric"
                      value={serialBaud}
                      onChange={(event) => {
                        setSerialBaud(event.target.value);
                        setError(null);
                      }}
                    />
                  </div>
                  <datalist id="session-serial-baud-options">
                    {SERIAL_BAUD_RATES.map((rate) => (
                      <option key={rate} value={rate} />
                    ))}
                  </datalist>
                  {baudRateError !== null && <p className="setting-error">{baudRateError}</p>}

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-serial-data-bits">
                      数据位
                    </label>
                    <select
                      id="session-serial-data-bits"
                      value={String(serialDataBits)}
                      onChange={(event) => {
                        setSerialDataBits(Number(event.target.value));
                        setError(null);
                      }}
                    >
                      {SERIAL_DATA_BIT_OPTIONS.map((bits) => (
                        <option key={bits} value={bits}>
                          {bits}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-serial-stop-bits">
                      停止位
                    </label>
                    <select
                      id="session-serial-stop-bits"
                      value={String(serialStopBits)}
                      onChange={(event) => {
                        setSerialStopBits(Number(event.target.value));
                        setError(null);
                      }}
                    >
                      {SERIAL_STOP_BIT_OPTIONS.map((bits) => (
                        <option key={bits} value={bits}>
                          {bits}
                        </option>
                      ))}
                    </select>
                  </div>
                  {/* 1.5 保留在下拉里，但如实提示当前平台没有对应枚举，不静默降级 */}
                  {stopBitsUnsupported && (
                    <p className="setting-error">{STOP_BITS_UNSUPPORTED_HINT}</p>
                  )}

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-serial-parity">
                      校验
                    </label>
                    <select
                      id="session-serial-parity"
                      value={serialParity}
                      onChange={(event) => {
                        setSerialParity(event.target.value);
                        setError(null);
                      }}
                    >
                      {SERIAL_PARITY_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>
                          {option.label}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-serial-flow">
                      流控
                    </label>
                    <select
                      id="session-serial-flow"
                      value={serialFlow}
                      onChange={(event) => {
                        setSerialFlow(event.target.value);
                        setError(null);
                      }}
                    >
                      {SERIAL_FLOW_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>
                          {option.label}
                        </option>
                      ))}
                    </select>
                  </div>
                </section>
              )}

              {/* Telnet 这一步没有私钥/转发等 SSH 专属选项 -> 不展示这组设置 */}
              {isSshLike(protocol) && (
                <section className="settings-group">
                  <h3>高级 {protocol === "sftp" ? "SFTP" : "SSH"} 设置</h3>

                  <div className="setting-row">
                    <label className="setting-label">
                      <input
                        type="checkbox"
                        className="inline-check"
                        checked={x11Forwarding}
                        onChange={(event) => setX11Forwarding(event.target.checked)}
                      />
                      X11 转发
                    </label>
                    <span className="setting-note">远程图形程序显示到本地桌面</span>
                  </div>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-command">
                      登录后执行命令
                    </label>
                    <input
                      id="session-command"
                      value={executeCommand}
                      placeholder="可选，例如 cd /var/log && tail -f app.log"
                      onChange={(event) => setExecuteCommand(event.target.value)}
                    />
                  </div>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-credential">
                      凭据库密码
                    </label>
                    <select
                      id="session-credential"
                      value={credentialId}
                      onChange={(event) => setCredentialId(event.target.value)}
                    >
                      <option value="">不使用（连接时输入）</option>
                      {credentialId !== "" &&
                        !credentials.some((item) => item.id === credentialId) && (
                          <option value={credentialId}>已删除的凭据</option>
                        )}
                      {credentials.map((item) => (
                        <option key={item.id} value={item.id}>
                          {item.label}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="setting-row">
                    <label className="setting-label" htmlFor="session-key">
                      使用私钥
                    </label>
                    <div className="path-row">
                      <input
                        id="session-key"
                        value={privateKeyPath}
                        placeholder="未选择则使用密码认证"
                        onChange={(event) => setPrivateKeyPath(event.target.value)}
                      />
                      <button type="button" onClick={() => void pickPrivateKey()} disabled={pickingKey}>
                        {pickingKey ? "打开中…" : "浏览…"}
                      </button>
                    </div>
                  </div>

                  <div className="setting-row">
                    <label className="setting-label">
                      <input
                        type="checkbox"
                        className="inline-check"
                        checked={keyHasPassphrase}
                        disabled={privateKeyPath.trim() === ""}
                        onChange={(event) => setKeyHasPassphrase(event.target.checked)}
                      />
                      私钥有口令
                    </label>
                    <span className="setting-note">
                      勾选后连接时会询问私钥口令，这里只记录「有口令」这一事实
                    </span>
                  </div>
                </section>
              )}

              <section className="settings-group">
                <h3>会话</h3>

                <div className="setting-row">
                  <label className="setting-label" htmlFor="session-name">
                    名称 <span className="required">*</span>
                  </label>
                  <input
                    id="session-name"
                    ref={nameRef}
                    value={name}
                    placeholder="例如 生产-web-01"
                    onChange={(event) => {
                      setName(event.target.value);
                      setError(null);
                    }}
                  />
                </div>

                <div className="setting-row">
                  <label className="setting-label" htmlFor="session-group">
                    分组
                  </label>
                  <select
                    id="session-group"
                    value={groupId ?? ""}
                    onChange={(event) => setGroupId(event.target.value || null)}
                  >
                    <option value="">（未分组）</option>
                    {groupOptions.map((option) => (
                      <option key={option.id} value={option.id}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>
              </section>

              {error && <p className="form-error">{error}</p>}
            </>
          )}
        </div>

        <div className="dialog-footer">
          <button type="button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="primary"
            onClick={submit}
            disabled={
              busy || protocol === null || baudRateError !== null || serialPortError !== null
            }
          >
            {busy ? "保存中…" : mode === "create" ? "确定" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
