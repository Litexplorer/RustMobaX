/**
 * 一个极小的文件级并发池。
 *
 * 参数：items 为待处理条目，limit 为同时在途的上限，worker 为单条目处理函数
 * 核心流程：先派满 limit 个，任何一个结束就补下一个；一旦有条目失败，
 * 立刻停止派发新条目，等在途条目全部结束，再把第一个失败的条目交回调用方。
 * 只做「限流 + 失败即止」两件事，所以不引第三方库。
 */

/** 池的收尾情况：失败了谁，以及是否提前停下 */
export type PoolResult<T> = {
  /** 第一个失败的条目，全部成功时为 null */
  failed: T | null;
  /** 是否因为失败或外部停止而没有把条目跑完 */
  stopped: boolean;
};

/** 派发前的额外判断：返回 true 就不再派发新条目 */
export type PoolOptions = {
  /**
   * 每次准备派发时问一次。
   * 用于「取消后不再派发」：外层把标志位置 true，池就会自然收敛。
   */
  shouldStop?: () => boolean;
};

/**
 * 用固定并发上限跑完一批条目。
 *
 * 参数：items 为条目数组，limit 为并发上限（小于 1 时按 1 处理），
 * worker 为单条目处理函数（拿得到下标，便于区分「第一个要续传的」），
 * options.shouldStop 可在外部叫停派发
 * 核心流程：用 next 记住下一个待派发下标、active 记住在途数量，
 * 每 settle 一个就调一次 pump 补位；失败只记第一个，之后只等在途结束，
 * 因此不会出现「一个文件失败还在继续派发剩下的」这种浪费。
 */
export function runPool<T>(
  items: readonly T[],
  limit: number,
  worker: (item: T, index: number) => Promise<void>,
  options: PoolOptions = {},
): Promise<PoolResult<T>> {
  const width = Math.max(1, Math.floor(limit));
  return new Promise<PoolResult<T>>((resolve) => {
    if (items.length === 0) {
      resolve({ failed: null, stopped: false });
      return;
    }
    /** 下一个待派发的条目下标 */
    let next = 0;
    /** 在途条目数量 */
    let active = 0;
    /** 第一个失败的条目 */
    let failed: T | null = null;
    /** 是否已经收尾，避免重复 resolve */
    let settled = false;

    const finish = (stopped: boolean) => {
      if (settled) return;
      settled = true;
      resolve({ failed, stopped });
    };

    const pump = () => {
      if (settled) return;
      // 失败或外部叫停：不再派发，等在途的清空再收尾
      if (failed !== null || options.shouldStop?.()) {
        if (active === 0) finish(true);
        return;
      }
      while (active < width && next < items.length) {
        const index = next;
        const item = items[index];
        next += 1;
        active += 1;
        worker(item, index)
          .then(() => {
            active -= 1;
            pump();
          })
          .catch(() => {
            active -= 1;
            // 只记第一个失败，原因由 worker 自己在外面记着
            if (failed === null) failed = item;
            pump();
          });
      }
      if (active === 0 && next >= items.length) finish(false);
    };

    pump();
  });
}
