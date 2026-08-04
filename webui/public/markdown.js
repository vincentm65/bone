export function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

const ALLOWED_TAGS = [
  "a", "blockquote", "br", "code", "del", "em", "h1", "h2", "h3", "h4",
  "h5", "h6", "hr", "img", "li", "ol", "p", "pre", "span", "strong", "table",
  "tbody", "td", "th", "thead", "tr", "ul",
];
const ALLOWED_ATTR = ["alt", "class", "href", "src", "title"];
const SAFE_CLASS = /^(?:code-block|task-item|task-check|checked|language-[\w-]+|tok-(?:comment|keyword|number|string))$/;
const hookedPurifiers = new WeakSet();

function normalizeUrl(raw) {
  return String(raw || "").replace(/[\u0000-\u0020\u007f-\u009f]/g, "");
}

export function isSafeLinkUrl(raw) {
  const url = normalizeUrl(raw);
  return /^(?:https?:\/\/|mailto:|\/(?!\/)|#)/i.test(url);
}

export function isSafeImageUrl(raw) {
  const url = normalizeUrl(raw);
  return /^(?:https:\/\/|\/(?!\/))/i.test(url);
}

function installSanitizerHooks(purifier) {
  if (hookedPurifiers.has(purifier)) return;
  purifier.addHook("uponSanitizeAttribute", (node, data) => {
    const tag = node.nodeName.toLowerCase();
    if (data.attrName === "href" && (tag !== "a" || !isSafeLinkUrl(data.attrValue))) data.keepAttr = false;
    if (data.attrName === "src" && (tag !== "img" || !isSafeImageUrl(data.attrValue))) data.keepAttr = false;
    if (data.attrName === "class") {
      const classes = data.attrValue.split(/\s+/).filter((name) => SAFE_CLASS.test(name));
      if (classes.length) data.attrValue = classes.join(" ");
      else data.keepAttr = false;
    }
  });
  purifier.addHook("afterSanitizeAttributes", (node) => {
    const tag = node.nodeName.toLowerCase();
    if (tag === "code") {
      const parent = node.parentElement;
      const language = node.getAttribute("class")?.split(/\s+/)
        .map((name) => name.match(/^language-([\w-]+)$/))
        .find(Boolean)?.[1];
      if (language && parent?.nodeName.toLowerCase() === "pre" && parent.classList.contains("code-block")) {
        parent.setAttribute("data-language", language);
      }
    }
    if (tag === "a" && node.hasAttribute("href")) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
    if (tag === "img" && node.hasAttribute("src")) {
      node.setAttribute("loading", "lazy");
      node.setAttribute("decoding", "async");
      node.setAttribute("referrerpolicy", "no-referrer");
    }
  });
  hookedPurifiers.add(purifier);
}

export function sanitizeRenderedHtml(html, purifier = globalThis.DOMPurify) {
  if (!purifier?.sanitize || !purifier?.addHook) throw new Error("DOMPurify is required to render Markdown");
  installSanitizerHooks(purifier);
  return purifier.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto):\/\/|mailto:|\/(?!\/)|#)/i,
    ALLOW_ARIA_ATTR: false,
    ALLOW_DATA_ATTR: false,
    SANITIZE_NAMED_PROPS: true,
    SAFE_FOR_XML: true,
  });
}

function inlineMd(source) {
  const code = [];
  const raw = source.replace(/`([^`]+)`/g, (_, value) => {
    code.push(`<code>${escapeHtml(value)}</code>`);
    return `\u0000C${code.length - 1}\u0000`;
  });
  let rendered = escapeHtml(raw);
  rendered = rendered
    .replace(/!\[([^\]]*)\]\(([^\s)]+)(?:\s+(?:&quot;|&#39;)(.*?)(?:&quot;|&#39;))?\)/g,
      (_, alt, url, title) => `<img src="${url}" alt="${alt}"${title ? ` title="${title}"` : ""}>`)
    .replace(/\[([^\]]+)\]\(([^\s)]+)(?:\s+(?:&quot;|&#39;)(.*?)(?:&quot;|&#39;))?\)/g,
      (_, label, url, title) => `<a href="${url}"${title ? ` title="${title}"` : ""}>${label}</a>`)
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/__([^_]+)__/g, "<strong>$1</strong>")
    .replace(/~~([^~]+)~~/g, "<del>$1</del>")
    .replace(/(^|[^*])\*([^*]+)\*/g, "$1<em>$2</em>")
    .replace(/(^|[^_])_([^_]+)_/g, "$1<em>$2</em>");
  return rendered.replace(/\u0000C(\d+)\u0000/g, (_, index) => code[Number(index)]);
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

function startsMarkdownBlock(lines, index) {
  return /^\s*(?:#{1,6}\s|[-*+]\s|\d+\.\s|>|```)/.test(lines[index]);
}

export function renderMarkdown(src, purifier = globalThis.DOMPurify) {
  const lines = String(src || "").split("\n");
  let html = "", i = 0, listType = null;
  const closeList = () => { if (listType) { html += `</${listType}>`; listType = null; } };
  while (i < lines.length) {
    const line = lines[i], fence = line.match(/^\s*```(\w*)/);
    if (fence) { closeList(); const buf = [], lang = fence[1].toLowerCase(); i++; while (i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i++]); if (i < lines.length) i++; html += `<pre class="code-block" data-language="${escapeHtml(lang)}"><code class="language-${escapeHtml(lang)}">${highlightCode(buf.join("\n"))}</code></pre>`; continue; }
    if (/^\s*$/.test(line)) { closeList(); i++; continue; }
    const h = line.match(/^(#{1,6})\s+(.*)/); if (h) { closeList(); html += `<h${h[1].length}>${inlineMd(h[2])}</h${h[1].length}>`; i++; continue; }
    const ul = line.match(/^\s*[-*+]\s+(.*)/); if (ul) { if (listType !== "ul") { closeList(); html += "<ul>"; listType = "ul"; } const task = ul[1].match(/^\[([ xX])\]\s+(.*)/); html += task ? `<li class="task-item"><span class="task-check${task[1] !== " " ? " checked" : ""}">${task[1] === " " ? "□" : "✓"}</span><span>${inlineMd(task[2])}</span></li>` : `<li>${inlineMd(ul[1])}</li>`; i++; continue; }
    const ol = line.match(/^\s*\d+\.\s+(.*)/); if (ol) { if (listType !== "ol") { closeList(); html += "<ol>"; listType = "ol"; } html += `<li>${inlineMd(ol[1])}</li>`; i++; continue; }
    const bq = line.match(/^\s*>\s?(.*)/); if (bq) { closeList(); html += `<blockquote>${inlineMd(bq[1])}</blockquote>`; i++; continue; }
    if (/^\s*([-*_])\1\1+\s*$/.test(line)) { closeList(); html += "<hr>"; i++; continue; }
    if (/^\s*\|.*\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|[\s:|-]*$/.test(lines[i + 1])) {
      closeList(); const cells = (r) => r.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim()); const head = cells(line); i += 2; let body = "";
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) { body += "<tr>" + cells(lines[i]).map((c) => `<td>${inlineMd(c)}</td>`).join("") + "</tr>"; i++; }
      html += `<table><thead><tr>${head.map((c) => `<th>${inlineMd(c)}</th>`).join("")}</tr></thead><tbody>${body}</tbody></table>`; continue;
    }
    closeList(); const para = [line]; i++; while (i < lines.length && lines[i].trim() && !startsMarkdownBlock(lines, i)) para.push(lines[i++]); html += `<p>${para.map(inlineMd).join("<br>")}</p>`;
  }
  closeList();
  return sanitizeRenderedHtml(html, purifier);
}
