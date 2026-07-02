export function parseDiff(text) {
  const lines = []; let add = 0, del = 0, prevNum = null, oldNum = null, newNum = null;
  for (const raw of String(text).split("\n")) {
    const m = raw.match(/^\s*(\d+)\s([-+ ])\s(.*)$/);
    const hunk = raw.match(/^@@\s+-(\d+)(?:,\d+)?\s+\+(\d+)(?:,\d+)?\s+@@/);
    if (hunk) { if (lines.length) lines.push({ type: "hunk" }); oldNum = Number(hunk[1]); newNum = Number(hunk[2]); prevNum = null; continue; }
    let num, sign, value;
    if (m) { num = Number(m[1]); sign = m[2]; value = m[3]; }
    else if (oldNum != null && newNum != null && /^[ +\-]/.test(raw) && !/^(---|\+\+\+)\s/.test(raw)) {
      sign = raw[0]; value = raw.slice(1); num = sign === "+" ? newNum : oldNum;
      if (sign !== "+") oldNum++; if (sign !== "-") newNum++;
    } else continue;
    if (prevNum != null && num < prevNum) lines.push({ type: "hunk" });
    if (sign === "+") { lines.push({ type: "add", ln: num, text: value }); add++; }
    else if (sign === "-") { lines.push({ type: "del", ln: num, text: value }); del++; }
    else lines.push({ type: "ctx", ln: num, text: value });
    prevNum = num;
  }
  return { lines, add, del };
}

export function artifactText(artifact) {
  if (!artifact) return "";
  return artifact.kind === "diff" ? (artifact.lines || []).filter((line) => line.text != null).map((line) => line.text).join("\n") : (artifact.content || "");
}
