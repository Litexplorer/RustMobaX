import { useCallback, useEffect, useMemo, useState } from "react";
import { errorDetail, errorKind, errorMessage } from "../api/session";
import type { Session } from "../api/session";
import type { HostKeyInfo } from "../api/terminal";
import {
  tunnelConfigDelete,
  tunnelConfigList,
  tunnelConfigUpdate,
  tunnelList,
  tunnelStart,
  tunnelStop,
} from "../api/tunnel";
import type { TunnelConfig, TunnelInfo, TunnelKindName } from "../api/tunnel";
import { useEscapeClose } from "../lib/useEscapeClose";
import { ConfirmDialog } from "./ConfirmDialog";
import { IconSearch } from "./icons";

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
  /**
   * 启动前置检查：由父层统一判断「这批隧道要不要先解锁凭据库」。
   *
   * 参数 tunnelIds 为本次想启动的隧道 id（行内启动传一个，全部启动传整批）。
   * 返回 true 表示父层已改成弹出解锁框接管这批启动：调用方不要再调 tunnel_start，
   * 也不要贴错误提示或占着过渡态（解锁成功后由父层自己把这批启动续上）。
   * 整批一次问完，批量启动因此只会弹一次解锁框。
   */
  needsVaultBeforeStart: (tunnelIds: string[]) => Promise<boolean>;
  onClose: () => void;
};

/** 列表里的一行：配置与运行态按 id 合并后的结果。 */
type Row = {
  id: string;
  name: string;
  /** 来源会话名；配置里的会话已被删除时用固定文案，孤儿行没有来源配置则留空 */
  sessionName: string;
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

/**
 * 每行「自动启动」开关是否正在提交，键为配置 id。
 *
 * 与 PendingMap 分开存：开关只改配置、不动运行态，不该让状态列显示「启动中…」，
 * 也不该因为它在提交就把同一行的启停按钮判成忙。
 */
type AutoPendingMap = Record<string, boolean | undefined>;

type Banner = { tone: "info" | "error"; text: string };

/** 用户在指纹弹窗里取消（或信任失败）时的固定提示文案。 */
const HOST_KEY_CANCELLED = "已取消，未信任主机指纹";

/** 配置指向的会话已经不存在时的固定文案（与新建 / 编辑表单里的占位选项一致）。 */
const SESSION_DELETED = "（会话已删除）";

/** 分页条可选的每页条数。 */
const PAGE_SIZES = [10, 20, 50];

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
 * 参数：configs 为库里的配置，runtime 为当前运行态，sessionNames 为会话 id 到名称的映射
 * 核心流程：先按配置顺序出行（并吸收运行态里的实际端口），再把运行态里多出来的
 * 孤儿项追加在后面，这样配置已删除但还在跑的隧道不会凭空消失。
 */
function mergeRows(
  configs: TunnelConfig[],
  runtime: TunnelInfo[],
  sessionNames: Map<string, string>,
): Row[] {
  const live = new Map(runtime.map((item) => [item.id, item]));
  const rows: Row[] = [];
  const seen = new Set<string>();

  for (const config of configs) {
    const run = live.get(config.id);
    seen.add(config.id);
    rows.push({
      id: config.id,
      name: (run?.name || config.name || "").trim(),
      sessionName: sessionNames.get(config.sessionId) ?? SESSION_DELETED,
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
      // 孤儿行只剩运行态，拿不到来源会话，留空即不参与按会话名的匹配
      sessionName: "",
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
 * 判断一行是否命中搜索关键字。
 *
 * 参数：row 为合并后的行，keyword 为用户输入
 * 核心流程：关键字去掉首尾空白并转小写后做包含匹配，覆盖名称、来源会话、类型
 * （英文 local / remote / dynamic 与中文标签同时认）、监听与目标地址端口；
 * 关键字为空白时全部命中，清空搜索即恢复完整列表。
 */
function matchRow(row: Row, keyword: string): boolean {
  const needle = keyword.trim().toLowerCase();
  if (needle === "") return true;
  const fields = [
    row.name,
    row.sessionName,
    row.kind,
    kindLabel(row.kind),
    `${row.listenHost}:${row.listenPort}`,
    `${row.targetHost}:${row.targetPort}`,
  ];
  return fields.some((field) => field.toLowerCase().includes(needle));
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
  const {
    revision,
    sessions,
    onOpenForm,
    onNotify,
    onClose,
    onConfirmHostKey,
    needsVaultBeforeStart,
  } = props;

  // Esc = 关闭列表；叠在它上面的表单 / 删除确认会自己入栈成为栈顶，
  // 所以连按 Esc 是「先关上层，再关列表」
  useEscapeClose(onClose);

  const [rows, setRows] = useState<Row[]>([]);
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState<PendingMap>({});
  const [autoPending, setAutoPending] = useState<AutoPendingMap>({});
  const [banner, setBanner] = useState<Banner | null>(null);
  /** 待确认删除的行，非空时叠加 ConfirmDialog */
  const [confirming, setConfirming] = useState<Row | null>(null);
  /** 搜索关键字：纯前端过滤，不发任何请求 */
  const [keyword, setKeyword] = useState("");
  /** 当前页码：从 1 起；数据变少时由 current 夹取，不会渲染成空页 */
  const [page, setPage] = useState(1);
  /** 每页条数：纯前端分片，可选 10 / 20 / 50 */
  const [pageSize, setPageSize] = useState(20);
  /** 有任何一行在启停中：禁用批量按钮，避免叠着操作 */
  const busy = Object.keys(pending).length > 0;

  /**
   * 会话 id -> 名称：列表要显示每条隧道的来源会话，也支持按会话名搜索。
   *
   * 父层每次渲染都用 filter 重建 sessions 数组，引用每次都变；所以用「id + 名称」
   * 拼出的内容签名做依赖，内容没变就复用同一个 Map，load 的依赖也就不会变，
   * 避免父层其它状态刷新（例如指标轮询）把隧道列表跟着重拉一遍。
   */
  const sessionSignature = sessions.map((item) => `${item.id}\u0000${item.name}`).join("\u0001");
  const sessionNames = useMemo(
    () => new Map(sessions.map((item) => [item.id, item.name])),
    [sessionSignature],
  );

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
      setRows(mergeRows(configs, runtime, sessionNames));
      setBanner(
        runtimeError ? { tone: "error", text: `运行状态读取失败：${runtimeError}` } : null,
      );
    } catch (err) {
      setBanner({ tone: "error", text: errorMessage(err) });
    } finally {
      setLoading(false);
    }
  }, [sessionNames]);

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
   * 核心流程：行内按钮走这条路径；先问一次父层要不要先解锁凭据库 ——
   * 要的话父层弹出解锁框并接管这次启动，本行直接收尾（过渡态在 finally 里复原，
   * 不会卡在「启动中…」）。之后才是未知主机指纹拉起 HostKeyDialog，
   * 取消或失败的原因都贴在提示条里，行回到「已停止」。
   */
  const startOne = async (row: Row) => {
    if (pending[row.id] || busy) return;
    setBanner(null);
    mark(row.id, "start");
    try {
      if (await needsVaultBeforeStart([row.id])) return;
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
   * 切换一条配置的「自动启动」。
   *
   * 参数：row 为目标行
   * 核心流程：只改配置、不动运行态，因此不走 PendingMap（状态列不会闪「启动中…」，
   * 同行的启停按钮也不会因为它在提交而变灰）。成功后用后端返回值就地替换该行 config ——
   * 列表开关与之后打开的编辑表单读的都是同一份数据，两边不会打架；
   * 失败只把原因贴到列表提示条，不动本地状态。孤儿行没有配置可改，不渲染开关。
   */
  const toggleAutoStart = async (row: Row) => {
    const config = row.config;
    if (!config || autoPending[row.id]) return;
    const wanted = config.autoStart !== true;
    setBanner(null);
    setAutoPending((prev) => ({ ...prev, [row.id]: true }));
    try {
      // 整条覆盖式更新：把现有字段原样带上，只翻转 autoStart
      const saved = await tunnelConfigUpdate(config.id, { ...config, autoStart: wanted });
      setRows((prev) =>
        prev.map((item) => (item.id === saved.id ? { ...item, config: saved } : item)),
      );
      onNotify(`已${wanted ? "开启" : "关闭"}自动启动：${row.name || row.id}`);
    } catch (err) {
      setBanner({ tone: "error", text: errorMessage(err) });
    } finally {
      setAutoPending((prev) => {
        const rest = { ...prev };
        delete rest[row.id];
        return rest;
      });
    }
  };

  /**
   * 全部启动。
   *
   * 参数：无
   * 核心流程：串行逐条启动，单条失败只记下来不打断后面的，最后汇总成一条提示；
   * 串行还能让失败原因指向具体名字，也避免同时建多条连接。
   * 整批启动前先问一次父层要不要解锁凭据库：要的话一次性把这批 id 交出去，
   * 父层弹一次解锁框并接管全部启动，本函数直接收尾 —— 既不会每条弹一次，
   * 也不会给被拦下的行留下「启动中…」的过渡态（此时还没 mark 任何行）。
   */
  const startAll = async () => {
    const targets = rows.filter((row) => row.config !== null && !row.running);
    if (targets.length === 0) {
      setBanner({ tone: "info", text: "没有需要启动的隧道" });
      return;
    }
    setBanner(null);
    // 先问解锁再上过渡态：被拦下时列表已被解锁框顶掉，挂着的「启动中…」没人会去复原
    if (await needsVaultBeforeStart(targets.map((row) => row.id))) return;
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
  /** 关键字过滤后的行：关键字为空白时与 rows 完全一致 */
  const filtered = rows.filter((row) => matchRow(row, keyword));
  /** 条数文案：有关键字时给出匹配数，工具栏与分页条共用这一套口径 */
  const totalText =
    keyword.trim() === ""
      ? `共 ${rows.length} 条`
      : `匹配 ${filtered.length} / ${rows.length} 条`;
  /** 工具栏计数：在条数后面补上运行中条数 */
  const countText = `${totalText} · 运行中 ${runningCount} 条`;
  /** 总页数：至少 1 页，让只有一页时分页控件也照常显示 */
  const pageCount = Math.max(1, Math.ceil(filtered.length / pageSize));
  /** 夹取后的当前页：删行或加过滤条件后数据变少，不会停在越界页码上 */
  const current = Math.min(page, pageCount);
  /** 当前页要渲染的行 */
  const pageRows = filtered.slice((current - 1) * pageSize, current * pageSize);

  // 让 page 跟随夹取结果收敛：数据变少时把旧页码写回，避免数据回升后跳回旧页。
  // 本 effect 必须排在下面的「回到第 1 页」之前：改关键字时两者会同批更新，后者生效才落到第 1 页。
  useEffect(() => {
    if (page !== current) setPage(current);
  }, [page, current]);

  // 关键字或每页条数变了就回到第 1 页：只依赖这两项，不会每次渲染都触发
  useEffect(() => {
    setPage(1);
  }, [keyword, pageSize]);

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
                {loading ? "读取中…" : countText}
              </span>
              <div className="tree-search tunnel-search">
                <span className="tree-search-icon">
                  <IconSearch size={13} />
                </span>
                <input
                  value={keyword}
                  placeholder="搜索名称 / 会话 / 监听 / 目标"
                  aria-label="搜索隧道"
                  onChange={(event) => setKeyword(event.target.value)}
                />
              </div>
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
            ) : filtered.length === 0 ? (
              <div className="tunnel-empty">
                <p>没有匹配「{keyword.trim()}」的隧道</p>
              </div>
            ) : (
              // 表格与分页条是兄弟节点，用一个片段包起来
              <>
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
                  {pageRows.map((row) => {
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
                          {/* 名称收紧 + 自动启动开关：开关 flex:none，名字超长仍按 ellipsis 截断，
                              不新增列也不改变行高；两态只换颜色与边框，字号/内边距/文字都不变，
                              所以整个胶囊宽度不变，列宽不会被带动 */}
                          <span className="tunnel-name-wrap">
                            <span className="tunnel-name" title={row.name || "(未命名)"}>
                              {row.name === "" ? "(未命名)" : row.name}
                            </span>
                            {row.config !== null && (
                              <button
                                type="button"
                                className={`tunnel-auto-tag is-${
                                  row.config.autoStart === true ? "on" : "off"
                                }`}
                                aria-pressed={row.config.autoStart === true}
                                disabled={autoPending[row.id] === true}
                                title={
                                  row.config.autoStart === true
                                    ? "已开启自动启动，点击关闭"
                                    : "点击开启自动启动（启动应用时自动连接）"
                                }
                                onClick={() => void toggleAutoStart(row)}
                              >
                                自动
                              </button>
                            )}
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
              <div className="tunnel-pager">
                <span className="tunnel-pager-count">{totalText}</span>
                <span className="tunnel-pager-page">
                  第 {current} / {pageCount} 页
                </span>
                <label className="tunnel-pager-size">
                  每页
                  <select
                    value={pageSize}
                    aria-label="每页条数"
                    onChange={(event) => setPageSize(Number(event.target.value))}
                  >
                    {PAGE_SIZES.map((size) => (
                      <option key={size} value={size}>
                        {size}
                      </option>
                    ))}
                  </select>
                </label>
                {/* 两个翻页按钮复用表格行内小按钮的尺寸与配色，不另造一套样式 */}
                <div className="tunnel-row-actions">
                  <button
                    type="button"
                    disabled={current <= 1}
                    onClick={() => setPage(current - 1)}
                  >
                    上一页
                  </button>
                  <button
                    type="button"
                    disabled={current >= pageCount}
                    onClick={() => setPage(current + 1)}
                  >
                    下一页
                  </button>
                </div>
              </div>
              </>
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
