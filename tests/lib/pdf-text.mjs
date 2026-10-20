// PDF 文本层提取（预研验证用）：借助本机 python pypdf，返回每页文本数组
import { execFileSync } from "node:child_process";
export async function extract(pdfPath) {
  const out = execFileSync("python", ["-c", `
import json, sys
from pypdf import PdfReader
r = PdfReader(sys.argv[1])
print(json.dumps([p.extract_text() or "" for p in r.pages], ensure_ascii=False))
`, pdfPath], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  return JSON.parse(out);
}
