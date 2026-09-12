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

## 第 2 轮（不计数）— 视角：机制通路存活 + 不变量

做了什么：通读 server/mod.rs 全部 2000 行，对每个选项/命令问"改了输入，输出真的变吗"；对 active/last/cur 索引的不变量逐入口核对。

发现（5 项，全部已修）：

| # | 问题 | 修复 |
|---|------|------|
| 1 | 默认 `,`/`$` 绑定的模板 `rename-window -- %%` 把 `--` 当成新名字（`Args::is_flag` 不消费 `--`） | `is_flag` 消费 `--` 并结束 flag 解析；删掉三处手写的 `--` 特判；单元测试 `double_dash_ends_flags` + e2e 走 `,` 绑定 |
| 2 | `set mouse off` 只停掉 server 端鼠标处理，client 控制台仍 `ENABLE_MOUSE_INPUT`，终端原生选区依旧不可用 | 新增 `ServerMsg::SetMouse`：attach 后与 `set mouse` 时下发，client 切换 `ENABLE_MOUSE_INPUT`/`ENABLE_QUICK_EDIT_MODE`；e2e 断言两次切换都到达 |
| 3 | 配置文件解析错误只写日志，用户看不到 | 记入 `config_errors`，首个 attach 的 client 以 overlay 显示 |
| 4 | 多行 Error（`source-file` 多条）被压成一行状态栏消息 | `show()` 统一：含换行走 overlay；e2e 用坏配置文件验证两条错误都显示 |
| 5 | re-attach 不清 overlay/drag/mouse_buttons/swallow_up | `Outcome::Attach` 里全部重置 |

不变量核对（无发现）：`w.active ∈ w.panes`（remove_pane / break-pane / kill -a 三个入口）；`s.cur < s.windows.len()`；`base-index` 在 label / resolve / `0-9` 绑定三处一致；status_click 的列偏移与 render 的 `"[name] "` + `"label "` 布局一致。

测试结果（修复后）：lib 48 passed / console 2 passed / e2e 6 passed / clippy 0 warnings。

## 第 3 轮（计数 1/3）— 视角：代码正确性（错误路径、资源、并发）+ 边界输入

（待填）
