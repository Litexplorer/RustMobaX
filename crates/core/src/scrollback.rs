//! 有界回滚缓冲：按单调游标增量读取终端输出。
//!
//! 输出泵每读到一段字节就同时写进这里，上层按「上次拿到的 next」当游标继续拉取。
//! 缓冲满后从头部丢弃最旧的字节，但游标只增不减，
//! 因此消费者能明确区分「还没写到」与「已经被挤掉」。

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 默认回滚容量：256 KiB。
///
/// 无参数，核心流程：普通的整屏输出只有几 KB，256 KiB 能装下几十屏历史，
/// 同时把每个会话的常驻内存钉在一个可预期的小数目上。
pub const DEFAULT_SCROLLBACK_CAPACITY: usize = 256 * 1024;

/// 一次增量读取的结果。
///
/// 无参数，核心流程：`start` / `next` 是会话建立以来的绝对字节序号，
/// 上层拿 `next` 当下一次的游标；`truncated` 为真说明游标指向的字节已被挤掉，
/// `start` 因此被推到缓冲起点。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollbackChunk {
    /// 本次片段在绝对字节流里的起始偏移。
    pub start: u64,
    /// 下一次读取应使用的游标，恒等于 `start + bytes.len()`。
    pub next: u64,
    /// 游标早于缓冲起点时为真，表示中间有字节已被丢弃。
    pub truncated: bool,
    /// 本次片段的内容，最多为调用方要求的字节数。
    pub bytes: Vec<u8>,
}

/// 缓冲内部状态。
///
/// 无参数，核心流程：`bytes` 只保留最近 `capacity` 个字节，
/// `written` 记录累计写入量；两者在同一把锁下更新，保证游标与内容永远对得上。
struct Inner {
    bytes: VecDeque<u8>,
    written: u64,
}

/// 有界回滚缓冲。
///
/// 无参数，核心流程：写入只做「追加 + 从头部裁剪」，读取按绝对游标定位；
/// 全部状态在一把 `Mutex` 后面，读线程与命令层可以并发访问。
pub struct Scrollback {
    capacity: usize,
    inner: Mutex<Inner>,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_SCROLLBACK_CAPACITY)
    }
}

impl Scrollback {
    /// 按指定容量建一个空缓冲。
    ///
    /// 参数：`bytes` 为容量上限（字节），0 会被抬到 1，避免每次写入都立刻清空。
    /// 核心流程：只记下容量，容器随写入自然增长。
    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            capacity: bytes.max(1),
            inner: Mutex::new(Inner {
                bytes: VecDeque::new(),
                written: 0,
            }),
        }
    }

    /// 追加一段输出。
    ///
    /// 参数：`data` 为原始字节。
    /// 核心流程：整段写进队尾 -> `written` 单调累加 -> 超出容量时从队头批量丢弃，
    /// 丢弃量一次算清，避免逐字节弹。
    pub fn push(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().expect("回滚缓冲锁已中毒");
        inner.bytes.extend(data.iter().copied());
        inner.written = inner.written.saturating_add(data.len() as u64);
        let overflow = inner.bytes.len().saturating_sub(self.capacity);
        if overflow > 0 {
            inner.bytes.drain(..overflow);
        }
    }

    /// 从指定游标开始读一段。
    ///
    /// 参数：`cursor` 为绝对游标（上次返回的 `next`），`max` 为本次最多取的字节数。
    /// 核心流程：先算缓冲起点 `written - len`；
    /// 游标早于起点 -> 从起点读并标 `truncated`；
    /// 游标超过 `written` -> 返回空片段且 `start = written`；
    /// 其余情况按偏移切片，最多取 `max` 字节。
    pub fn read(&self, cursor: u64, max: usize) -> ScrollbackChunk {
        let inner = self.inner.lock().expect("回滚缓冲锁已中毒");
        let written = inner.written;
        let oldest = written.saturating_sub(inner.bytes.len() as u64);
        if cursor > written {
            return ScrollbackChunk {
                start: written,
                next: written,
                truncated: false,
                bytes: Vec::new(),
            };
        }
        let truncated = cursor < oldest;
        let start = if truncated { oldest } else { cursor };
        let offset = (start - oldest) as usize;
        let take = max.min(inner.bytes.len() - offset);
        let bytes: Vec<u8> = inner
            .bytes
            .iter()
            .skip(offset)
            .take(take)
            .copied()
            .collect();
        let next = start + bytes.len() as u64;
        ScrollbackChunk {
            start,
            next,
            truncated,
            bytes,
        }
    }

    /// 取累计写入字节数。
    ///
    /// 无参数，核心流程：直接返回单调计数器，
    /// 调用方拿它判断自己是否已经追平最新输出。
    pub fn written(&self) -> u64 {
        self.inner.lock().expect("回滚缓冲锁已中毒").written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 增量读取应衔接无缝，被挤掉的头部要如实标记。
    #[test]
    fn reads_incrementally_and_marks_truncation() {
        let buffer = Scrollback::with_capacity(8);
        buffer.push(b"abcd");
        buffer.push(b"efgh");
        buffer.push(b"ijkl");

        assert_eq!(buffer.written(), 12, "累计写入量应随每次 push 单调累加");

        // 第一次读：游标 0 早于缓冲起点 4 -> 从头给，并标记截断
        let first = buffer.read(0, 4);
        assert_eq!(first.start, 4, "被挤掉后应从缓冲起点开始读");
        assert_eq!(
            first.start,
            buffer.written() - 8,
            "起点应等于 written 减容量"
        );
        assert!(first.truncated, "游标早于缓冲起点时必须标记截断");
        assert_eq!(first.next, first.start + first.bytes.len() as u64);
        assert_eq!(first.bytes, b"efgh");

        // 第二次读：拿上一次的 next 接着读 -> 不重复、不遗漏
        let second = buffer.read(first.next, 4);
        assert_eq!(second.start, first.next, "下一次的起点应等于上一次的 next");
        assert!(!second.truncated, "游标在缓冲范围内不应标记截断");
        assert_eq!(second.bytes, b"ijkl");
        assert_eq!(second.next, second.start + second.bytes.len() as u64);
        assert!(second.next > first.next, "游标必须严格递增");

        let merged = [first.bytes, second.bytes].concat();
        assert_eq!(merged, b"efghijkl", "两次拼接应是无缝的连续片段");
    }

    /// 游标超过已写字节时应给空片段，而不是回退或报错。
    #[test]
    fn cursor_past_written_returns_empty_at_written() {
        let buffer = Scrollback::with_capacity(8);
        buffer.push(b"abcd");
        buffer.push(b"efgh");
        buffer.push(b"ijkl");

        let chunk = buffer.read(100, 16);
        assert_eq!(chunk.start, buffer.written(), "越界游标应回落到 written");
        assert_eq!(chunk.next, buffer.written());
        assert!(chunk.bytes.is_empty(), "还没写到的区域应返回空片段");
        assert!(!chunk.truncated, "越界不算截断");
    }

    /// 容量小于单次推送量时也只保留尾部，written 不受影响。
    #[test]
    fn oversized_push_keeps_only_tail() {
        let buffer = Scrollback::with_capacity(4);
        buffer.push(b"0123456789");

        assert_eq!(buffer.written(), 10, "written 记录的是累计量，不随裁剪回退");
        let chunk = buffer.read(0, 10);
        assert_eq!(chunk.start, 6, "起点应为 written 减容量");
        assert!(chunk.truncated);
        assert_eq!(chunk.bytes, b"6789", "只保留最新的 4 个字节");
    }
}
