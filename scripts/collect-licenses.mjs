/**
 * 开源许可合规（FR-PKG-07）：构建期自动收集全部直接依赖的许可文本到 src-tauri/LICENSES/。
 * - npm 依赖：package.json dependencies → node_modules/<pkg>/LICENSE*
 * - Rust crates：Cargo.toml [dependencies] → 从 Cargo.lock 取版本 → cargo registry 源码目录 LICENSE*
 * 由 tauri.conf.json 的 beforeBuildCommand 调用（依赖变更后自动更新，防手工遗漏）。
 */
import { readFileSync, writeFileSync, existsSync, mkdirSync, readdirSync, copyFileSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const outDir = join(root, "src-tauri", "LICENSES");
const manifest = [];

function collectNpm() {
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const deps = new Set(Object.keys(pkg.dependencies ?? {}));
  // devDependencies 中被发布产物引用的也必须收集（FR-PKG-07 AC：发布包内每个直接依赖可检索）
  // ——pagedjs 经 pdfchamber.ts 打进 dist（曾漏）
  deps.add("pagedjs");
  let n = 0;
  for (const dep of deps) {
    const dir = join(root, "node_modules", dep);
    const lic = readdirSync(dir, { withFileTypes: true })
      .filter((f) => f.isFile() && /^(license|licence|copying|notice)/i.test(f.name))
      .map((f) => join(dir, f.name))[0];
    const meta = existsSync(join(dir, "package.json"))
      ? JSON.parse(readFileSync(join(dir, "package.json"), "utf8"))
      : {};
    if (lic) {
      const destDir = join(outDir, "npm", dep);
      mkdirSync(destDir, { recursive: true });
      copyFileSync(lic, join(destDir, "LICENSE"));
      n++;
    }
    manifest.push({ kind: "npm", name: dep, version: meta.version ?? "?", license: meta.license ?? (lic ? "见 LICENSE" : "未找到") });
  }
  console.log(`npm 依赖许可：${n}/${deps.length} 份已收集`);
}

function cargoRegistrySrc() {
  // cargo registry 源码缓存根（遍历可能的 registry 目录）
  const home = process.env.CARGO_HOME || join(process.env.USERPROFILE ?? process.env.HOME ?? "", ".cargo");
  const reg = join(home, "registry", "src");
  if (!existsSync(reg)) return null;
  for (const d of readdirSync(reg)) {
    const p = join(reg, d);
    if (statSync(p).isDirectory()) return p;
  }
  return null;
}

function collectCrates() {
  const cargoToml = readFileSync(join(root, "src-tauri", "Cargo.toml"), "utf8");
  // [dependencies] 段的直接依赖名（跳过注释行）
  const section = cargoToml.split(/\n\[/).find((s) => s.startsWith("dependencies]")) ?? "";
  const names = [...section.matchAll(/^\s*([a-z0-9_-]+)\s*=/gm)].map((m) => m[1]);
  // Cargo.lock 版本映射
  const lock = readFileSync(join(root, "src-tauri", "Cargo.lock"), "utf8");
  const versions = new Map();
  for (const m of lock.matchAll(/name = "([a-z0-9_-]+)"\s*\nversion = "([^"]+)"/g)) {
    if (!versions.has(m[1])) versions.set(m[1], m[2]);
  }
  const src = cargoRegistrySrc();
  let n = 0;
  for (const name of names) {
    const ver = versions.get(name) ?? "";
    let lic = null;
    if (src) {
      const dir = join(src, `${name}-${ver}`);
      if (existsSync(dir)) {
        lic = readdirSync(dir, { withFileTypes: true })
          .filter((f) => f.isFile() && /^(license|licence|copying|notice)/i.test(f.name))
          .map((f) => join(dir, f.name))[0];
      }
    }
    if (lic) {
      const destDir = join(outDir, "crates", `${name}-${ver}`);
      mkdirSync(destDir, { recursive: true });
      copyFileSync(lic, join(destDir, "LICENSE"));
      n++;
    }
    manifest.push({ kind: "crate", name, version: ver || "?", license: lic ? "见 LICENSE" : "未在本地缓存找到（见 crates.io）" });
  }
  console.log(`Rust crate 许可：${n}/${names.length} 份已收集`);
}

function writeManifest() {
  const lines = [
    "# Writer 开源组件清单",
    "",
    "完整许可文本见本目录 npm/ 与 crates/ 子目录（构建期由 scripts/collect-licenses.mjs 自动收集）。",
    "",
    "| 类型 | 组件 | 版本 | 许可 |",
    "|---|---|---|---|",
    ...manifest.map((m) => `| ${m.kind} | ${m.name} | ${m.version} | ${m.license} |`),
  ];
  writeFileSync(join(outDir, "MANIFEST.md"), lines.join("\n"), "utf8");
  console.log(`清单：src-tauri/LICENSES/MANIFEST.md（${manifest.length} 项）`);
}

mkdirSync(outDir, { recursive: true });
collectNpm();
collectCrates();
writeManifest();
// 供 NSIS 打包带入安装目录的确认（tauri resources 配置见 tauri.conf.json 迭代）
console.log("LICENSES 收集完成");
