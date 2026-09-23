import { create } from "zustand";

interface ModalStackState {
  /** 当前处于 open 状态的 ConfirmDialog 系弹窗数量（可能同时叠多层）。 */
  count: number;
  push: () => void;
  pop: () => void;
}

/**
 * 全局弹窗计数：原生子 WebView（如果宿主/嵌入方有）不参与 CSS 层叠，任何 HTML
 * 弹窗打开时都必须显式隐藏它，否则弹窗会被原生内容盖住。ConfirmDialog 在 open
 * 时 push、关闭/卸载时 pop，需要这个信号的原生内容面板订阅 count>0 来临时隐藏。
 */
export const useModalStackStore = create<ModalStackState>((set) => ({
  count: 0,
  push: () => set((s) => ({ count: s.count + 1 })),
  pop: () => set((s) => ({ count: Math.max(0, s.count - 1) })),
}));
