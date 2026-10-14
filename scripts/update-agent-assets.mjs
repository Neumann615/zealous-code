#!/usr/bin/env node
// 维护 agent-assets/ 下的只读参考仓库:
//   1. 缺失的仓库执行完整 clone;
//   2. 已存在的仓库要求工作区干净,执行 git fetch --prune origin;
//   3. 将仓库 detach 到 origin 默认分支的最新 commit,并输出当前 commit 便于引用。
// 用法: node scripts/update-agent-assets.mjs
import { execFile } from 'node:child_process';
import { access } from 'node:fs/promises';
import path from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';

const execFileAsync = promisify(execFile);

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const ASSETS_DIR = path.join(REPO_ROOT, 'agent-assets');
const EXEC_OPTS = { maxBuffer: 32 * 1024 * 1024 };

const REFERENCES = [
  { name: 'zcode', url: 'https://github.com/zai-org/zcode.git' },
  { name: 'codex', url: 'https://github.com/openai/codex.git' },
];

async function git(repoDir, args) {
  const { stdout } = await execFileAsync('git', ['-C', repoDir, ...args], EXEC_OPTS);
  return stdout.trim();
}

async function pathExists(target) {
  try {
    await access(target);
    return true;
  } catch {
    return false;
  }
}

async function resolveDefaultBranch(repoDir) {
  // 优先读取本地 origin/HEAD;clone 后未设置该符号引用时,退回查询远端 HEAD。
  try {
    const ref = await git(repoDir, ['symbolic-ref', 'refs/remotes/origin/HEAD']);
    return ref.replace(/^refs\/remotes\/origin\//, '');
  } catch {
    const { stdout } = await execFileAsync(
      'git',
      ['-C', repoDir, 'ls-remote', '--symref', 'origin', 'HEAD'],
      EXEC_OPTS,
    );
    const match = /^ref:\s+refs\/heads\/(\S+)\tHEAD$/m.exec(stdout);
    if (!match) {
      throw new Error(`无法解析 origin 默认分支`);
    }
    return match[1];
  }
}

async function updateReference({ name, url }) {
  const repoDir = path.join(ASSETS_DIR, name);
  const hasGitDir = await pathExists(path.join(repoDir, '.git'));

  if (!hasGitDir) {
    if (await pathExists(repoDir)) {
      // 目录存在但不是 git 仓库(可能是失败 clone 的残留),不做破坏性清理,交由人工处理。
      throw new Error(`${repoDir} 已存在但不是 git 仓库,请手动清理后重试`);
    }
    console.log(`[clone] ${name} ← ${url}`);
    await execFileAsync('git', ['clone', url, repoDir], EXEC_OPTS);
  } else {
    const status = await git(repoDir, ['status', '--porcelain']);
    if (status !== '') {
      throw new Error(`${name} 工作区存在未提交修改,拒绝更新。请在 ${repoDir} 手动处理后重试`);
    }
    console.log(`[fetch] ${name}`);
    await git(repoDir, ['fetch', '--prune', 'origin']);
  }

  const branch = await resolveDefaultBranch(repoDir);
  await git(repoDir, ['checkout', '--detach', `origin/${branch}`]);
  const commit = await git(repoDir, ['rev-parse', 'HEAD']);
  const subject = await git(repoDir, ['log', '-1', '--format=%s']);
  console.log(`[ok] ${name} @ origin/${branch} ${commit}`);
  console.log(`     ${subject}`);
}

async function main() {
  await access(ASSETS_DIR).catch(async () => {
    const { mkdir } = await import('node:fs/promises');
    await mkdir(ASSETS_DIR, { recursive: true });
  });

  const failures = [];
  for (const reference of REFERENCES) {
    try {
      await updateReference(reference);
    } catch (error) {
      failures.push(reference.name);
      console.error(`[fail] ${reference.name}: ${error.message}`);
    }
  }

  if (failures.length > 0) {
    console.error(`\n${failures.length} 个参考仓库更新失败: ${failures.join(', ')}`);
    process.exitCode = 1;
  }
}

await main();
