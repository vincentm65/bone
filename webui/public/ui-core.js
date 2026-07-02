export const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
export const MAX_ATTACHMENTS = 8;

export class DraftStore {
  constructor(storage, prefix = "bone-draft:") { this.storage = storage; this.prefix = prefix; }
  key(conversationId) { return this.prefix + (conversationId == null ? "new" : conversationId); }
  get(conversationId) { return this.storage.getItem(this.key(conversationId)) || ""; }
  set(conversationId, value) {
    const key = this.key(conversationId);
    value ? this.storage.setItem(key, value) : this.storage.removeItem(key);
  }
  move(from, to) {
    const value = this.get(from);
    if (value) this.set(to, value);
    this.set(from, "");
  }
}

export async function fileToAttachment(file) {
  if (file.size > MAX_ATTACHMENT_BYTES) throw new Error(`${file.name} exceeds the 10 MB limit`);
  if (file.type.startsWith("image/")) {
    const dataUrl = await readFile(file, "dataurl");
    return { id: crypto.randomUUID(), name: file.name, size: file.size, kind: "image", media_type: file.type, data: dataUrl.split(",")[1], preview: dataUrl };
  }
  const textLike = file.type.startsWith("text/") || /\.(md|txt|json|ya?ml|toml|csv|js|mjs|ts|tsx|jsx|css|html|rs|py|go|java|c|h|cpp|hpp|sh|sql)$/i.test(file.name);
  if (!textLike) throw new Error(`${file.name} is not an image or supported text file`);
  return { id: crypto.randomUUID(), name: file.name, size: file.size, kind: "text", text: await readFile(file, "text") };
}

function readFile(file, mode) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`Could not read ${file.name}`));
    reader.onload = () => resolve(reader.result);
    mode === "dataurl" ? reader.readAsDataURL(file) : reader.readAsText(file);
  });
}

export function buildSubmission(text, attachments) {
  const files = attachments.filter((a) => a.kind === "text");
  const suffix = files.map((a) => `\n\n<attached_file name="${a.name.replace(/[\"<>]/g, "_")}">\n${a.text}\n</attached_file>`).join("");
  return {
    text: text + suffix,
    images: attachments.filter((a) => a.kind === "image").map(({ media_type, data }) => ({ media_type, data })),
  };
}

export async function requestJson(url, options = {}) {
  const response = await fetch(url, options);
  if (!response.ok) {
    const message = (await response.text().catch(() => "")).trim();
    throw new Error(message || `${response.status} ${response.statusText}`);
  }
  if (response.status === 204) return null;
  return response.json();
}

export function downloadText(name, content) {
  const url = URL.createObjectURL(new Blob([content], { type: "text/plain;charset=utf-8" }));
  const link = document.createElement("a");
  link.href = url; link.download = name; link.click();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
