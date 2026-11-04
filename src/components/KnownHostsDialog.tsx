import { useCallback, useEffect, useState } from "react";
import { errorMessage } from "../api/session";
import { forgetHostKey, listKnownHosts } from "../api/terminal";
import type { KnownHost } from "../api/terminal";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  /** 删除成功后的状态栏提示文案 */
  onNotify: (text: string) => void;
  onClose: () => void;
};

/**
 * 已知主机指纹管理弹窗。
 *
 * 参数：见 Props
 * 核心流程：打开即拉一次列表，支持手动刷新与逐条删除；
 * 删除成功后重新拉列表，让用户看到库里真实的样子。
 */
export function KnownHostsDialog({ onNotify, onClose }: Props) {
  const [hosts, setHosts] = useState<KnownHost[]>([]);
  const [loading, setLoading] = useState(true);
  /** 正在删除的记录键，避免连点同一行走两次删除 */
  const [deleting, setDeleting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Esc = 关闭（只读列表，没有未保存内容，无需二次确认）
  useEscapeClose(onClose);

  /**
   * 拉取已记录的主机指纹。
   *
   * 参数：无
   * 核心流程：失败只把原因显示在弹窗里，不清空上一次拿到的列表。
   */
  const load = useCallback(async () => {
    setLoading(true);
    try {
      setHosts(await listKnownHosts());
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /**
   * 删除一条主机指纹。
   *
   * 参数：target 为要删除的记录
   * 核心流程：按 host + port 调后端删除，成功后提示并重新拉列表。
   */
  const remove = async (target: KnownHost) => {
    const key = `${target.host}:${target.port}`;
    setDeleting(key);
    setError(null);
    try {
      await forgetHostKey(target.host, target.port);
      onNotify(`已删除 ${key} 的主机指纹`);
      await load();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setDeleting(null);
    }
  };

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-wide" role="dialog" aria-label="已知主机">
        <div className="dialog-header">
          <h2>已知主机</h2>
          <button type="button" className="dialog-close" aria-label="关闭" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="dialog-body">
          <p className="detail-empty">
            这里记录的是连接时核对过的服务端指纹。删除后再次连接该主机，会重新要求确认指纹。
          </p>

          <div className="modal-actions">
            <button type="button" onClick={() => void load()} disabled={loading || deleting !== null}>
              {loading ? "刷新中…" : "刷新"}
            </button>
          </div>

          {hosts.length === 0 ? (
            <p className="detail-empty">{loading ? "正在读取…" : "还没有记录任何主机指纹"}</p>
          ) : (
            <table className="known-host-table">
              <thead>
                <tr>
                  <th style={{ width: "26%" }}>主机</th>
                  <th style={{ width: "64px" }}>端口</th>
                  <th style={{ width: "120px" }}>密钥类型</th>
                  <th>指纹</th>
                  <th style={{ width: "64px" }} />
                </tr>
              </thead>
              <tbody>
                {hosts.map((item) => {
                  const key = `${item.host}:${item.port}`;
                  return (
                    <tr key={`${key}:${item.keyType}`}>
                      <td title={item.host}>{item.host}</td>
                      <td>{item.port}</td>
                      <td title={item.keyType}>{item.keyType}</td>
                      <td className="fingerprint-cell" title={item.fingerprint}>
                        {item.fingerprint}
                      </td>
                      <td className="row-actions">
                        <button
                          type="button"
                          className="danger"
                          disabled={deleting !== null}
                          onClick={() => void remove(item)}
                        >
                          {deleting === key ? "删除中…" : "删除"}
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}

          {error && <p className="form-error">{error}</p>}
        </div>

        <div className="dialog-footer">
          <button type="button" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
