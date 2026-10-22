# Dogfooding 154 验收记录(spec 154:工具与上下文增强)

- 执行日期:2026-10-22
- Dogfooding 二进制:target/debug/zealous.exe(97e70b7,spec 154 实现)
- LOCAL TRIAL 回归二进制:dist zip 解压(runtime source 97e70b7,
  zip SHA256 CDA2EF906662C83ED7F7901CA860942A887E777379295B7AB10A567F400A98C1,
  exe SHA256 DFA87B455D2C0B432BF0F9E64FE04BAE3AFFC137A8D01F6F3E66278F07F3E42E)
- 模型:智谱 glm-5.3-flash
- 临时仓库:%TEMP%\zealous-dogfooding-154\

## D1 大文件定位修改 —— PASS

- 夹具:data.txt 1500 行 × ~400B ≈ 585KB(>512KB,整读必然被拒)
- 模型行为:3 次分片读取(offset 1/501/1001,limit 500)扫描 +
  offset 1200 精准确认,随后 edit_file;
  **data.txt 整读 0 次**;
- 审批:1 次(edit_file,唯一匹配);修改精确(1 insertion, 1 deletion,
  LEGACY-FLAG=on→off);rollback commit 仅含 data.txt;
- 账本 seq 42 连续,9 turns。

## D2 正则搜索分散修改 —— PASS

- 夹具:src/ lib/ tools/ docs/ 四目录,DEPRECATED_API 分布在
  3 个文件 + docs 干净文件;
- 模型行为:search_text regex=true ×2(0 次 read_file!),
  随后 3 次 edit_file 精确改名(lib/core.py、src/api.py、tools/gen.py);
  docs/readme.md 零改动,工作区干净(.zealous 除外);
- 账本 seq 38 连续,7 turns,3 次审批。

## D3 历史与分支理解 —— PASS

- 夹具:main(v1→v2)+ experiment 分支(v3 plugins),当前 main;
- 模型行为:git_branch(all=true)→ git_status → git_log ×2 →
  git_show(experiment)→ git_show(main:README.md)→
  git_show(experiment~1),共 3 次 git_show;
- 报告:分支清单(main/experiment)、当前分支、experiment 相对 main
  的分叉与提交差异,并明确声明未做任何修改;
- **write/edit/shell 调用 0 次**,工作区干净;seq 40 连续,9 turns。

## LOCAL TRIAL 回归(新包二进制)

- 重跑 D1 场景:同一分片模式(3×500 扫描 + 精准读),
  edit 精确,seq 49 连续,Completed —— PASS。

## 质量门与重打包

- 每步实现后 fmt/test --all/clippy 全绿;
- mock 集成测试 e2e_spec154_tools_sequence 通过
  (plan→分片读→正则搜索→git_show→edit→message,
  seq 连续、T1 审批、rollback 干净、无泄漏);
- LOCAL TRIAL(CDA2EF90…A98C1)→ 受影响回归通过 →
  默认打包生成新 OFFLINE EXTERNAL CANDIDATE(见 RC 记录)。

## P2 观察

- Cargo.lock 副产物:本轮未再现(计数仍 1)。
- 无新增 P0 / P1 / P2。

## 阶段末本地 bundle(spec 154)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| zealous-code-main-spec154.bundle | 352672 | 2FE45EA761E2DEF3916CDC38EEEE97157D4681602D51DC19D03F50AAF23E403C |

- verify complete history 通过,含 main 至 a7b9286(RC record commit);
- verify 通过后删除旧 zealous-code-main-dogfooding151.bundle
  (FF103F8B…F484CB,为新 bundle 历史前缀);
- 本提交为 bundle rotation commit,不进入 bundle(spec 150 既定限制);
- offline RC lock 已刷新至新候选包;archive bundle 保留;仍不推 remote。