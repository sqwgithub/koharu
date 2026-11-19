# Koharu 中文项目速览

这个分支基于 `koharu-rs/koharu`，侧重漫画的中文翻译和排版。桌面应用用 Rust 处理项目、模型和画布，React / TypeScript 提供编辑界面，通过 Tauri 命令读写同一个打开的项目。[项目 README](../README.md) · [仓库协作规则](../AGENTS.md)

## 开始一轮开发

- 先在仓库根目录运行 `git status --short --branch`。已有改动可能来自其他会话，同一文件有重叠时用 `git diff -- <路径>` 核对。此工作副本的 `origin` 指向上游，`fork` 指向中文分支所在仓库，实际地址以 `git remote -v` 为准。
- 当前约定是先改源码，运行与改动直接相关的测试或类型检查。用户要求构建开发版时，再启动桌面构建或 Tauri 开发监听。用户说“提交本地 Git”时，只提交该任务的改动。

## 从哪里找代码

| 要处理的问题 | 首先查看 |
| --- | --- |
| OCR、译文、页面顺序、图层 ID、保存和撤销 | [项目数据模型](../crates/koharu-scene/README.md) · [SQLite 存储](../crates/koharu-scene/src/store.rs) |
| 页面跳过、续跑、流水线调度、术语分析 | [流水线](../crates/koharu-pipeline/README.md) · [调度器](../crates/koharu-pipeline/src/scheduler/mod.rs) · [续跑判定](../crates/koharu-pipeline/src/resume.rs) |
| 多页翻译、回包错位、跨页用词 | [翻译入口](../crates/koharu-pipeline/src/stages/translate/mod.rs) · [批量请求与写回](../crates/koharu-pipeline/src/stages/translate/batch.rs) · [项目会话](../crates/koharu-pipeline/src/stages/translate/conversation.rs) |
| 模型接入、请求参数、推理级别 | [语言模型层](../crates/koharu-llm/README.md) · [供应商请求适配](../crates/koharu-llm/src/model.rs) |
| “故事”面板、点击定位、页面导航开关 | [StoryPanel.tsx](../packages/koharu/components/editor/StoryPanel.tsx) · [PageRail.tsx](../packages/koharu/components/editor/PageRail.tsx) · [桌面命令](../crates/koharu-desktop/README.md) |
| 修补和译文是否可见、缩略图、PSD 导出 | [渲染器](../crates/koharu-renderer/src/renderer/mod.rs) · [画布交互](../crates/koharu-canvas/README.md) · [分层 PSD](../crates/koharu-psd/README.md) |

## 排查译文和画面

**气泡里的台词不对。** `.khr` 是 SQLite 项目文件，页面及其图层树以 JSON 保存。文本块的 `source` 保存 OCR 原文，`translation` 保存译文。“故事”面板按 `Project::story` 的顺序显示可见文字，点击条目会按 `PageId` 和 `LayerId` 定位。排查时先核对同一图层的原文和译文，再检查请求和回包。[故事阅读顺序](../crates/koharu-scene/src/project.rs) · [文本块结构](../crates/koharu-scene/src/kinds.rs)

**连续几页的用词不一致。** OpenCode Go 的 DeepSeek v4 纯文本模式会将多页 OCR 按容量组批，在同一项目会话中翻译。可见气泡的 `P005-B003` 表示项目第 5 页、“故事”第 3 项。程序核对编号和回显原文，再按图层 ID 写回。若编号和原文都正确，仍需对照原文判断译义。[批量翻译实现及回归测试](../crates/koharu-pipeline/src/stages/translate/batch.rs)

**跳过页面后画面有变化。** 页面开关保存为 `excluded`。当前工作区实现会隐藏已有译文和修补，重新纳入后恢复显示。排查时可分别核对页面标记、图层中的已存数据和渲染结果。[导航开关](../packages/koharu/components/editor/PageRail.tsx) · [渲染回归测试](../crates/koharu-renderer/tests/rendering.rs)

专项维护流程放在 [skills/](skills/)。
