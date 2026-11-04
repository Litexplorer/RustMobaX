import { invoke } from "@tauri-apps/api/core";

/**
 * 一个本机字体族。
 *
 * name 是字族名（如 "JetBrains Mono"），monospaced 来自字体自身的等宽标记，
 * 界面据此把等宽字体排在候选列表前面。
 */
export type FontFamily = {
  name: string;
  monospaced: boolean;
};

/**
 * 枚举本机安装的字体族。
 *
 * 参数：无
 * 核心流程：后端扫一次字体目录并用 OnceLock 缓存，之后每次调用都读缓存；
 * 读不到字体时返回空数组而不报错，前端退化成「手输字体名」模式。
 */
export function systemFonts(): Promise<FontFamily[]> {
  return invoke<FontFamily[]>("system_fonts");
}
