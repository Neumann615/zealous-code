# spec 152:Provider 错误嵌套治理(问题定义与修复方案)

来源:Dogfooding 151 A2。`nested_provider_error_count` 达 3
(继承 1 + A2-500 + A2-timeout),按"×3 立项"规则建专项。
**本 spec 阶段只定义问题与修复方案,不在 Dogfooding 中顺手修。**

## 问题

Provider 错误经多层包装后重复携带同类信息,可读性差:

- HTTP 500 场景:
  `provider: provider_exhausted after 4 attempts: provider_exhausted
  after 0 attempts: http 500: {"error": …}`
  —— `exhausted after N attempts` 出现两次,且内层计数(0)与外层(4)
  语义冲突。
- timeout 场景:
  `provider: provider_exhausted after 4 attempts: provider_exhausted
  after 0 attempts: network: …: Network Error: Network Error: …
  (os error 10060)`
  —— 双重 exhausted 嵌套 + `Network Error` 三连重复 + OS 本地化文本。

影响:用户需自行拼接才能理解"重试 4 次后仍失败,最后一次原因是 X";
审计账本中的错误文本冗长。

## 根因(代码定位)

- `zealous-engine/src/provider.rs`:`ProviderError::Exhausted` 在重试层
  与调用层各格式化一次(`provider_exhausted after N attempts` 重复);
  `last` 字段携带的已是内层 Display 结果(含自己的 `provider_exhausted
  after 0 attempts` 前缀)。
- 网络层错误文本在 ureq Error → String → 再包装链路中重复拼接
  `Network Error`。

## 修复方案(实施阶段执行)

1. 重试层只产生结构化 `Exhausted { attempts, last_cause }`;
   **最终 Display 只在一处**格式化:
   `provider_exhausted after {attempts} attempts: {last_cause}`,
   其中 `last_cause` 使用底层错误(如 `http 500: …` / `network: …`),
   不再包含内层 `provider_exhausted` 前缀。
2. ureq 网络错误映射为单一 `network: {url}: {原因}` 形态;
   去掉重复的 `Network Error` 段;OS 错误码保留。
3. 回归要求:
   - 既有 provider 单测(含 with_retry 注入)全绿;
   - 新增快照断言:500 / timeout / badjson 三类最终文本各自精确形态;
   - Dogfooding A2 四个子场景重跑,账本错误文本符合新格式,
     不再出现双重 `exhausted after` 与 `Network Error` 重复。

## 实施时机

独立实施 + 完整质量门 + 重新打包(运行时代码变化会使当前 offline RC
失效,按 spec 150 三段溯源重新生成)。

## 实施记录(2026-10-22)

已按本方案实施(commit 3141499):新增 `Transient` 变体、
classify_ureq 归一化(移除 `Network Error` 重复段)、错误单点格式化。
回归:provider 单测 3 项新增全绿;Dogfooding A2 四子场景复跑通过,
账本错误文本单层(exhausted ×1 / Network Error ×0),详见
docs/acceptance/dogfooding-152-153.md。
