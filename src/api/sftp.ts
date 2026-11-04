import { Channel, invoke } from "@tauri-apps/api/core";
import type { SshAuthRequest } from "./terminal";

/** 远端目录里的一项，字段与后端 SftpEntry 一致（camelCase）。 */
export type SftpEntry = {
  name: string;
  /** 完整路径；服务端默认目录下返回的是相对名字。 */
  path: string;
  isDir: boolean;
  size: number;
  /** 权限位，含文件类型位。 */
  permissions: number;
  /** 修改时间（Unix 秒），0 表示服务端没给。 */
  modified: number;
};

/** 建立 SFTP 会话的请求，字段与后端 SftpConnectRequest 对齐。 */
export type SftpConnectRequest = {
  host: string;
  port: number;
  username: string;
  auth: SshAuthRequest;
  /** 用户已在界面上确认过该主机指纹。 */
  acceptUnknownHostKey: boolean;
};

/** 内置编辑器的写入上限：编辑器是整段文本场景，其余传输走流式命令不受此限。 */
export const SFTP_WRITE_LIMIT_BYTES = 2 * 1024 * 1024;

/**
 * 把字节数转成人类可读的大小。
 *
 * 参数：bytes 为字节数
 * 核心流程：按 1024 进制逐级降单位，最多保留一位小数；
 * 目录列表、磁盘占用条、传输进度共用同一口径，避免一屏两种单位。
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "-";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = bytes / 1024;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${value.toFixed(1)} ${units[index]}`;
}

/**
 * 打开一条独立的 SFTP 连接。
 *
 * 参数：request 为连接参数
 * 核心流程：由后端新建 SSH 连接并开 sftp 子系统，返回会话标识；
 * 与 shell 会话不共享标识，关标签时各自关各自的。
 */
export function sftpConnect(request: SftpConnectRequest): Promise<string> {
  return invoke<string>("sftp_connect", { request });
}

/**
 * 列出一个远端目录。
 *
 * 参数：id 为会话标识，path 为空串表示服务端默认目录（家目录）
 * 核心流程：目录已按「目录在前、同名次按名称」排好序，前端不再重排。
 */
export function sftpList(id: string, path: string): Promise<SftpEntry[]> {
  return invoke<SftpEntry[]>("sftp_list", { id, path });
}

/**
 * 以文本方式读取远端文件。
 *
 * 参数：id 为会话标识，path 为远端文件路径
 * 核心流程：后端超过 1 MiB、含 NUL 或非 UTF-8 都会报错，
 * 面板据此提示「不是文本文件」而不是硬塞乱码。
 */
export function sftpReadText(id: string, path: string): Promise<string> {
  return invoke<string>("sftp_read_text", { id, path });
}

/**
 * 以文本方式覆盖写远端文件。
 *
 * 参数：id 为会话标识，path 为目标路径，content 为文本内容
 * 核心流程：整段覆盖，不做换行转换；字节数上限由调用方先校验。
 */
export function sftpWriteText(id: string, path: string, content: string): Promise<void> {
  return invoke("sftp_write_text", { id, path, content });
}

/**
 * 新建远端目录。
 *
 * 参数：id 为会话标识，path 为目录路径
 * 核心流程：父目录不存在时由服务端报错，面板原样提示。
 */
export function sftpMkdir(id: string, path: string): Promise<void> {
  return invoke("sftp_mkdir", { id, path });
}

/**
 * 删除远端文件或空目录。
 *
 * 参数：id 为会话标识，path 为远端路径，isDir 表示按目录删除
 * 核心流程：目录走 RMDIR，非空目录会被服务端拒绝。
 */
export function sftpRemove(id: string, path: string, isDir: boolean): Promise<void> {
  return invoke("sftp_remove", { id, path, isDir });
}

/**
 * 重命名或移动远端条目。
 *
 * 参数：id 为会话标识，from 为原路径，to 为新路径
 * 核心流程：同目录改名与跨目录移动同一条命令，目标已存在时由服务端决定覆盖与否。
 */
export function sftpRename(id: string, from: string, to: string): Promise<void> {
  return invoke("sftp_rename", { id, from, to });
}

/**
 * 关闭 SFTP 会话。
 *
 * 参数：id 为会话标识
 * 核心流程：后端幂等，重复调用不报错，供面板卸载时兜底清理。
 */
export function sftpClose(id: string): Promise<void> {
  return invoke("sftp_close", { id });
}

/** 后端推来的传输事件，与契约里的 TransferEvent 一一对应。 */
export type TransferEvent =
  | { kind: "progress"; bytes: number }
  | { kind: "done"; bytes: number }
  | { kind: "error"; message: string };

/** 一次传输结束后的账目，用来在界面上说清「传了多少、从哪续上的」。 */
export type TransferOutcome = {
  /** 本次实际写入的字节数 */
  written: number;
  /** 后端认为需要写入的总字节数，0 表示它也不知道（例如对端没给大小） */
  total: number;
  /** 本次是从目标文件的第几个字节接着写的，0 表示从头写的 */
  resumedFrom: number;
};

/** 传输选项：resume 为 true 时后端会接着 .rmxpart 断点续传。 */
export type TransferOptions = {
  /** 目标旁已有 .rmxpart 残留时接着传，失败重试与「继续传输」都置 true */
  resume?: boolean;
  /** 每收到一条后端事件就回调一次，进度条据此刷新 */
  onEvent?: (event: TransferEvent) => void;
};

/**
 * 传输句柄：cancelToken 同步可用，done 才是最终结果。
 *
 * 之所以不直接返回 Promise，是因为取消必须在传输进行中就能拿到 token；
 * 并发池也是靠这个句柄记住在途任务，取消时逐个点掉。
 */
export type TransferHandle = {
  cancelToken: string;
  done: Promise<TransferOutcome>;
};

/**
 * 生成一个取消令牌。
 *
 * 参数：无
 * 核心流程：优先用 crypto.randomUUID；老 WebView 没有这个方法时退回
 * 时间戳加随机数，保证同一会话内不重复即可，不当作安全随机数使用。
 */
function newCancelToken(): string {
  const cryptoApi = globalThis.crypto as Crypto | undefined;
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return cryptoApi.randomUUID();
  }
  return `rmx-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * 起一次流式传输，两个方向共用这一份封装。
 *
 * 参数：command 为后端命令名，id 为会话标识，remotePath / localPath 为两端路径，
 * options 带 resume 与 onEvent
 * 核心流程：本地生成 cancelToken -> 建 Channel<TransferEvent> 并把调用方的
 * onEvent 挂上去（必须在 invoke 之前挂，后端可能立刻开始发事件）->
 * 把 token 与 channel 一起交给后端 -> 同步返回句柄，调用方既能立刻拿到 token
 * 去取消，也能 await done 拿最终账目。
 */
function startTransfer(
  command: "sftp_upload_local_file" | "sftp_download_remote_file",
  id: string,
  remotePath: string,
  localPath: string,
  options: TransferOptions,
): TransferHandle {
  const cancelToken = newCancelToken();
  const onEvent = new Channel<TransferEvent>();
  if (options.onEvent) onEvent.onmessage = options.onEvent;
  const done = invoke<TransferOutcome>(command, {
    id,
    remotePath,
    localPath,
    resume: options.resume ?? false,
    cancelToken,
    onEvent,
  });
  return { cancelToken, done };
}

/**
 * 把一个本地文件流式上传到远端。
 *
 * 参数：id 为会话标识，remotePath 为远端目标文件路径，localPath 为本机文件路径，
 * options 带 resume 与进度回调
 * 核心流程：后端先写 <目标>.rmxpart，传完再改名成目标，因此中途失败会留下残留；
 * resume 为 true 且残留存在时就接着传，否则从头开始。
 */
export function sftpUploadLocalFile(
  id: string,
  remotePath: string,
  localPath: string,
  options: TransferOptions = {},
): TransferHandle {
  return startTransfer("sftp_upload_local_file", id, remotePath, localPath, options);
}

/**
 * 把一个远端文件流式下载到本地。
 *
 * 参数：id 为会话标识，remotePath 为远端文件路径，localPath 为本机落盘路径，
 * options 带 resume 与进度回调
 * 核心流程：与上传对称，先落 .rmxpart 再改名，断点续传规则一致；
 * 内容同样不过前端，因此不限大小。
 */
export function sftpDownloadRemoteFile(
  id: string,
  remotePath: string,
  localPath: string,
  options: TransferOptions = {},
): TransferHandle {
  return startTransfer("sftp_download_remote_file", id, remotePath, localPath, options);
}

/**
 * 取消一次正在进行的传输。
 *
 * 参数：cancelToken 为 startTransfer 生成、随请求发给后端的令牌
 * 核心流程：后端收到后让拷贝循环退出并返回取消错误；前端不做清理，
 * .rmxpart 由后端保留，供下一次 resume=true 继续。
 */
export function sftpCancelTransfer(cancelToken: string): Promise<void> {
  return invoke("sftp_cancel_transfer", { cancelToken });
}

/** 远端磁盘占用，字段与后端 RemoteDisk 一致。 */
export type RemoteDisk = {
  /** 已用字节数 */
  usedBytes: number;
  /** 总容量字节数 */
  totalBytes: number;
  /** 剩余字节数 */
  freeBytes: number;
  /** 使用率，0 到 100 的整数 */
  percent: number;
  /** 该路径所在的挂载点，用于在 title 里说明统计的是哪个分区 */
  mount: string;
};

/**
 * 取某个远端路径所在分区的磁盘占用。
 *
 * 参数：terminalId 为 SSH 终端标识（不是 SFTP 会话标识），path 为远端路径
 * 核心流程：后端登录该终端所在的 SSH 连接上跑 df，因此终端没连上就会抛错；
 * 面板据此整行隐藏，绝不显示猜出来的数字。
 */
export function remoteDisk(terminalId: string, path: string): Promise<RemoteDisk> {
  return invoke<RemoteDisk>("remote_disk", { terminalId, path });
}

/** 本地目录里的一项（递归上传用）。 */
export type LocalEntry = {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
};

/** 列出本地目录，用于整目录上传。 */
export function listLocalDir(path: string): Promise<LocalEntry[]> {
  return invoke<LocalEntry[]>("list_local_dir", { path });
}
