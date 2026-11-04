import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { errorMessage } from "../api/session";
import { useEscapeClose } from "../lib/useEscapeClose";

type AppInfo = { name: string; version: string; os: string; arch: string };

type Props = { onClose: () => void };

/**
 * 关于窗口。
 *
 * 参数：onClose 为关闭回调
 * 核心流程：挂载时向后端要应用元信息 → 展示版本与平台；
 * 拉取失败只降级为提示，不影响关闭。
 */
export function AboutDialog({ onClose }: Props) {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Esc = 关闭
  useEscapeClose(onClose);

  useEffect(() => {
    invoke<AppInfo>("app_info")
      .then(setInfo)
      .catch((reason) => setError(errorMessage(reason)));
  }, []);

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-small" role="dialog" aria-label="关于 RustMobaX">
        <h2>RustMobaX</h2>
        <p className="about-tagline">跨平台远程工作台 · M0 会话管理</p>
        {error ? (
          <p className="form-error">读取应用信息失败：{error}</p>
        ) : info ? (
          <dl className="summary">
            <dt>版本</dt>
            <dd>{info.version}</dd>
            <dt>平台</dt>
            <dd>
              {info.os} / {info.arch}
            </dd>
          </dl>
        ) : (
          <p className="detail-empty">读取中…</p>
        )}
        <div className="modal-actions">
          <button type="button" className="primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
