export function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

const ALLOWED_TAGS = [
  "a", "abbr", "address", "article", "b", "bdi", "bdo", "blockquote", "br",
  "caption", "cite", "code", "col", "colgroup", "data", "dd", "del", "details",
  "dfn", "div", "dl", "dt", "em", "figcaption", "figure", "footer", "h1", "h2",
  "h3", "h4", "h5", "h6", "header", "hr", "i", "img", "kbd", "li", "main",
  "mark", "ol", "p", "pre", "q", "rp", "rt", "ruby", "s", "samp", "section",
  "small", "span", "strong", "sub", "summary", "sup", "table", "tbody", "td",
  "tfoot", "th", "thead", "time", "tr", "u", "ul", "var",
];
const ALLOWED_ATTR = [
  "alt", "class", "colspan", "datetime", "href", "open", "reversed", "rowspan",
  "scope", "src", "start", "title", "value",
];
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

function stashHtmlTags(source, stash) {
  let result = "";
  for (let i = 0; i < source.length;) {
    if (source.startsWith("<!--", i)) {
      const end = source.indexOf("-->", i + 4);
      const stop = end < 0 ? source.length : end + 3;
      stash.push(source.slice(i, stop));
      result += `\u0000H${stash.length - 1}\u0000`;
      i = stop;
      continue;
    }
    if (source[i] !== "<" || !/^<\/?[A-Za-z][\w:-]*(?:\s|\/?>)|^<![A-Za-z]/.test(source.slice(i))) {
      result += source[i++];
      continue;
    }
    let quote = null;
    let end = i + 1;
    for (; end < source.length; end++) {
      const char = source[end];
      if (quote) {
        if (char === quote) quote = null;
      } else if (char === '"' || char === "'") quote = char;
      else if (char === ">") break;
    }
    if (end === source.length) {
      result += source[i++];
      continue;
    }
    stash.push(source.slice(i, end + 1));
    result += `\u0000H${stash.length - 1}\u0000`;
    i = end + 1;
  }
  return result;
}

function inlineMd(source) {
  const code = [];
  const html = [];
  let raw = source.replace(/`([^`]+)`/g, (_, value) => {
    code.push(`<code>${escapeHtml(value)}</code>`);
    return `\u0000C${code.length - 1}\u0000`;
  });
  raw = stashHtmlTags(raw, html);
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
  return rendered
    .replace(/\u0000C(\d+)\u0000/g, (_, index) => code[Number(index)])
    .replace(/\u0000H(\d+)\u0000/g, (_, index) => html[Number(index)]);
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

const HTML_BLOCK_TAGS = new Set([
  "address", "article", "aside", "base", "blockquote", "body", "caption", "center", "col",
  "colgroup", "dd", "details", "dialog", "dir", "div", "dl", "dt", "fieldset", "figcaption",
  "figure", "footer", "form", "frame", "frameset", "h1", "h2", "h3", "h4", "h5", "h6",
  "head", "header", "hr", "html", "iframe", "legend", "li", "link", "main", "menu", "menuitem",
  "meta", "nav", "noframes", "ol", "optgroup", "option", "p", "param", "script", "search",
  "section", "source", "style", "summary", "table", "tbody", "td", "tfoot", "th", "thead",
  "title", "tr", "track", "ul", "svg", "math", "object", "template", "textarea",
]);
const VOID_TAGS = new Set(["base", "col", "frame", "hr", "img", "input", "link", "meta", "param", "source", "track"]);

function htmlBlockAt(lines, start) {
  const first = lines[start].trimStart();
  if (first.startsWith("<!--")) {
    let end = start;
    while (end + 1 < lines.length && !lines[end].includes("-->")) end++;
    return { html: lines.slice(start, end + 1).join("\n"), next: end + 1 };
  }
  if (/^<![A-Za-z]/.test(first)) return { html: lines[start], next: start + 1 };
  const match = first.match(/^<(\/)?([A-Za-z][\w:-]*)(?:\s|\/?>)/);
  if (!match || !HTML_BLOCK_TAGS.has(match[2].toLowerCase())) return null;
  const tag = match[2].toLowerCase();
  if (match[1] || VOID_TAGS.has(tag) || first.includes(`</${tag}>`) || /\/>\s*$/.test(first)) {
    return { html: lines[start], next: start + 1 };
  }
  let end = start;
  const close = new RegExp(`</${tag}\\s*>`, "i");
  while (end + 1 < lines.length && lines[end].trim() && !close.test(lines[end])) end++;
  return { html: lines.slice(start, end + 1).join("\n"), next: end + 1 };
}

function startsMarkdownBlock(lines, index) {
  const line = lines[index];
  return /^\s*(?:#{1,6}\s|[-*+]\s|\d+\.\s|>|```)/.test(line) || htmlBlockAt(lines, index) != null;
}

export function renderMarkdown(src, purifier = globalThis.DOMPurify) {
  const lines = String(src || "").split("\n");
  let html = "", i = 0, listType = null;
  const closeList = () => { if (listType) { html += `</${listType}>`; listType = null; } };
  while (i < lines.length) {
    const line = lines[i], fence = line.match(/^\s*```(\w*)/);
    if (fence) { closeList(); const buf = [], lang = fence[1].toLowerCase(); i++; while (i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i++]); if (i < lines.length) i++; html += `<pre class="code-block" data-language="${escapeHtml(lang)}"><code class="language-${escapeHtml(lang)}">${highlightCode(buf.join("\n"))}</code></pre>`; continue; }
    if (/^\s*$/.test(line)) { closeList(); i++; continue; }
    const htmlBlock = htmlBlockAt(lines, i);
    if (htmlBlock) { closeList(); html += htmlBlock.html; i = htmlBlock.next; continue; }
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
