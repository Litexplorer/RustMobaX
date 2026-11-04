import { useCallback, useEffect, useState } from "react";
import { errorDetail, errorKind, errorMessage } from "../api/session";
import type { Session } from "../api/session";
import type { HostKeyInfo } from "../api/terminal";
import {
  tunnelConfigDelete,
  tunnelConfigList,
  tunnelList,
  tunnelStart,
  tunnelStop,
} from "../api/tunnel";
import type { TunnelConfig, TunnelInfo, TunnelKindName } from "../api/tunnel";
import { useEscapeClose } from "../lib/useEscapeClose";
import { ConfirmDialog } from "./ConfirmDialog";

type Props = {
  /** 可作为隧道源的会话，父层已过滤成 ssh / sftp（新建 / 编辑表单要用） */
  sessions: Session[];
  /**
   * 请用户确认未知主机指纹。
   *
   * 由 App 层复用终端那套 HostKeyDialog 实现：true 表示已信任（调用方自己重试），
   * false 表示用户取消或信任失败。
   */
  onConfirmHostKey: (info: HostKeyInfo) => Promise<boolean>;
  /** 父层在表单保存后自增，用来触发一次重新拉取 */
  revision: number;
  /** 打开新建（null）或编辑（带配置）表单，两个弹窗可叠加 */
  onOpenForm: (config: TunnelConfig | null) => void;
  /** 成功类提示回传到主界面状态栏 */
  onNotify: (text: string) => void;
  onClose: () => void;
};

/** 列表里的一行：配置与运行态按 id 合并后的结果。 */
type Row = {
  id: string;
  name: string;
  kind: TunnelKindName;
  running: boolean;
  listenHost: string;
  listenPort: number;
  targetHost: string;
  targetPort: number;
  /** 配置本体；运行态里多出来的孤儿项为 null，只能停止 */
  config: TunnelConfig | null;
};

/** 每行正在进行的动作，键为配置 id：用来做过渡态并阻止重复点击。 */
type PendingMap = Record<string, "start" | "stop" | undefined>;

type Banner = { tone: "info" | "error"; text: string };

/** 用户在指纹弹窗里取消（或信任失败）时的固定提示文案。 */
const HOST_KEY_CANCELLED = "已取消，未信任主机指纹";

/** 一次启动的结果：成功带回运行态，失败只带一句可直接展示的文案。 */
type StartOutcome = { ok: true; info: TunnelInfo } | { ok: false; message: string };

/**
 * 启动一条隧道，遇到未知主机指纹时先确认再重试一次。
 *
 * 参数：id 为配置 id，confirm 为父层提供的指纹确认入口
 * 核心流程：只有 kind=hostKeyUnknown 且带 detail 的错误才升级成指纹弹窗
 * （与终端连接共用同一个 HostKeyDialog）；用户确认信任后重试一次，
 * 重试再失败只回报后端原始错误，不会二次弹窗；用户取消则回固定文案。
 */
async function startWithHostKey(
  id: string,
  confirm: (info: HostKeyInfo) => Promise<boolean>,
): Promise<StartOutcome> {
  try {
    return { ok: true, info: await tunnelStart(id) };
  } catch (err) {
    if (errorKind(err) !== "hostKeyUnknown") {
      return { ok: false, message: errorMessage(err) };
    }
    const detail = errorDetail<HostKeyInfo>(err);
    if (!detail) {
      return { ok: false, message: errorMessage(err) };
    }
    const trusted = await confirm(detail);
    if (!trusted) {
      return { ok: false, message: HOST_KEY_CANCELLED };
    }
    try {
      return { ok: true, info: await tunnelStart(id) };
    } catch (retryErr) {
      return { ok: false, message: errorMessage(retryErr) };
    }
  }
}

/**
 * 取转发类型的中文短标签。
 *
 * 参数：kind 为转发类型
 * 核心流程：只用于列表展示，未知类型按动态转发兜底。
 */
function kindLabel(kind: TunnelKindName): string {
  if (kind === "local") return "本地";
  if (kind === "remote") return "远程";
  return "动态";
}

/**
 * 拼监听 / 目标两列的展示文本。
 *
 * 参数：host / port 为地址与端口，emptyPort 为端口不可用时的占位文案
 * 核心流程：地址为空时回落到后端默认的 127.0.0.1；端口 <= 0 说明还没分配。
 */
function addressText(host: string, port: number, emptyPort: string): string {
  const shownHost = host.trim() === "" ? "127.0.0.1" : host.trim();
  return port > 0 ? `${shownHost}:${port}` : `${shownHost}:${emptyPort}`;
}

/**
 * 按 id 合并配置与运行态。
 *
 * 参数：configs 为库里的配置，runtime 为当前运行态
 * 核心流程：先按配置顺序出行（并吸收运行态里的实际端口），再把运行态里多出来的
 * 孤儿项追加在后面，这样配置已删除但还在跑的隧道不会凭空消失。
 */
function mergeRows(configs: TunnelConfig[], runtime: TunnelInfo[]): Row[] {
  const live = new Map(runtime.map((item) => [item.id, item]));
  const rows: Row[] = [];
  const seen = new Set<string>();

  for (const config of configs) {
    const run = live.get(config.id);
    seen.add(config.id);
    rows.push({
      id: config.id,
      name: (run?.name || config.name || "").trim(),
      kind: config.kind,
      running: run?.running === true,
      listenHost: run?.listenHost || config.listenHost,
      // 端口以运行态为准；未运行时回落到配置里的期望值
      listenPort: run && run.listenPort > 0 ? run.listenPort : config.listenPort,
      targetHost: run?.targetHost || config.targetHost,
      targetPort: run && run.targetPort > 0 ? run.targetPort : config.targetPort,
      config,
    });
  }

  for (const run of runtime) {
    if (seen.has(run.id)) continue;
    rows.push({
      id: run.id,
      name: run.name.trim() === "" ? "(已删除配置)" : run.name,
      kind: run.kind,
      running: run.running,
      listenHost: run.listenHost,
      listenPort: run.listenPort,
      targetHost: run.targetHost,
      targetPort: run.targetPort,
      config: null,
    });
  }

  return rows;
}

/**
 * 端口转发（隧道）列表弹窗。
 *
 * 参数：见 Props
 * 核心流程：顶部工具栏（新建 / 全部启动 / 全部停止 / 刷新），主体按配置列出全部隧道，
 * 行内直接启停、编辑、删除；配置与运行态分开拉取后按 id 合并，
 * 所有失败原因都用单行提示条就地显示，不弹原生 alert。
 */
export function TunnelDialog(props: Props) {
  const { revision, onOpenForm, onNotify, onClose, onConfirmHostKey } = props;

  // Esc = 关闭列表；叠在它上面的表单 / 删除确认会自己入栈成为栈顶，
  // 所以连按 Esc 是「先关上层，再关列表」
  useEscapeClose(onClose);

  const [rows, setRows] = useState<Row[]>([]);
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState<PendingMap>({});
  const [banner, setBanner] = useState<Banner | null>(null);
  /** 待确认删除的行，非空时叠加 ConfirmDialog */
  const [confirming, setConfirming] = useState<Row | null>(null);
  /** 有任何一行在启停中：禁用批量按钮，避免叠着操作 */
  const busy = Object.keys(pending).length > 0;

  /**
   * 全量刷新：配置 + 运行态。
   *
   * 参数：无
   * 核心流程：配置拿不到才算整体失败；运行态失败只降级成「都当成没在跑」并提示，
   * 这样后端个别命令还没准备好时列表仍然可用。
   */
  const load = useCallback(async () => {
    setLoading(true);
    try {
      const configs = await tunnelConfigList();
      let runtime: TunnelInfo[] = [];
      let runtimeError: string | null = null;
      try {
        runtime = await tunnelList();
      } catch (err) {
        runtimeError = errorMessage(err);
      }
      setRows(mergeRows(configs, runtime));
      setBanner(
        runtimeError ? { tone: "error", text: `运行状态读取失败：${runtimeError}` } : null,
      );
    } catch (err) {
      setBanner({ tone: "error", text: errorMessage(err) });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    // revision 变化（表单保存后）就重新拉一次，让列表反映最新配置
  }, [load, revision]);

  /**
   * 只刷新运行态。
   *
   * 参数：无
   * 核心流程：启停之后调用，把每行的 running 与实际端口同步过来；
   * 运行态里消失的项视为已停止，监听端口回落到配置值。
   */
  const syncRuntime = useCallback(async () => {
    try {
      const runtime = await tunnelList();
      const live = new Map(runtime.map((item) => [item.id, item]));
      setRows((prev) =>
        prev.map((row) => {
          const run = live.get(row.id);
          if (!run) {
            return {
              ...row,
              running: false,
              listenPort: row.config ? row.config.listenPort : row.listenPort,
            };
          }
          return {
            ...row,
            running: run.running,
            listenHost: run.listenHost || row.listenHost,
            listenPort: run.listenPort > 0 ? run.listenPort : row.listenPort,
            targetHost: run.targetHost || row.targetHost,
            targetPort: run.targetPort > 0 ? run.targetPort : row.targetPort,
          };
        }),
      );
    } catch (err) {
      setBanner({ tone: "error", text: errorMessage(err) });
    }
  }, []);

  const mark = (id: string, action: "start" | "stop") =>
    setPending((prev) => ({ ...prev, [id]: action }));

  const unmark = (id: string) =>
    setPending((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });

  /**
   * 启动一条隧道。
   *
   * 参数：row 为目标行
   * 核心流程：行内按钮走这条路径；未知主机指纹会拉起 HostKeyDialog，
   * 取消或失败的原因（例如「凭据库未解锁」）都贴在提示条里，行回到「已停止」。
   */
  const startOne = async (row: Row) => {
    if (pending[row.id] || busy) return;
    setBanner(null);
    mark(row.id, "start");
    try {
      const outcome = await startWithHostKey(row.id, onConfirmHostKey);
      if (!outcome.ok) {
        setBanner({ tone: "error", text: outcome.message });
        return;
      }
      onNotify(`隧道已启动：${outcome.info.name || row.name || outcome.info.id}`);
      await syncRuntime();
    } finally {
      unmark(row.id);
    }
  };

  /**
   * 停止一条隧道。
   *
   * 参数：row 为目标行
   * 核心流程：孤儿项（配置已删除）也能走这里，停止后它自然从列表消失。
   */
  const stopOne = async (row: Row) => {
    if (pending[row.id] || busy) return;
    setBanner(null);
    mark(row.id, "stop");
    try {
      await tunnelStop(row.id);
      onNotify(`已停止隧道：${row.name || row.id}`);
      await syncRuntime();
    } catch (err) {
      setBanner({ tone: "error", text: errorMessage(err) });
    } finally {
      unmark(row.id);
    }
  };

  /**
   * 全部启动。
   *
   * 参数：无
   * 核心流程：串行逐条启动，单条失败只记下来不打断后面的，最后汇总成一条提示；
   * 串行还能让失败原因指向具体名字，也避免同时建多条连接。
   */
  const startAll = async () => {
    const targets = rows.filter((row) => row.config !== null && !row.running);
    if (targets.length === 0) {
      setBanner({ tone: "info", text: "没有需要启动的隧道" });
      return;
    }
    setBanner(null);
    let ok = 0;
    const failures: string[] = [];
    for (const row of targets) {
      mark(row.id, "start");
      try {
        // 批量启动里遇到未知主机同样就地确认一次；串行执行不会叠出多个弹窗
        const outcome = await startWithHostKey(row.id, onConfirmHostKey);
        if (outcome.ok) {
          ok += 1;
        } else {
          failures.push(`${row.name || row.id}（${outcome.message}）`);
        }
      } finally {
        unmark(row.id);
      }
    }
    await syncRuntime();
    if (failures.length === 0) {
      setBanner({ tone: "info", text: `已启动 ${ok} 条隧道` });
      onNotify(`已启动 ${ok} 条隧道`);
    } else {
      setBanner({
        tone: "error",
        text: `${ok} 个成功，${failures.length} 个失败：${failures.join("；")}`,
      });
    }
  };

  /**
   * 全部停止。
   *
   * 参数：无
   * 核心流程：与全部启动对称，失败项带上原因汇总，成功的照常停下。
   */
  const stopAll = async () => {
    const targets = rows.filter((row) => row.running);
    if (targets.length === 0) {
      setBanner({ tone: "info", text: "没有正在运行的隧道" });
      return;
    }
    setBanner(null);
    let ok = 0;
    const failures: string[] = [];
    for (const row of targets) {
      mark(row.id, "stop");
      try {
        await tunnelStop(row.id);
        ok += 1;
      } catch (err) {
        failures.push(`${row.name || row.id}（${errorMessage(err)}）`);
      } finally {
        unmark(row.id);
      }
    }
    await syncRuntime();
    if (failures.length === 0) {
      setBanner({ tone: "info", text: `已停止 ${ok} 条隧道` });
      onNotify(`已停止 ${ok} 条隧道`);
    } else {
      setBanner({
        tone: "error",
        text: `${ok} 个成功，${failures.length} 个失败：${failures.join("；")}`,
      });
    }
  };

  /**
   * 删除一条配置（已二次确认）。
   *
   * 参数：row 为待删除的行
   * 核心流程：运行中先停再删，避免留下无法再管理的连接；完成后整体重拉列表。
   */
  const removeOne = async (row: Row) => {
    setBanner(null);
    mark(row.id, "stop");
    try {
      if (row.running) await tunnelStop(row.id);
      await tunnelConfigDelete(row.id);
      onNotify(`已删除隧道：${row.name || row.id}`);
      await load();
    } catch (err) {
      setBanner({ tone: "error", text: errorMessage(err) });
    } finally {
      unmark(row.id);
    }
  };

  const runningCount = rows.filter((row) => row.running).length;

  return (
    <>
      <div className="modal-backdrop" role="presentation">
        <div className="modal modal-wide tunnel-dialog" role="dialog" aria-label="SSH 隧道">
          <div className="dialog-header">
            <h2>SSH 隧道</h2>
            <button type="button" className="dialog-close" aria-label="关闭" onClick={onClose}>
              ×
            </button>
          </div>

          <div className="dialog-body tunnel-body">
            <div className="tunnel-toolbar">
              <span className="tunnel-toolbar-count">
                {loading ? "读取中…" : `共 ${rows.length} 条 · 运行中 ${runningCount} 条`}
              </span>
              <div className="tunnel-toolbar-actions">
                <button
                  type="button"
                  className="primary"
                  disabled={loading}
                  onClick={() => onOpenForm(null)}
                >
                  新建隧道
                </button>
                <button type="button" disabled={busy || loading} onClick={() => void startAll()}>
                  全部启动
                </button>
                <button type="button" disabled={busy || loading} onClick={() => void stopAll()}>
                  全部停止
                </button>
                <button type="button" disabled={loading} onClick={() => void load()}>
                  {loading ? "刷新中…" : "刷新"}
                </button>
              </div>
            </div>

            {banner && <p className={`tunnel-banner is-${banner.tone}`}>{banner.text}</p>}

            {rows.length === 0 ? (
              <div className="tunnel-empty">
                <p>{loading ? "正在读取隧道配置…" : "还没有隧道，点击「新建隧道」创建"}</p>
                {!loading && (
                  <button type="button" className="primary" onClick={() => onOpenForm(null)}>
                    新建隧道
                  </button>
                )}
              </div>
            ) : (
              <div className="tunnel-table-scroll">
              <table className="tunnel-table">
                <thead>
                  <tr>
                    <th className="tunnel-col-state">状态</th>
                    <th className="tunnel-col-name">名称</th>
                    <th className="tunnel-col-kind">类型</th>
                    <th className="tunnel-col-listen">监听</th>
                    <th className="tunnel-col-target">目标</th>
                    <th className="tunnel-col-actions">操作</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row) => {
                    const pendingAction = pending[row.id] ?? null;
                    const stateText =
                      pendingAction === "start"
                        ? "启动中…"
                        : pendingAction === "stop"
                          ? "停止中…"
                          : row.running
                            ? "运行中"
                            : "已停止";
                    const stateClass =
                      pendingAction !== null
                        ? "is-pending"
                        : row.running
                          ? "is-running"
                          : "is-stopped";
                    const listenText = addressText(row.listenHost, row.listenPort, "自动");
                    const targetText =
                      row.kind === "dynamic"
                        ? "SOCKS5"
                        : addressText(row.targetHost, row.targetPort, "-");
                    const orphan = row.config === null;
                    return (
                      <tr key={row.id} className={orphan ? "is-orphan" : undefined}>
                        <td className="tunnel-col-state">
                          <span className="tunnel-status" title={stateText}>
                            <span className={`tunnel-dot ${stateClass}`} aria-hidden="true" />
                            {stateText}
                          </span>
                        </td>
                        <td className="tunnel-col-name">
                          <span className="tunnel-name" title={row.name || "(未命名)"}>
                            {row.name === "" ? "(未命名)" : row.name}
                          </span>
                        </td>
                        <td className="tunnel-col-kind">{kindLabel(row.kind)}</td>
                        <td className="tunnel-col-listen" title={listenText}>
                          {listenText}
                        </td>
                        <td className="tunnel-col-target" title={targetText}>
                          {targetText}
                        </td>
                        <td className="tunnel-col-actions">
                          <div className="tunnel-row-actions">
                            {row.running ? (
                              <button
                                type="button"
                                className="is-stop"
                                disabled={pendingAction !== null || busy}
                                onClick={() => void stopOne(row)}
                              >
                                {pendingAction === "stop" ? "停止中…" : "停止"}
                              </button>
                            ) : (
                              <button
                                type="button"
                                className="is-start"
                                disabled={pendingAction !== null || orphan || busy}
                                onClick={() => void startOne(row)}
                              >
                                {pendingAction === "start" ? "启动中…" : "启动"}
                              </button>
                            )}
                            <button
                              type="button"
                              disabled={orphan || pendingAction !== null}
                              title={orphan ? "配置已删除，无法编辑" : "编辑这条隧道"}
                              onClick={() => row.config && onOpenForm(row.config)}
                            >
                              编辑
                            </button>
                            <button
                              type="button"
                              className="danger"
                              disabled={orphan || pendingAction !== null}
                              title={orphan ? "配置已删除，无法删除" : "删除这条隧道"}
                              onClick={() => setConfirming(row)}
                            >
                              删除
                            </button>
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
              </div>
            )}
          </div>

          <div className="dialog-footer">
            <button type="button" onClick={onClose}>
              关闭
            </button>
          </div>
        </div>
      </div>

      {confirming && (
        <ConfirmDialog
          message={`确定删除隧道「${confirming.name || confirming.id}」吗？${
            confirming.running ? "该隧道正在运行，将先停止再删除。" : ""
          }`}
          onConfirm={() => {
            const target = confirming;
            setConfirming(null);
            void removeOne(target);
          }}
          onCancel={() => setConfirming(null)}
        />
      )}
    </>
  );
}
