export function escapeHtml(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"); }
function safeHref(raw) { const href = raw.trim(); return /^(https?:|mailto:|\/|#)/i.test(href) ? escapeHtml(href) : "#"; }
function inlineMd(s) {
  const code = [], breaks = [];
  let raw = s.replace(/`([^`]+)`/g, (_, c) => { code.push(`<code>${escapeHtml(c)}</code>`); return `\u0000C${code.length - 1}\u0000`; });
  raw = raw.replace(/<br\s*\/?>/gi, () => { breaks.push("<br/>"); return `\u0000B${breaks.length - 1}\u0000`; });
  let t = escapeHtml(raw);
  t = t.replace(/!\[([^\]]*)\]\(([^\s)]+)(?:\s+["']([^"']*)["'])?\)/g, (_, alt, url, title) => `<img src="${safeHref(url)}" alt="${alt}"${title ? ` title="${title}"` : ""} loading="lazy" />`)
    .replace(/\[([^\]]+)\]\(([^\s)]+)(?:\s+["']([^"']*)["'])?\)/g, (_, label, url, title) => `<a href="${safeHref(url)}"${title ? ` title="${title}"` : ""} target="_blank" rel="noreferrer">${label}</a>`)
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>").replace(/__([^_]+)__/g, "<strong>$1</strong>")
    .replace(/~~([^~]+)~~/g, "<del>$1</del>").replace(/(^|[^*])\*([^*]+)\*/g, "$1<em>$2</em>").replace(/(^|[^_])_([^_]+)_/g, "$1<em>$2</em>");
  return t.replace(/\u0000C(\d+)\u0000/g, (_, i) => code[Number(i)]).replace(/\u0000B(\d+)\u0000/g, (_, i) => breaks[Number(i)]);
}
const CODE_WORDS = new Set(("as async await break case catch class const continue crate def default delete do else enum export extends false finally fn for from function if impl import in interface let match mod move mut new None null of pub raise return self Some static struct super switch this throw trait true try type typeof undefined use var void while with yield").split(" "));
export function highlightCode(source) {
  const escaped = escapeHtml(source), stash = [];
  const keep = (cls, value) => { stash.push(`<span class="tok-${cls}">${value}</span>`); return `\u0000T${stash.length - 1}\u0000`; };
  return escaped.replace(/(&quot;|'|&#39;)(?:\\.|(?!\1).)*?\1/g, (m) => keep("string", m))
    .replace(/(\/\/[^\n]*|\/\*[\s\S]*?\*\/)/g, (m) => keep("comment", m)).replace(/(^|\n)(\s*#[^\n]*)/g, (_, lead, comment) => lead + keep("comment", comment))
    .replace(/\b(0x[\da-f]+|\d+(?:\.\d+)?)\b/gi, (m) => keep("number", m)).replace(/\b[A-Za-z_$][\w$]*\b/g, (m) => CODE_WORDS.has(m) ? keep("keyword", m) : m)
    .replace(/\u0000T(\d+)\u0000/g, (_, i) => stash[Number(i)]);
}
export function renderMarkdown(src) {
  const lines = src.split("\n"); let html = "", i = 0, listType = null;
  const closeList = () => { if (listType) { html += `</${listType}>`; listType = null; } };
  while (i < lines.length) {
    const line = lines[i], fence = line.match(/^\s*```(\w*)/);
    if (fence) { closeList(); const buf = [], lang = fence[1].toLowerCase(); i++; while (i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i++]); if (i < lines.length) i++; html += `<pre class="code-block" data-language="${escapeHtml(lang)}"><code class="language-${escapeHtml(lang)}">${highlightCode(buf.join("\n"))}</code></pre>`; continue; }
    if (/^\s*$/.test(line)) { closeList(); i++; continue; }
    const h = line.match(/^(#{1,3})\s+(.*)/); if (h) { closeList(); html += `<h${h[1].length}>${inlineMd(h[2])}</h${h[1].length}>`; i++; continue; }
    const ul = line.match(/^\s*[-*+]\s+(.*)/); if (ul) { if (listType !== "ul") { closeList(); html += "<ul>"; listType = "ul"; } const task = ul[1].match(/^\[([ xX])\]\s+(.*)/); html += task ? `<li class="task-item"><input type="checkbox" disabled ${task[1] !== " " ? "checked" : ""} /><span>${inlineMd(task[2])}</span></li>` : `<li>${inlineMd(ul[1])}</li>`; i++; continue; }
    const ol = line.match(/^\s*\d+\.\s+(.*)/); if (ol) { if (listType !== "ol") { closeList(); html += "<ol>"; listType = "ol"; } html += `<li>${inlineMd(ol[1])}</li>`; i++; continue; }
    const bq = line.match(/^\s*>\s?(.*)/); if (bq) { closeList(); html += `<blockquote>${inlineMd(bq[1])}</blockquote>`; i++; continue; }
    if (/^\s*([-*_])\1\1+\s*$/.test(line)) { closeList(); html += "<hr/>"; i++; continue; }
    if (/^\s*\|.*\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|[\s:|-]*$/.test(lines[i + 1])) {
      closeList(); const cells = (r) => r.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim()); const head = cells(line); i += 2; let body = "";
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) { body += "<tr>" + cells(lines[i]).map((c) => `<td>${inlineMd(c)}</td>`).join("") + "</tr>"; i++; }
      html += `<table><thead><tr>${head.map((c) => `<th>${inlineMd(c)}</th>`).join("")}</tr></thead><tbody>${body}</tbody></table>`; continue;
    }
    closeList(); const para = [line]; i++; while (i < lines.length && lines[i].trim() && !/^\s*(#{1,3}\s|[-*]\s|\d+\.\s|>|```)/.test(lines[i])) para.push(lines[i++]); html += `<p>${para.map(inlineMd).join("<br/>")}</p>`;
  }
  closeList(); return html;
}
