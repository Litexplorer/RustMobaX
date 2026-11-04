import { useEffect, useState } from "react";
import { errorMessage } from "../api/session";
import type { Session } from "../api/session";
import { tunnelConfigCreate, tunnelConfigUpdate, tunnelList } from "../api/tunnel";
import type { TunnelConfig, TunnelConfigInput, TunnelKindName } from "../api/tunnel";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  /** 要编辑的配置；null 表示新建 */
  config: TunnelConfig | null;
  /** 可作为隧道源的会话，父层已过滤成 ssh / sftp */
  sessions: Session[];
  /** 保存成功后回传落库结果，由父层负责关闭弹窗并刷新列表 */
  onSaved: (config: TunnelConfig) => void;
  onCancel: () => void;
};

const KIND_OPTIONS: { value: TunnelKindName; label: string; hint: string }[] = [
  { value: "local", label: "本地转发", hint: "本机监听，连接转到远端目标" },
  { value: "remote", label: "远程转发", hint: "远端监听，转发回本机可达的目标" },
  { value: "dynamic", label: "动态 SOCKS5", hint: "本机监听充当 SOCKS5 代理，无需目标" },
];

/** 字段级错误：键与下面的输入框一一对应，显示在对应字段下方。 */
type FieldErrors = {
  sessionId?: string;
  listenPort?: string;
  targetHost?: string;
  targetPort?: string;
};

/**
 * 会话下拉的选项文案。
 *
 * 参数：session 为一条 SSH / SFTP 会话
 * 核心流程：拼成「名称 (user@host:port)」，缺字段时留空而不是写 undefined。
 */
function sessionLabel(session: Session): string {
  const user = session.username ?? "";
  const host = session.host ?? "";
  const port = session.port ? `:${session.port}` : "";
  return `${session.name} (${user}@${host}${port})`;
}

/**
 * 解析端口输入。
 *
 * 参数：raw 为用户输入，allowAuto 表示空值或 0 是否合法（监听端口可自动分配）
 * 核心流程：空值按自动分配处理，非数字或超出范围返回 null 由调用方报错。
 */
function readPort(raw: string, allowAuto: boolean): number | null {
  const trimmed = raw.trim();
  if (trimmed === "") return allowAuto ? 0 : null;
  if (!/^\d+$/.test(trimmed)) return null;
  const value = Number(trimmed);
  if (value === 0) return allowAuto ? 0 : null;
  if (value > 65535) return null;
  return value;
}

/**
 * 新建 / 编辑隧道配置弹窗。
 *
 * 参数：见 Props
 * 核心流程：字段校验全部在前端先过一遍（端口范围、非 dynamic 必须有目标），
 * 通过后调 create 或 update；后端报错原样贴在底部，不弹原生 alert。
 */
export function TunnelFormDialog(props: Props) {
  const { config, sessions, onSaved, onCancel } = props;

  const [name, setName] = useState(config?.name ?? "");
  const [sessionId, setSessionId] = useState(config?.sessionId ?? sessions[0]?.id ?? "");
  const [kind, setKind] = useState<TunnelKindName>(config?.kind ?? "local");
  const [listenHost, setListenHost] = useState(config?.listenHost ?? "127.0.0.1");
  const [listenPort, setListenPort] = useState(config ? String(config.listenPort) : "");
  const [targetHost, setTargetHost] = useState(config?.targetHost ?? "");
  const [targetPort, setTargetPort] = useState(
    config && config.targetPort > 0 ? String(config.targetPort) : "",
  );

  const [errors, setErrors] = useState<FieldErrors>({});
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  /** 要编辑的配置正在运行：改动不会影响已建立的连接，保存后需重新启动才生效 */
  const [runningHint, setRunningHint] = useState(false);

  // Esc = 关闭表单（等价于点「取消」/「×」，不做二次确认）；
  // 保存中不生效，否则刚发出的保存结果回来时弹窗已经不在了
  useEscapeClose(onCancel, !saving);

  useEffect(() => {
    if (!config) return;
    const configId = config.id;
    let disposed = false;
    void tunnelList()
      .then((list) => {
        if (disposed) return;
        setRunningHint(list.some((item) => item.id === configId && item.running));
      })
      .catch(() => {
        // 运行态读不到就不提示，不影响正常编辑
      });
    return () => {
      disposed = true;
    };
  }, [config]);

  const needsTarget = kind !== "dynamic";
  const noSessions = sessions.length === 0;
  /** 编辑一条其来源会话已被删掉的配置时，补一个占位选项，避免下拉回落到第一条 */
  const missingSession = sessionId !== "" && !sessions.some((item) => item.id === sessionId);

  /**
   * 校验并保存。
   *
   * 参数：无
   * 核心流程：先逐字段校验并把错误挂到对应输入框，全部通过才组装 camelCase 请求；
   * dynamic 类型不发送目标，统一传空串 / 0，避免后端收到无意义的值。
   */
  const save = async () => {
    const nextErrors: FieldErrors = {};
    if (sessionId === "") nextErrors.sessionId = "请先创建 SSH 会话";

    const listen = readPort(listenPort, true);
    if (listen === null) nextErrors.listenPort = "监听端口应为 0 到 65535，留空表示自动分配";

    let target: number | null = 0;
    if (needsTarget) {
      if (targetHost.trim() === "") nextErrors.targetHost = "请填写目标地址";
      target = readPort(targetPort, false);
      if (target === null) nextErrors.targetPort = "目标端口应为 1 到 65535 的数字";
    }

    setErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) {
      setSubmitError(null);
      return;
    }

    const input: TunnelConfigInput = {
      name: name.trim(),
      sessionId,
      kind,
      listenHost: listenHost.trim(),
      listenPort: listen ?? 0,
      targetHost: needsTarget ? targetHost.trim() : "",
      targetPort: needsTarget ? (target ?? 0) : 0,
    };

    setSaving(true);
    setSubmitError(null);
    try {
      const saved = config
        ? await tunnelConfigUpdate(config.id, input)
        : await tunnelConfigCreate(input);
      onSaved(saved);
    } catch (err) {
      setSubmitError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-wide" role="dialog" aria-label="隧道配置">
        <div className="dialog-header">
          <h2>{config ? "编辑隧道" : "新建隧道"}</h2>
          <button type="button" className="dialog-close" aria-label="关闭" onClick={onCancel}>
            ×
          </button>
        </div>

        <div className="dialog-body tunnel-form-body">
          {runningHint && (
            <p className="setting-note">该隧道正在运行，修改将在下次启动后生效。</p>
          )}
          <div className="settings-group">
            <div className="setting-row">
              <label className="setting-label" htmlFor="tunnel-form-name">
                名称
              </label>
              <input
                id="tunnel-form-name"
                value={name}
                placeholder="留空由后端自动生成"
                onChange={(event) => setName(event.target.value)}
              />
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="tunnel-form-session">
                服务器
              </label>
              <select
                id="tunnel-form-session"
                value={sessionId}
                onChange={(event) => {
                  setSessionId(event.target.value);
                  setErrors((prev) => ({ ...prev, sessionId: undefined }));
                }}
              >
                {noSessions && <option value="">没有可用的 SSH 会话</option>}
                {missingSession && <option value={sessionId}>（会话已删除）</option>}
                {sessions.map((item) => (
                  <option key={item.id} value={item.id}>
                    {sessionLabel(item)}
                  </option>
                ))}
              </select>
            </div>
            {errors.sessionId && <p className="setting-error">{errors.sessionId}</p>}
            {noSessions && <p className="setting-note">请先创建 SSH 会话</p>}

            <div className="setting-row">
              <label className="setting-label" htmlFor="tunnel-form-kind">
                类型
              </label>
              <select
                id="tunnel-form-kind"
                value={kind}
                onChange={(event) => setKind(event.target.value as TunnelKindName)}
              >
                {KIND_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            </div>
            <p className="setting-note">
              {KIND_OPTIONS.find((option) => option.value === kind)?.hint}
            </p>

            <div className="setting-row">
              <label className="setting-label" htmlFor="tunnel-form-listen-host">
                监听地址
              </label>
              <input
                id="tunnel-form-listen-host"
                value={listenHost}
                placeholder="127.0.0.1"
                onChange={(event) => setListenHost(event.target.value)}
              />
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="tunnel-form-listen-port">
                监听端口
              </label>
              <input
                id="tunnel-form-listen-port"
                className="input-narrow"
                value={listenPort}
                placeholder="0"
                onChange={(event) => {
                  setListenPort(event.target.value);
                  setErrors((prev) => ({ ...prev, listenPort: undefined }));
                }}
              />
            </div>
            {errors.listenPort ? (
              <p className="setting-error">{errors.listenPort}</p>
            ) : (
              <p className="setting-note">留空或填 0 表示自动分配；建立后以列表里的实际端口为准</p>
            )}

            {needsTarget && (
              <>
                <div className="setting-row">
                  <label className="setting-label" htmlFor="tunnel-form-target-host">
                    目标地址
                  </label>
                  <input
                    id="tunnel-form-target-host"
                    value={targetHost}
                    placeholder="例如 db.internal"
                    onChange={(event) => {
                      setTargetHost(event.target.value);
                      setErrors((prev) => ({ ...prev, targetHost: undefined }));
                    }}
                  />
                </div>
                {errors.targetHost && <p className="setting-error">{errors.targetHost}</p>}

                <div className="setting-row">
                  <label className="setting-label" htmlFor="tunnel-form-target-port">
                    目标端口
                  </label>
                  <input
                    id="tunnel-form-target-port"
                    className="input-narrow"
                    value={targetPort}
                    placeholder="5432"
                    onChange={(event) => {
                      setTargetPort(event.target.value);
                      setErrors((prev) => ({ ...prev, targetPort: undefined }));
                    }}
                  />
                </div>
                {errors.targetPort && <p className="setting-error">{errors.targetPort}</p>}
              </>
            )}

            {!needsTarget && <p className="setting-note">SOCKS5 代理，无需填写目标地址与端口</p>}

            {submitError && <p className="form-error">{submitError}</p>}
          </div>
        </div>

        <div className="dialog-footer">
          <button type="button" onClick={onCancel} disabled={saving}>
            取消
          </button>
          <button
            type="button"
            className="primary"
            disabled={saving || noSessions}
            onClick={() => void save()}
          >
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
