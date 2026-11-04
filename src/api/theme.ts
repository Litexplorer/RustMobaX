import { invoke } from "@tauri-apps/api/core";

/**
 * 自定义主题的后端契约封装。
 *
 * 参数：见各函数
 * 核心流程：后端把主题当成一条记录存（id / name / kind / payload 字符串），
 * 前端只在导入时把 payload 解析成调色板 -> 后端不做语义校验，
 * 所以长度上限与 JSON 合法性必须由前端先挡住（见 lib/themeImport.ts）。
 */
export type CustomThemeRecord = {
  id: string;
  name: string;
  kind: "dark" | "light";
  /** 主题 JSON 原文，形状见 lib/themeImport.ts 的导入契约 */
  payload: string;
};

/** 保存主题时提交的字段：不带 id 表示新建 */
export type CustomThemeInput = {
  id?: string;
  name: string;
  kind: "dark" | "light";
  payload: string;
};

/** 后端限制：名称最多 64 字节 */
export const THEME_NAME_MAX_BYTES = 64;
/** 后端限制：payload 最多 16384 字节 */
export const THEME_PAYLOAD_MAX_BYTES = 16 * 1024;

/**
 * 列出全部自定义主题。
 *
 * 参数：无
 * 核心流程：直接把后端返回的数组交给调用方，坏 payload 由解析层跳过。
 */
export function themeListCustom(): Promise<CustomThemeRecord[]> {
  return invoke<CustomThemeRecord[]>("theme_list_custom");
}

/**
 * 新建或覆盖一条自定义主题。
 *
 * 参数：theme 为名称、明暗与 payload（带 id 时覆盖已有记录）
 * 核心流程：后端返回落库后的 id -> 调用方用它拼 custom:<id> 并立即切过去。
 */
export function themeSaveCustom(theme: CustomThemeInput): Promise<string> {
  return invoke<string>("theme_save_custom", { theme });
}

/**
 * 删除一条自定义主题。
 *
 * 参数：id 为主题 id（不含 custom: 前缀，用后端返回的原值）
 * 核心流程：返回剩余条数，调用方据此刷新列表；删除在用的主题要另行回落。
 */
export function themeDeleteCustom(id: string): Promise<number> {
  return invoke<number>("theme_delete_custom", { id });
}
