/**
 * Lightweight shell syntax highlighting: no full shell grammar parsing,
 * just colors the most common token classes (command name, flags,
 * strings, variables, pipes/redirects, comments) so a command line in the
 * coding agent panel visually stands apart from ordinary prose.
 */

const TOKEN_RE = /("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\$\{[^}]+\}|\$\w+|&&|\|\||>>|<<|[|<>;])|(\s+)|(\S+)/g;

function escapeHtml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function span(cls: string, text: string): string {
  return `<span class="${cls}">${escapeHtml(text)}</span>`;
}

function findCommentIndex(line: string): number {
  let inSingle = false;
  let inDouble = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === "'" && !inDouble) inSingle = !inSingle;
    else if (c === '"' && !inSingle) inDouble = !inDouble;
    else if (c === "#" && !inSingle && !inDouble && (i === 0 || /\s/.test(line[i - 1]))) return i;
  }
  return -1;
}

function highlightLine(line: string): string {
  const commentIdx = findCommentIndex(line);
  const codePart = commentIdx >= 0 ? line.slice(0, commentIdx) : line;
  const commentPart = commentIdx >= 0 ? line.slice(commentIdx) : "";

  let html = "";
  let atLineStart = true;
  let expectCommand = true;
  let match: RegExpExecArray | null;
  TOKEN_RE.lastIndex = 0;
  while ((match = TOKEN_RE.exec(codePart))) {
    const [, special, space, word] = match;
    if (space) {
      html += escapeHtml(space);
      continue;
    }
    if (special) {
      if (special[0] === '"' || special[0] === "'") html += span("shell-string", special);
      else if (special[0] === "$") html += span("shell-var", special);
      else {
        html += span("shell-op", special);
        if (special === "|" || special === "&&" || special === "||" || special === ";") expectCommand = true;
      }
      atLineStart = false;
      continue;
    }
    if (word) {
      if (atLineStart || expectCommand) html += span("shell-cmd", word);
      else if (word.startsWith("-")) html += span("shell-flag", word);
      else html += span("shell-arg", word);
      atLineStart = false;
      expectCommand = false;
    }
  }
  if (commentPart) html += span("shell-comment", commentPart);
  return html;
}

export function highlightShellCommand(raw: string): string {
  return raw.split("\n").map(highlightLine).join("\n");
}
