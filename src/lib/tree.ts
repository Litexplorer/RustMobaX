import type { Session, SessionGroup } from "../api/session";

/**
 * 求分组从根到自身的名称链。
 *
 * 参数：groups 为全部分组，id 为目标分组
 * 核心流程：沿 parentId 向上回溯后反转，找不到即返回空数组。
 */
export function groupPath(groups: SessionGroup[], id: string | null): string[] {
  const byId = new Map(groups.map((group) => [group.id, group]));
  const path: string[] = [];
  let current = id;
  let guard = 0;
  while (current && guard < 64) {
    const group = byId.get(current);
    if (!group) break;
    path.unshift(group.name);
    current = group.parentId;
    guard += 1;
  }
  return path;
}

/**
 * 把分组树摊平成带完整路径的下拉选项。
 *
 * 参数：groups 为全部分组
 * 核心流程：逐个求名称链并以 " / " 连接，按路径字典序输出便于阅读。
 */
export function flattenGroups(groups: SessionGroup[]): { id: string; label: string }[] {
  return groups
    .map((group) => ({ id: group.id, label: groupPath(groups, group.id).join(" / ") }))
    .sort((a, b) => a.label.localeCompare(b.label, "zh-CN"));
}

/**
 * 按同级顺序给分组排序。
 *
 * 参数：a、b 为待比较分组
 * 核心流程：先比 sortOrder，再按名称兜底，保证渲染顺序稳定。
 */
export function compareGroup(a: SessionGroup, b: SessionGroup): number {
  if (a.sortOrder !== b.sortOrder) return a.sortOrder - b.sortOrder;
  return a.name.localeCompare(b.name, "zh-CN");
}

/**
 * 判断某个分组是否还有其他分组挂在它下面。
 *
 * 参数：groups 为全部分组，id 为目标分组
 * 核心流程：直接看是否有分组的 parentId 指向它，用于删除前提示。
 */
export function hasChildGroups(groups: SessionGroup[], id: string): boolean {
  return groups.some((group) => group.parentId === id);
}

/**
 * 首界面搜索框的关键字过滤。
 *
 * 参数：sessions 为待搜索会话（返回值保持它的原始顺序），groups 为全部分组
 * （用来把 groupId 还原成可搜索的分组全路径），query 为用户输入
 * 核心流程：关键字去首尾空白并转小写后，对名称、主机、用户名、端口、协议、
 * 分组全路径逐一做子串匹配，任一命中即保留；空关键字直接返回空数组，
 * 让首界面一进来不铺满整屏会话；不做排序，结果顺序由调用方传入的顺序决定。
 */
export function filterSessions(
  sessions: Session[],
  groups: SessionGroup[],
  query: string,
): Session[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [];

  return sessions.filter((session) => {
    const fields = [
      session.name,
      session.host ?? "",
      session.username ?? "",
      session.port === null ? "" : String(session.port),
      session.protocol,
      groupPath(groups, session.groupId).join(" / "),
    ];
    return fields.some((field) => field.toLowerCase().includes(needle));
  });
}
