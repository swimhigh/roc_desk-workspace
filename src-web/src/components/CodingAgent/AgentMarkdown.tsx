import React, { useMemo } from "react";
import { renderMarkdown } from "../../utils/markdown";
import { highlightShellCommand } from "../../utils/shellHighlight";

const SHELL_LANGS = new Set(["bash", "sh", "shell", "zsh", "console", "powershell", "ps1", "cmd", "batch"]);

interface AgentMarkdownProps {
  content: string;
  onOpenFile?: (path: string, line?: number) => void;
}

const FILE_REF = /^(.*?\.(?:rs|ts|tsx|js|jsx|json|md|css|scss|html|toml|yaml|yml|sql|py|go|java|kt|sh|ps1|xml))(?:[:#]L?(\d+))?$/i;

function parseFileRef(value: string): { path: string; line?: number } | null {
  const cleaned = value.trim().replace(/^file:\/\//, "").replace(/^['"`]|['"`]$/g, "").replace(/[),.;。，；）】》]+$/g, "");
  const match = cleaned.match(FILE_REF);
  if (!match || /^https?:\/\//i.test(cleaned)) return null;
  return { path: match[1].replace(/\\/g, "/"), line: match[2] ? Number(match[2]) : undefined };
}

function decorateFileReferences(html: string): string {
  const document = new DOMParser().parseFromString(`<div id="agent-md-root">${html}</div>`, "text/html");
  const root = document.getElementById("agent-md-root");
  if (!root) return html;

  // Markdown doesn't turn a plain-text path into a link (e.g. a model
  // commonly outputs `D:\\code\\project\\docs\\plan.md`) -- convert file
  // references found in text nodes into `<a>` so absolute/relative paths
  // with a known extension can also reuse onOpenFile to open the editor.
  const pathPattern = /(?:[A-Za-z]:[\\/]|\\\\|\.{0,2}[\\/]|\/)[^\s<>"'`]+?\.(?:rs|ts|tsx|js|jsx|json|md|css|scss|html|toml|yaml|yml|sql|py|go|java|kt|sh|ps1|xml|vue|svelte|c|cpp|h|hpp)(?::#?L?\d+)?/gi;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const textNodes: Text[] = [];
  let current: Node | null;
  while ((current = walker.nextNode())) textNodes.push(current as Text);
  for (const textNode of textNodes) {
    if (textNode.parentElement?.closest("a, code, pre")) continue;
    const text = textNode.nodeValue || "";
    pathPattern.lastIndex = 0;
    if (!pathPattern.test(text)) continue;
    pathPattern.lastIndex = 0;
    const fragment = document.createDocumentFragment();
    let cursor = 0;
    for (const match of text.matchAll(pathPattern)) {
      const value = match[0];
      const index = match.index ?? 0;
      if (index > cursor) fragment.appendChild(document.createTextNode(text.slice(cursor, index)));
      const anchor = document.createElement("a");
      anchor.href = value;
      anchor.textContent = value;
      anchor.className = "agent-file-ref";
      fragment.appendChild(anchor);
      cursor = index + value.length;
    }
    if (cursor < text.length) fragment.appendChild(document.createTextNode(text.slice(cursor)));
    textNode.parentNode?.replaceChild(fragment, textNode);
  }

  root.querySelectorAll("a, code").forEach((element) => {
    const raw = element instanceof HTMLAnchorElement ? element.getAttribute("href") || element.textContent || "" : element.textContent || "";
    if (parseFileRef(raw) && !element.closest("pre")) element.classList.add("agent-file-ref");
  });

  // Fenced code blocks tagged with a shell-ish language (```bash etc.) get
  // colored so the command stands out among surrounding prose; other
  // languages (python/json/...) are left alone to avoid mis-coloring.
  root.querySelectorAll("pre > code").forEach((element) => {
    const lang = Array.from(element.classList)
      .find((c) => c.startsWith("language-"))
      ?.slice("language-".length);
    if (lang && SHELL_LANGS.has(lang)) {
      element.innerHTML = highlightShellCommand(element.textContent || "");
      element.classList.add("agent-shell-block");
    }
  });

  return root.innerHTML;
}

/** Sanitized Markdown rendering that also turns code-styled file paths/
 * relative links into openable references. */
export const AgentMarkdown: React.FC<AgentMarkdownProps> = ({ content, onOpenFile }) => {
  const html = useMemo(() => decorateFileReferences(renderMarkdown(content)), [content]);

  const handleClick = (event: React.MouseEvent<HTMLDivElement>) => {
    if (!onOpenFile) return;
    const element = (event.target as HTMLElement).closest("a, code");
    if (!element || element.closest("pre")) return;
    const raw = element instanceof HTMLAnchorElement ? element.getAttribute("href") || element.textContent || "" : element.textContent || "";
    const ref = parseFileRef(raw);
    if (!ref) return;
    event.preventDefault();
    onOpenFile(ref.path, ref.line);
  };

  return <div className="agent-markdown" onClick={handleClick} dangerouslySetInnerHTML={{ __html: html }} />;
};
