// `target/`から、しばらくビルドに使われていない生成物を消す。Claude Codeのセッションの開始時に
// .claude/settings.jsonのフックが呼ぶ。手で呼んでもよい。
//
//   node scripts/sweep-target.mjs [日数(既定は3)]
//
// cargoは、Cargo.lockやワークスペースの版が変わるたびに別のハッシュの生成物を作り、古いものを消さない。
// ビルドのたびに、使う生成物の`.fingerprint/<名前>-<ハッシュ>`を読むので、そこが日数より前から
// 読まれていなければ(atimeはrelatimeで1日に1回しか進まないので、日単位で見る)、同じハッシュの
// `deps`・`build`・`examples`の生成物ごと消す。`incremental`は、最後にコンパイルした時刻で見る。
// それ以外(`target/debug/data`・`target/dist`・実行ファイルの写し)には触れない。

import { existsSync, lstatSync, readdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const target = join(dirname(fileURLToPath(import.meta.url)), "..", "target");
const days = Number(process.argv[2] ?? 3);
if (!(days > 0)) {
  console.error("日数は正の数で渡す");
  process.exit(2);
}
const cutoff = Date.now() - days * 86_400_000;

const HASH = /-([0-9a-f]{16})(?:\.|$)/;
// 中断されたrustcが`deps`に残す一時ファイル
const LEFTOVER = /^(?:rmeta|rustc)[A-Za-z0-9]{6}$|\.rcgu\.o$/;

let freed = 0;

function list(dir) {
  try {
    return readdirSync(dir);
  } catch {
    return [];
  }
}

function sizeOf(path) {
  const st = lstatSync(path);
  if (!st.isDirectory()) return st.size;
  return list(path).reduce((sum, name) => sum + sizeOf(join(path, name)), 0);
}

function remove(path) {
  freed += sizeOf(path);
  rmSync(path, { recursive: true, force: true });
}

function lastUsed(path) {
  const st = lstatSync(path);
  return Math.max(st.atimeMs, st.mtimeMs);
}

function sweepProfile(profile) {
  const fingerprints = join(profile, ".fingerprint");
  const live = new Set();
  const stale = new Set();
  for (const name of list(fingerprints)) {
    const hash = HASH.exec(name)?.[1];
    if (!hash) continue;
    const dir = join(fingerprints, name);
    const used = Math.max(lastUsed(dir), ...list(dir).map((f) => lastUsed(join(dir, f))));
    if (used >= cutoff) {
      live.add(hash);
    } else {
      stale.add(hash);
      remove(dir);
    }
  }
  for (const sub of ["deps", "build", "examples"]) {
    for (const name of list(join(profile, sub))) {
      const path = join(profile, sub, name);
      const hash = HASH.exec(name)?.[1];
      const old = lastUsed(path) < cutoff;
      // `.fingerprint`の無いハッシュは、もうどのビルドからも使われない
      if (hash ? !live.has(hash) && (stale.has(hash) || old) : LEFTOVER.test(name) && old) {
        remove(path);
      }
    }
  }
  for (const name of list(join(profile, "incremental"))) {
    const path = join(profile, "incremental", name);
    if (lstatSync(path).mtimeMs < cutoff) remove(path);
  }
}

// `target/<プロファイル>`と、ターゲットを指定したビルドの`target/<ターゲット>/<プロファイル>`
for (const a of list(target)) {
  for (const dir of [join(target, a), ...list(join(target, a)).map((b) => join(target, a, b))]) {
    if (existsSync(join(dir, ".fingerprint"))) sweepProfile(dir);
  }
}

if (freed > 0) {
  console.log(`target/から、${days}日使われていない生成物を消した(${(freed / 1e9).toFixed(1)}GB)`);
}
