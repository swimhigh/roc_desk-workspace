import React from "react";
import ReactDOM from "react-dom/client";
// `@roc_desk/tool-editor` 的 side-effecting 导入：注册 Monaco 的本地 worker 配置、
// `makefile`/`log` 语言、以及 `<CodeEditor/>` 按名字请求的 `roc-dark`/`roc-light`
// 主题——不 import 一次的话 Monaco 会静默回退成内置浅色 `vs` 主题，即使外壳其余
// 部分已经是深色（`roc_desk-editor`/`roc_desk-explorer` 都踩过这个坑，见各自
// 仓库 2026-09-23 的主题修复记录）。
import "@roc_desk/tool-editor";
import { App } from "./App";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
