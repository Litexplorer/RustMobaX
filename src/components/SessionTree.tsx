import { useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";
import type { Session, SessionGroup } from "../api/session";
import { compareGroup } from "../lib/tree";
import {
  IconChevronDown,
  IconChevronRight,
  IconFolder,
  IconServer,
  IconTerminal,
} from "./icons";

type Props = {
  groups: SessionGroup[];
  sessions: Session[];
  collapsed: Record<string, boolean>;
  selectedGroupId: string | null;
  selectedSessionId: string | null;
  onToggleGroup: (id: string) => void;
  onSelectGroup: (id: string) => void;
  onOpenSession: (session: Session) => void;
  onRenameGroup: (group: SessionGroup) => void;
  onDeleteGroup: (group: SessionGroup) => void;
  onEditSession: (session: Session) => void;
  onCopySession: (session: Session) => void;
  onDeleteSession: (session: Session) => void;
  onMoveSession: (sessionId: string, groupId: string | null) => void;
  onMoveGroup: (groupId: string, parentId: string | null) => void;
};

type DragPayload = { kind: "session" | "group"; id: string };

const MOVE_THRESHOLD = 6;

/**
 * 渲染左侧会话树。
 *
 * 参数：见 Props
 * 核心流程：按 parentId 归拢子分组与会话后递归展开；拖拽用指针事件自己实现，
 * 因为 Tauri 的 dragDropEnabled 会拦截 DOM 原生拖放，指针方案两边都不冲突。
 */
export function SessionTree(props: Props) {
  const {
    groups,
    sessions,
    collapsed,
    selectedGroupId,
    selectedSessionId,
    onToggleGroup,
    onSelectGroup,
    onOpenSession,
    onRenameGroup,
    onDeleteGroup,
    onEditSession,
    onCopySession,
    onDeleteSession,
    onMoveSession,
    onMoveGroup,
  } = props;

  const [dragging, setDragging] = useState<DragPayload | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const dropTargetRef = useRef<string | null>(null);
  const startPointRef = useRef<{ x: number; y: number } | null>(null);
  const movedRef = useRef(false);
  const suppressClickRef = useRef(false);

  // 拖拽期间接管指针 -> 用 elementFromPoint 判断落点，避免依赖原生 DnD
  useEffect(() => {
    if (!dragging) return undefined;

    const handleMove = (event: PointerEvent) => {
      const start = startPointRef.current;
      if (start) {
        const distance =
          Math.abs(event.clientX - start.x) + Math.abs(event.clientY - start.y);
        if (distance > MOVE_THRESHOLD) movedRef.current = true;
      }
      if (!movedRef.current) return;

      const element = document.elementFromPoint(event.clientX, event.clientY);
      const groupElement =
        element instanceof Element ? element.closest("[data-group-id]") : null;
      const target = groupElement?.getAttribute("data-group-id") ?? null;
      dropTargetRef.current = target;
      setDropTarget(target);
    };

    const handleUp = () => {
      const moved = movedRef.current;
      const target = dropTargetRef.current;
      if (moved) {
        // 拖拽结束后短暂吞掉后续 click -> 避免顺带打开标签
        suppressClickRef.current = true;
        window.setTimeout(() => {
          suppressClickRef.current = false;
        }, 0);
        if (dragging.kind === "session") {
          onMoveSession(dragging.id, target);
        } else {
          onMoveGroup(dragging.id, target);
        }
      }
      setDragging(null);
      setDropTarget(null);
      dropTargetRef.current = null;
      startPointRef.current = null;
      movedRef.current = false;
    };

    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", handleUp);
    return () => {
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", handleUp);
    };
  }, [dragging, onMoveSession, onMoveGroup]);

  /**
   * 记录拖拽起点。
   *
   * 参数：event 为指针按下事件，payload 为被拖对象
   * 核心流程：只记坐标与负载，同时清掉上一次遗留的落点与移动标记，
   * 避免残留状态让下一次单击被误判成拖拽。
   */
  const beginDrag = (event: ReactPointerEvent, payload: DragPayload) => {
    if (event.button !== 0) return;
    startPointRef.current = { x: event.clientX, y: event.clientY };
    movedRef.current = false;
    dropTargetRef.current = null;
    setDropTarget(null);
    setDragging(payload);
  };

  const rootGroups = groups.filter((group) => group.parentId === null).sort(compareGroup);
  const rootSessions = sessions.filter((session) => session.groupId === null);

  const renderSession = (session: Session, depth: number) => (
    <div
      key={session.id}
      className={`tree-row session-item ${selectedSessionId === session.id ? "is-active" : ""} ${
        dragging?.kind === "session" && dragging.id === session.id ? "is-dragging" : ""
      }`}
      style={{ paddingLeft: 8 + depth * 14 }}
      onPointerDown={(event) => beginDrag(event, { kind: "session", id: session.id })}
      onClick={() => {
        if (suppressClickRef.current) return;
        onOpenSession(session);
      }}
      title={session.host ?? "本地终端"}
    >
      <span className={`session-icon protocol-${session.protocol}`}>
        {session.protocol === "sftp" ? (
          <IconFolder size={15} />
        ) : session.protocol === "ssh" ? (
          <IconServer size={15} />
        ) : (
          <IconTerminal size={15} />
        )}
      </span>
      <span className="session-name">{session.name}</span>
      <span className="session-target">{session.host ?? "local"}</span>
      <div className="row-actions" onClick={(event) => event.stopPropagation()}>
        <button type="button" onClick={() => onEditSession(session)}>
          编辑
        </button>
        <button type="button" onClick={() => onCopySession(session)}>
          复制
        </button>
        <button type="button" className="danger" onClick={() => onDeleteSession(session)}>
          删除
        </button>
      </div>
    </div>
  );

  const renderGroup = (group: SessionGroup, depth: number): ReactNode => {
    const childGroups = groups.filter((item) => item.parentId === group.id).sort(compareGroup);
    const childSessions = sessions.filter((session) => session.groupId === group.id);
    const isCollapsed = collapsed[group.id] === true;
    const empty = childGroups.length === 0 && childSessions.length === 0;
    const isDropTarget = dropTarget === group.id;

    return (
      <div key={group.id} className="group-node">
        <div
          className={`tree-row group-row ${selectedGroupId === group.id ? "is-active" : ""} ${
            isDropTarget ? "is-drop-target" : ""
          } ${dragging?.kind === "group" && dragging.id === group.id ? "is-dragging" : ""}`}
          style={{ paddingLeft: 6 + depth * 14 }}
          data-group-id={group.id}
          onPointerDown={(event) => beginDrag(event, { kind: "group", id: group.id })}
          onClick={() => {
            if (suppressClickRef.current) return;
            onSelectGroup(group.id);
          }}
        >
          <span
            className="group-toggle"
            onClick={(event) => {
              event.stopPropagation();
              onToggleGroup(group.id);
            }}
          >
            {isCollapsed ? <IconChevronRight size={12} /> : <IconChevronDown size={12} />}
          </span>
          <span className="group-icon">
            <IconFolder size={15} />
          </span>
          <span className="group-name">{group.name}</span>
          <span className="group-count">{childGroups.length + childSessions.length}</span>
          <div className="row-actions" onClick={(event) => event.stopPropagation()}>
            <button type="button" onClick={() => onRenameGroup(group)}>
              重命名
            </button>
            <button type="button" className="danger" onClick={() => onDeleteGroup(group)}>
              删除
            </button>
          </div>
        </div>
        {!isCollapsed && (
          <div className="group-children">
            {childGroups.map((child) => renderGroup(child, depth + 1))}
            {childSessions.map((session) => renderSession(session, depth + 1))}
            {empty && (
              <div className="empty-hint" style={{ paddingLeft: 28 + depth * 14 }}>
                空分组
              </div>
            )}
          </div>
        )}
      </div>
    );
  };

  if (groups.length === 0 && sessions.length === 0) {
    return (
      <div className="tree-empty">
        还没有会话
        <br />
        点工具栏「新建会话」，或从 MobaXterm 导入
      </div>
    );
  }

  return (
    <div className="session-tree">
      {rootGroups.map((group) => renderGroup(group, 0))}
      {rootSessions.map((session) => renderSession(session, 0))}
      <div className={`tree-drop-root ${dragging ? "is-active" : ""}`}>
        拖到此处可移到未分组
      </div>
    </div>
  );
}
