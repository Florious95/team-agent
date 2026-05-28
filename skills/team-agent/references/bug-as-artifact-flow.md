# Bug As Artifact — Flow Convention (Reference)

This is a project-maintenance reference. NOT part of SKILL.md core. The SKILL.md surface keeps only ambient day-to-day operating rules; anything extra (release flow, this bug-flow convention, future Kanban infra) lives in `references/` and is read on demand.

## Purpose

The project maintainer reads bug docs as the primary把关/oversight loop. Going forward, the maintainer will also have a Web Kanban renderer that reads these files. This convention pins the path, format, and ownership so the Kanban (or any future view layer) has a stable single source.

This convention also doubles as a **flow file**: cr/te/developer/realistic-tester/spark all reference the same per-bug file during a slice; it is the intermediate record of the work, not a doc written specifically for the maintainer to read.

## Path strong-convention

All bug docs live in `.team/bugs/<3-digit-id>-<short-slug>.md` (project root relative). The numeric id matches the corresponding §N in `.team/artifacts/development-directions/team-agent-unattended-runtime-gaps.md` when one exists; bugs that don't have a gap-doc entry still get a sequential id (request the next from the leader at intake time).

`.team/bugs/README.md` carries the convention itself + index. The Web Kanban watches this directory.

## Format strong-convention (yaml + 5 sections)

```yaml
---
id: bug-<NNN>
title: 短而具体的中文标题,普通人能看懂
status: open | in-progress | closed
severity: low | medium | high | critical
surfaced-by: 来源(谁/哪里/什么时候撞到的)
linked-gap: <N>            # 可选,对应 gaps 文档 §N
linked-cr-verdict: <path>  # 可选,如果走过 cr 设计审查
linked-contract: <path>    # 可选,如果 te 已写契约
linked-commits: [<sha>, ...]
linked-release: <version-or-status>
---

## 背景
普通话,1-2 段,描述用户/场景/什么样的人会撞到这个 bug。不堆代码路径。

## 复现步骤
1 / 2 / 3 编号,让新接手的人能照着撞出来同样现象。

## 原因分析
讲【为什么】,原因链:用户做了什么 → 框架代码里发生了什么 → 最终为什么得到坏结果。
可以引用 file:line 但必须伴随中文解释。

## 修改方案
怎么修。修法形态、涉及哪些文件、有什么 cr constraint。还没决定就写 "待 cr 设计审查"。

## 解决现状
表格列出 pipeline 各阶段(用户撞到 / 记入文档 / cr verdict / te 契约 / 实现 / 测试 / E2E / ship)的 status,带时间。
```

## Ownership: who writes what

| 角色 | 在 bug 文件里做什么 |
|---|---|
| **leader(this Claude)** | 创建文件、汇总 cr/te/developer 各阶段产出到「解决现状」、推进 status、closed 时填 linked-release |
| **agent(cr/te/developer/...)** | **读** 文件作背景上下文;**不直接写**(各自有专属产物位置:cr verdict 文件 / te 契约 / commit msg / spark findings) |
| **maintainer(user)** | 任意时间扫读全目录把关方向,纠正 leader 错的推进 |

**为什么不让 agent 直接写**:多人写同一文件冲突,且各 agent 自己的产物有更结构化的位置。leader 是单一汇总点,确保 bug 文件版本一致 + 普通话风格统一。

## Plain-language standard

跟 CONSTITUTION 风格一致:【短而硬】+ 【普通人能读懂】。

**不允许**:堆英文术语 / file:line 大段引用没注释 / 内部 jargon 把人挡在外面 / 给读者抛专业概念不解释。

**允许**:必要的 commit SHA / 文件名 / event 名,但每次出现都伴随中文解释。

## When to spawn a new bug file

任何一次新的友 bug / 真机 halt / cr/te/developer 工作过程中浮现的新结构性问题 → 立即创建 `.team/bugs/<next-id>-<slug>.md` 作为这次工作的承载文件,把所有产物 link 进来。

不要让一个 bug 没有自己的文件就开始派工。

## Relationship to SKILL.md

SKILL.md 保留团队 CLI 操作必备(quick-start / send / status / restart / shutdown / failure rules / worker protocol)。所有【流程/治理】层面的扩展(release flow / bug flow / 看板基础)都从 SKILL.md 分出到 references/,以保持 SKILL 主体的【最关键】定位。

## Relationship to future Web Kanban

`.team/bugs/` 的路径+格式强约是为 Kanban renderer 准备的单一来源:Kanban 监听该目录的 frontmatter,渲染三栏(open / in-progress / closed)看板,每张卡片展示 title + 状态 + linked-commits + plain-language 摘要,点开看完整正文。Kanban 是 view layer,不是 control plane(§1.5 / §2.1.MUST-NOT-1 不变)。

未来 Kanban 实现 slice 由用户拍优先级时启动(framework-innovation-directions §5 Slice 2)。
