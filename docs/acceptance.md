# 验收记录（三轮干净）

规则：每轮 = 一次 review（换视角）+ 一次全量测试。发现问题即修，该轮不计数，重新开始。
连续三轮零发现才算通过。

测试基线命令：`cargo test`（lib 单元 + tests/e2e.rs 管道级 + tests/console.rs 真实 ConPTY）、`cargo clippy --all-targets`。

## 第 1 轮（不计数）— 视角：静态一致性

做了什么：逐条对照 README/`--help` 的 claim 与代码；grep 所有 `=> {}` 形式的"接受但忽略"的 flag；找无人调用的 pub 项。

发现（8 项，全部已修）：

| # | 问题 | 修复 |
|---|------|------|
| 1 | `list-keys`/`list-windows`/`list-sessions` 多行输出在已 attach 时被压成一行状态栏消息，README 说 `?` 能"列出按键绑定" | 新增 overlay 视图（tmux view mode）：多行输出覆盖窗口区，任意键关闭；`render::draw_overlay` + 单元测试 + e2e |
| 2 | README 示例 `bind r source-file ~/.wmux.conf`，但 `~` 在 Windows 上不展开 | `expand_home()` |
| 3 | `new-session -A` 解析后丢弃 | 实现：同名 session 存在则 attach（非交互/`-d` 时静默成功） |
| 4 | `new-window -d`、`split-window -d/-b/-f`、`send-keys -l`、`kill-window/pane/session -a` 全部解析后丢弃 | 全部实现（`Node::split_at`/`split_root` 支持 before/full） |
| 5 | `Options::get` 无人调用 | 删除 |
| 6 | Cargo.toml 无 `rust-version`；README 写 1.85，但 clippy --fix 引入的 let-chains 需要 1.88 | `rust-version = "1.88"`，README 同步 |
| 7 | `bind -r`、`list-panes -a/-s` 仍是忽略语义 | README "Not implemented" 列明 |
| 8 | 测试 `real_client_in_conpty` 断言状态栏标题为 `"cmd"`，实际显示 OSC 标题全路径（行为正确） | 修断言 |

测试结果（修复后）：lib 47 passed / console 2 passed / e2e 6 passed / clippy 0 warnings。

遗留：无。

## 第 2 轮（计数 1/3）— 视角：机制通路存活 + 不变量

（待填）
