/**
 * 命令面板的数据模型与过滤。
 *
 * 命令都是纯数据 + 一个 run 回调：App 负责把「新建会话」「切标签」这些既有动作
 * 包成 Command 数组丢进来，这里只做匹配与排序，不碰 DOM 也不碰后端，
 * 方便单测，也方便以后换成从别处动态拼命令。
 */

/** 面板里可执行的一条命令 */
export interface Command {
  /** 唯一 id，动态命令需带后缀，如 `session:${sessionId}` */
  id: string;
  /** 中文标题，列表主文案 */
  title: string;
  /** 英文标题，列表副文案 */
  titleEn: string;
  /** 分组中文名，如「会话」/「跳转」/「隧道」 */
  group: string;
  /** 右侧提示：快捷键组合，或一句补充说明 */
  hint?: string;
  /** 额外可匹配词（拼音缩写、别名），只用于过滤，不展示 */
  keywords?: string[];
  /** 真正要干的事；同步抛错或返回 rejected promise 都由组件兜住 */
  run: () => void | Promise<void>;
}

/** 命中的强弱：前缀命中排在子串命中前面 */
const RANK_PREFIX = 2;
const RANK_SUBSTRING = 1;

/**
 * 一条命令在一组字段里的最高命中等级。
 *
 * 参数：command 为待匹配命令，needle 为已小写化的查询串
 * 核心流程：标题、英文标题、分组、关键肩任一个字段前缀命中就是最强，
 * 否则看有没有子串命中；都没有返回 0，调用方直接丢弃。
 */
function matchRank(command: Command, needle: string): number {
  const fields = [command.title, command.titleEn, command.group, ...(command.keywords ?? [])];
  let rank = 0;
  for (const field of fields) {
    if (!field) continue;
    const haystack = field.toLowerCase();
    if (haystack.startsWith(needle)) return RANK_PREFIX;
    if (haystack.includes(needle)) rank = RANK_SUBSTRING;
  }
  return rank;
}

/**
 * 按查询串过滤命令。
 *
 * 参数：commands 为全量命令，query 为用户输入
 * 核心流程：查询串去空白后为空就原样返回全部（保持原顺序）；否则大小写不敏感地
 * 匹配标题/英文标题/分组/关键字，前缀命中的排在前面，同级用原始下标兜底，
 * 保证分组顺序不会因为过滤而跳来跳去。
 */
export function filterCommands(commands: Command[], query: string): Command[] {
  const needle = query.trim().toLowerCase();
  if (needle === "") return [...commands];

  const hits: { command: Command; rank: number; index: number }[] = [];
  commands.forEach((command, index) => {
    const rank = matchRank(command, needle);
    if (rank > 0) hits.push({ command, rank, index });
  });

  hits.sort((a, b) => b.rank - a.rank || a.index - b.index);
  return hits.map((hit) => hit.command);
}
