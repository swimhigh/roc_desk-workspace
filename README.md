# 编程工作区

`roc_desk-workspace` 是 roc_desk 多仓库拆分后的独立工具仓库。

## 用途

打开一个本地文件夹作为"工作区"：文件树浏览与编辑、本地终端、本地 Git 面板
（状态/未暂存改动/历史/提交）。

## 依赖

- `roc_desk-common` 的 `roc_desk_core`（`common-v0.5.0`）——本次为它新增了
  `roc_desk_core::workspace` 模块（"打开一个本地文件夹并记住最近列表"这个内核
  概念，本仓库是它的第一个使用方）。
- `roc_desk-editor`（`v0.2.1`，间接依赖 `roc_desk-explorer` `v0.2.1`）——本地
  文件系统读写/浏览、root-path 键控的符号索引（`editor_symbols_*`）都直接复用
  这个工具库导出的命令，不在本仓库重新实现。
- 独立壳：`standalone` crate，负责生成该工具自己的 EXE。

## 构建 EXE

```powershell
.\build-standalone.ps1
```

默认生成 Release 版本：`bin\roc_desk-workspace.exe`。开发构建使用：

```powershell
.\build-standalone.ps1 -Configuration debug
```

> 已知环境问题：本机装有 360 安全卫士，实时防护会拦截刚编译出的
> `encoding_rs` 构建脚本执行（`拒绝访问 os error 5`），导致 Release 构建在这台
> 机器上失败；Debug 构建（`cargo build --workspace`，不走同一条编译路径）不受
> 影响，已用它完成过完整的命令级冒烟测试。这是本机杀毒软件的误杀，不是代码
> 问题——换一台机器，或者给 `target/` 目录加杀毒软件信任规则后重试 Release
> 构建即可。

## 前端

`src-web/`：自包含的 Vite + React 页面（`npm install && npm run build`，产物
落到 `standalone/dist/`）——打开文件夹 → 左侧文件树 → 纯文本编辑器（无语法
高亮，见下方"未完成部分"）→ 底部终端/Git 面板（xterm.js + `pty_*` 命令；
`git_*` 命令）。没有内嵌 `roc_desk-editor` 导出的 `<EditorPane/>`（Monaco 版
编辑器面板）——那需要给这个包补一条 npm 跨仓库依赖并拉入它相当重的依赖树
（Monaco/pdfjs/mammoth/xlsx），评估后判断风险和收益不成正比，改用一个纯文本
`<textarea/>` 编辑器，足够验证"打开文件夹 → 浏览 → 编辑 → 保存"这条主链路。

## 迁移状态（2026-09-22 完成）

**已完成并通过命令级冒烟测试**：
- 工作区概念（`workspace_list_recent`/`workspace_open_local`/
  `workspace_remove_recent`/`workspace_update_path`）——仅本地文件夹，
  不含远程（SSH/Agent）工作区，因为 `roc_desk-ssh` 的连接池还没有从宿主拆出来。
- 本地终端（`pty_open`/`pty_write`/`pty_resize`/`pty_close`），从宿主
  `src-tauri/src/pty/mod.rs` 原样迁移。
- 本地 Git 面板（`git_is_repo`/`git_status`/`git_diff`/`git_log`/
  `git_current_branch`/`git_commit_file`/`git_commit_paths`）——直接用 argv
  调 `git` 可执行文件，比宿主原来拼 shell 命令字符串的写法更简单，同样只支持
  本地（原版的 SSH/Agent 远程分支被去掉）。
- 符号索引：不在本仓库重新实现，直接依赖 `roc_desk-editor` 导出的
  `editor_symbols_build_index`/`editor_symbols_go_to_definition`/
  `editor_symbols_reindex_file`（root-path 键控，和本工具的"本地文件夹"模型
  天然匹配）。

**明确跳过、未做**（详见 `lib/src/lib.rs` 顶部文档注释）：
- AI 编程助手的多轮 Agent 循环 + 工具定义（宿主 `coding::session`/
  `coding::tools`，约 3500 行）——依赖宿主 `crate::ai`/`crate::agent_llm`
  （Provider 管理、LLM 调用/流式/重试），这部分还没有进 `roc_desk_core`。
  评估后判断这一块工作量太大，会拖垮其余部分，本次未做，也没有顺带迁移
  `crate::ai`/`crate::agent_llm` 底层能力（因为这个工具本身不打算消费它，
  迁移了也验证不了）。
- 远程（SSH/Agent）工作区和远程终端——依赖 `roc_desk-ssh` 的连接池，尚未拆分。
- Skills 压缩包导入（`coding::skills`）、网页抓取（`coding::webfetch`）——
  优先级低于以上内容，本次跳过。
- 文件改动 Diff/撤销暂存（`coding::changes`/`coding::diff`）——这套机制是
  为 AI Agent 提议的改动做 accept/reject/undo 用的，Agent 循环本身没做，
  这次也就没有移植它。
- 内嵌 `<EditorPane/>`（Monaco 编辑器面板）——见上方"前端"一节。

总体方案见：[多仓库拆分计划](https://github.com/swimhigh/roc_desk/blob/main/docs/MULTI_REPO_SPLIT_PLAN.md)。
