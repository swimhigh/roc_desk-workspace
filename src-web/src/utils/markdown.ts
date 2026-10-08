import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ breaks: true, gfm: true });

/** Markdown rendering for the coding agent's timeline -- content may
 * originate from a remote workspace (a not-fully-trusted remote file), and
 * Markdown itself allows embedded raw HTML, so parsed output must go
 * through `DOMPurify` before `dangerouslySetInnerHTML` -- otherwise this is
 * a ready-made XSS vector (e.g. an `<img onerror=...>` tucked into a file). */
export function renderMarkdown(text: string): string {
  const html = marked.parse(text, { async: false }) as string;
  return DOMPurify.sanitize(html);
}
