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

## 第 3 轮（不计数）— 视角：代码正确性（错误路径、资源、并发）+ 边界输入

做了什么：通读 client.rs / console.rs / pane.rs / clipboard.rs / ipc.rs 与 mod.rs 的 copy-mode 辅助函数；对每个 unsafe 与每个无界资源问"谁来收尾、上限在哪"；对 usize 减法找下溢。

发现（7 项，全部已修）：

| # | 问题 | 修复 |
|---|------|------|
| 1 | `copy_abs` 计算 `max - offset`：`clear-history` 或缩窗后 scrollback 变短，offset > max 下溢 panic | `copy_abs` 先把 offset/cy 夹回范围；`clear-history` 先退出 copy mode |
| 2 | 每客户端输出通道无界：pane 高速输出 + 客户端 `WriteConsoleW` 慢 → 服务端内存无限增长 | 有界通道 64 帧；满则丢帧并置 `last_grid=None` 强制下次全量重绘（语义上等价）；单元测试 `lagging_client_forces_full_redraw` |
| 3 | `kill-pane` 只 `TerminateProcess` 顶层 shell，shell 启动的子进程（ping、编译）变孤儿并继续占用 ConPTY | 每 pane 一个 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` job；kill/drop 先关 job；测试 `kill_takes_grandchildren_down`（孙进程 ping 被杀，marker 文件不出现）与 `job_kills_tree_on_drop` |
| 4 | 命名管道默认 DACL，本机其他用户可 connect | SDDL `D:P(A;;GA;;;<user SID>)(A;;GA;;;SY)`，每个 pipe 实例都带；真实二进制测试仍通过证明同用户可连 |
| 5 | `display-time 0` 变成"立即消失"，tmux 语义是"直到按键" | render_all 跳过 TTL 清理；handle_key 在任意逻辑键时清消息 |
| 6 | `clipboard::get_text` 找 NUL 无上限，数据无 NUL 时越界读 | `GlobalSize` 上界 |
| 7 | `-L` socket 名不过滤：`\`/`..` 可逃出 `wmux-<user>-` 命名空间 | `pipe_name` 对 socket 名同样白名单过滤；空名回落 `default`；单元测试 |

测试结果（修复后）：lib 53 passed / console 2 passed / e2e 6 passed / clippy 0 warnings。

## 第 4 轮（不计数）— 视角：边界与退化输入

做了什么：针对每个接收外部尺寸/坐标/字符串的入口构造退化输入（0x0 / 1x1 区域、越界鼠标坐标、非 UTF-8 argv、不存在的程序、非 ASCII 名字），先写测试再看结果。

发现（4 项，全部已修）：

| # | 问题 | 修复 |
|---|------|------|
| 1 | `std::env::args()` 遇到非 UTF-8 参数（路径里未配对代理项）直接 panic | `args_os()` + lossy |
| 2 | 点击窗口区域外缘（x == cols）会被当成最右 pane 的边框，开始一次错误的拖拽 | `border_owner` 只在 `window_area` 内生效 |
| 3 | 0 宽/高区域布局时第二个子节点被放到区域外（`pos += sz + 1` 不封顶） | `layout()` 对 `sz` 与 `pos` 以区域末端封顶；测试 `degenerate_areas_never_panic` |
| 4 | pane 矩形大于 client 网格时（两 client 尺寸不同），光标可能被放到网格外 | `compose` 额外检查光标在 `f.cols × f.rows` 内；测试 `compose_degenerate_sizes` |

新增测试：`list_keys_is_reproducible_across_servers`（两个独立 server 的 `list-keys` 逐字节一致）、不存在程序返回错误而非死 session、非 ASCII session/window 名往返。

测试结果（修复后）：lib 55 passed / console 2 passed / e2e 7 passed / clippy 0 warnings。

## 第 5 轮（计数 1/3，无发现）— 视角：可复现性 + 按键编码逻辑

做了什么：`cargo test` 连跑 3 次，对"排序后的测试名+状态"做 SHA-256；逐条核对 `key_from_record` / `encode_key` / `handle_key` 的 prefix、swallow_up、copy-mode、overlay、IME/代理项、AltGr、Ctrl+数字/标点、修饰键单按等路径。

数据：3 次运行指纹均为 `E9109651AFB600D1`，每次 lib 55 / console 2 / e2e 7 全绿。

发现：无。

## 第 6 轮（不计数）— 视角：静态一致性复查（依赖、示例配置、文档表格）

做了什么：grep 每个 Cargo 依赖在 `src/` 的使用；用真实二进制加载 `wmux.conf.example`（`WMUX_CONFIG`）并核对 `list-keys` 与 server 日志；README 按键表逐行对照 `default_bindings()`。

发现（1 项，已修）：

| # | 问题 | 修复 |
|---|------|------|
| 1 | `clap`、`toml` 两个依赖以及 tokio 的 `signal` feature 声明了但代码从未使用 | 删除 |

数据：示例配置加载日志 `loaded ... (20 commands)`，无错误行；`list-keys` 含 `W`/`h`/`r`/`|` 四条自定义绑定；README 表 16 行全部与默认绑定一致。

## 第 7 轮（不计数）— 视角：新增代码（winsec / job / 背压）的错误路径

做了什么：对第 3 轮新增的 `winsec.rs`、pane job、有界队列逐个失败分支追问"失败后系统处于什么状态、谁还能恢复"。

发现（1 项，已修）：

| # | 问题 | 修复 |
|---|------|------|
| 1 | 有界队列满时 `Client::send` 直接丢弃控制消息。真实触发场景：`mouse off` 下用户在宿主终端 QuickEdit 拖选会冻结 `WriteConsoleW`，队列填满，此时的 `Detached`/`SetMouse` 丢失，客户端永远收不到 detach | 控制消息进 `pending` 队列，每次 `render_all` 前补发；pending 非空时帧不插队（丢帧并强制全量重绘）；单元测试 `control_messages_survive_a_full_queue_in_order` |

核对无发现：`current_user_sid` 两段式缓冲区、LocalFree 配对；`OwnerOnly` 指针在 future 移动后每次现取；job assign 失败回落到 TerminateProcess；`Pane::drop` 先关 job 再关 ConPTY 的顺序。

测试结果（修复后）：lib 56 passed / console 2 passed / e2e 7 passed / clippy 0 warnings。

## 第 8 轮（计数 1/3，无发现）— 视角：可复现性 + 机制通路（真实二进制）

数据：`cargo test` 连跑 3 次指纹均为 `A4092EC0BD2FA264`（65 项：lib 56 / console 2 / e2e 7）。
release 二进制 + 临时配置（`prefix C-a`、`status-position top`、`base-index 1`、`bind -n M-q`、`default-shell cmd`）：
`list-windows` 编号从 1 起；`send-keys -t s1:2` 命中第二个窗口；`list-keys` 含 `root M-q`；pane 标题 `cmd.exe`。全部生效。

发现：无。

## 第 9 轮（计数 2/3，无发现）— 视角：静态一致性终审

做了什么：`--help` 文本中 20 个命令名逐个对照 `command::parse` 的别名表；README 中的路径（`.wmux.conf`、`.config\wmux`、`WMUX_CONFIG`、`%LOCALAPPDATA%\wmux\server.log`）、环境变量（`WMUX`、`WMUX_PANE`、`WMUX_LOG`）、管道名格式与代码对照；`docs/acceptance.md` 各轮数字与当时测试输出对照。

数据：clippy 0 warnings；65 项全绿；`git status` 干净；`wmux --help` / `wmux -V` 输出正常。

发现：无。

## 第 10 轮（计数 3/3，无发现）— 视角：核心不变量的真实场景验证

做了什么：release 二进制，在隐藏控制台里 attach 一个 session，`Stop-Process -Force` 强杀客户端（等价于关掉终端窗口），再从 CLI 查询、发键、在另一个隐藏控制台重新 attach，最后 `kill-server`。

数据（原样）：
```
keep: 1 windows (created 1s ago) [120x30] (attached)
-- kill client --
keep: 1 windows (created 2s ago) [120x30]
0: [120x29] %2 C:\WINDOWS\system32\cmd.exe (active)
-- re-attach --
keep: 1 windows (created 5s ago) [120x30] (attached)
client2 exited after kill-server: True code=0
```
`cargo test` 65 项全绿；clippy 0。

发现：无。

## 结论（第一次验收）

第 8、9、10 轮连续零发现，验收通过。累计修复 27 项（第 1–4、6、7 轮），新增测试 21 项；最终 65 项自动化测试（单元 56、真实 ConPTY 2、管道级 e2e 7）。

---

# 第二次验收：Windows Terminal 真机演示后

触发：用户要求在 Windows Terminal 里实际运行。用 `SendKeys` 向真实 WT 窗口打键、截图、`capture-pane` 读回。

## 第 11 轮（不计数）— 视角：真机运行（Windows Terminal + 中文 IME）

发现（2 项代码问题 + 2 项脚本问题，全部已修）：

| # | 问题 | 修复 |
|---|------|------|
| 1 | **server 无声消失**。根因：vt100 0.16.2 `Row::resize` 在 pane 缩小时把宽字符（中文）截成半个留在最后一列，之后擦除走 `clear_wide(col)` 访问 `col+1` 越界 panic（`row.rs:89: len is 50 but the index is 50`）；panic 经 `block_on` 直接带走进程，什么都没记 | `vendor/vt100` 打补丁（`resize` 清掉半个宽字符；`clear_wide` 边界检查），`[patch.crates-io]` 接入；回归测试 `shrink_through_wide_char_then_erase_does_not_panic`（补丁前复现 panic，补丁后通过） |
| 2 | server 任何 panic 都无日志、且杀掉全部 session | `main` 装 panic hook 写 `server.log`；事件循环 `catch_unwind`，恢复后强制全量重绘 |
| 3 | 演示脚本 `GetWindowTextW` 未声明 Unicode 封送，标题只剩首字符，找不到/聚焦不了窗口 | `CharSet.Unicode` |
| 4 | 演示脚本 IME 探测对数组用 `-notmatch`，误判后按 Shift 把输入法切回中文 | 先 `-join` 再匹配 |

新增：`capture-pane -p [-S -N] [-t]`（tmux 同名命令，CLI 读 pane 文本；e2e 覆盖）；client attach 时设置控制台标题 `wmux: <session>`。

真机数据（补丁后重跑，`shots3/`）：探测行原样到达（`echo probe-abc xyz`）；`Write-Host` 输出正确；`prefix %`/`"` 得到 3 个 pane（50x29 / 49x14 / 49x14），WSL 在 pane 里跑出 `Linux DFINE 6.18.33.2-microsoft-standard-WSL2`；`prefix z` 状态栏出现 `*Z`；`prefix c` + `prefix ,` 得到 `1:renamed*`；`prefix ?` overlay `[28 of 49 lines] press any key`；`prefix d` 后 `ls` 显示 `demo: 2 windows`（未 attached）；日志无 panic。

IME 说明：WT 窗口输入法处于中文模式时，字母进入拼音合成、空格提交候选，这与任何终端一致，不是 wmux 的行为；演示脚本用 `WM_IME_CONTROL` 把该窗口切到英文。

## 第 12 轮（计数 1/3，无发现）— 视角：vendor 补丁审查 + 可复现性

做了什么：逐行审 `vendor/vt100/src/row.rs` 的两处改动；核对 `Row` 其余索引点（`erase`/`remove`/`truncate`）只用光标内坐标；确认 `blit_screen` 对最后一列半宽字符的处理。`cargo test` 连跑 3 次。

数据：3 次指纹均为 `A522E4B06BC4101C`（66 项：lib 57 / console 2 / e2e 7）；clippy 0。

发现：无。

## 第 13 轮（计数 2/3，无发现）— 视角：静态一致性

做了什么：`capture-pane` 在 README/`--help`/e2e 三处一致；`Cargo.toml` `[patch.crates-io]`、`vendor/vt100` 15 个文件入库、`cargo tree` 解析到 vendor 路径；README 说明补丁与 panic 日志；memory/acceptance 数字更新为 66。

数据：全量测试 66 项全绿。

发现：无。

## 第 14 轮（计数 3/3，无发现）— 视角：真机演示复跑（Windows Terminal）

做了什么：补丁后的 release 二进制在新的 WT 窗口重跑完整演示脚本（探测、打字、`%`/`"` 分屏、WSL、缩放、新窗口、`,` 重命名、`?` overlay、detach）。

数据：与第 11 轮补丁后一致——3 pane（50x29 / 49x14 / 49x14）、`1:renamed*`、WSL `uname -a` 输出在 pane 内、detach 后 `ls` 显示 session 未 attached 仍存活；`server.log` 无 panic。（脚本最后一张截图在 WT 关闭标签页后取窗口矩形失败，属脚本行为，非 wmux。）

发现：无。

## 结论（第二次验收）

第 12、13、14 轮连续零发现，验收通过。本次新增修复 4 项（1 项上游 crate 缺陷、1 项 panic 兜底、2 项演示脚本），新增测试 2 项；最终 66 项自动化测试。

---

# 第三次验收：插件化

新增：`src/format.rs`（状态栏格式引擎）、`run-shell`、`set-hook`/`show-hooks`、`@plugin`/`plugin-path`/`load-plugin`/`list-plugins`、`show-options`、状态栏 `status-left/right`/`window-status-*`/`status-interval`。

## 第 15 轮（不计数）— 视角：静态一致性

| # | 问题 | 修复 |
|---|------|------|
| 1 | `config.rs` 注释声称插件可用 `show-options` 读 `@` 选项，命令不存在（tmux 插件的标准做法 `show-option -gqv @x`） | 实现 `show-options [-gqv] [name]`，`Options::get` + `SHOWABLE` 表；单元 + e2e |
| 2 | `spawn_shell` 线程创建失败时静默，客户端永远等 `Pending` | 失败即回送 `ShellDone(127)` |
| 3 | `load-plugin <file>` 不去重 | 与目录路径同样去重；e2e 二次加载不重复 |

## 第 16 轮（不计数）— 视角：机制通路（release 二进制 + 真实插件目录）

| # | 问题 | 修复 |
|---|------|------|
| 1 | `run-shell` 子进程里 `wmux` 不在 PATH（未 `cargo install` 时插件回调失败） | pane 与 run-shell 环境的 PATH 前置 wmux.exe 所在目录（`path_with_self`，幂等） |
| 2 | pwsh 无控制台时按 ANSI 代码页输出，中文变乱码 | 脚本前置 `[Console]::OutputEncoding = UTF8` |
| 3 | `show-hooks` 输出嵌套引号未转义，不可再解析 | 用 `quote()`；e2e 把输出喂回 tokenizer 验证 |

数据：`run-shell 'wmux -L $env:WMUX display-message callback-ok'` 打印 `callback-ok`；`Write-Output 中文` 原样返回；hook 文件写入 `fired`。

## 第 17 轮（不计数）— 视角：边界输入 + 错误路径

| # | 问题 | 修复 |
|---|------|------|
| 1 | 格式串里未知的 strftime 字段（如 `%Q`）让 chrono 的 `Display` 返回 Err，`to_string()` panic | 用 `write!` 捕获错误，原样输出；测试 `malformed_formats_do_not_panic` 覆盖 10 种畸形串 |
| 2 | hook 命令内部 panic 时 `in_hook` 守卫永远为 true，之后所有 hook 失效 | `catch_unwind` 恢复分支重置 |
| 3 | 状态栏 `#(cmd)` 挂死时 `shell_running` 永远占位，该项再也不刷新 | job object + 30s 看门狗，超时杀进程树，退出码 124；测试 `run_shell_timeout_kills_the_tree` |
| 4 | （修 3 时引入）无超时分支把 job 提前 drop，命令刚启动就被杀 | job 生命周期延长到 `wait_with_output` 之后；`exit 3` 测试抓到 |

测试结果：lib 66 / console 2 / e2e 8，clippy 0。

## 第 18 轮（计数 1/3，无发现）— 视角：可复现性 + 并发路径审查

数据：`cargo test` 3 次指纹均为 `C9E57FBF0DC118C5`（76 项：lib 66 / console 2 / e2e 8）；clippy 0。
审查：`refresh_status_shells` 的 due/fresh/running 三态；`spawn_shell` 失败回送；`StatusShell` 到达即清 running；`status-interval 0` 夹到 1；线程与主循环只经 unbounded channel 交互。

发现：无。

## 第 19 轮（计数 2/3，无发现）— 视角：静态一致性

做了什么：README 插件章节的 9 个 hook 名与 `HOOKS` 常量逐个对照；`show-options -gqv`/`load-plugin`/`list-plugins`/`run-shell`/`set-hook` 在 README、`--help`、解析器三处一致；release 二进制加载 `wmux.conf.example`（含新格式串与 `#()`）。

数据：日志 `loaded wmux.conf.example (23 commands)`，无错误；`show-options -gv status-right/status-left/status-interval` 回显与文件一致；全量 76 项通过；工作树干净。

发现：无。

## 第 20 轮（计数 3/3，无发现）— 视角：机制通路（release 二进制 + 真实插件目录）

数据：`@plugin agent-status` 经配置加载 → `list-plugins` 列出目录；`show-options -gqv @agent-status-file` 回显；`run-shell` 内 `wmux -L $env:WMUX display-message callback-ok` 打印 `callback-ok`、中文原样；`exit 5` 映射为 `shell exited with 5` / CLI 退出码 1；`new-window` 触发 `after-new-window` hook 的后台 `run-shell -b` 写入 `fired`；日志无 panic/ERROR。

发现：无。

## 结论（第三次验收）

第 18、19、20 轮连续零发现，验收通过。本次新增修复 10 项（第 15–17 轮），新增测试 10 项；最终 76 项自动化测试。

---

# 第四次验收：重启后 resume、按 session 保存、vim 键位、synchronize-panes

新增：`src/resurrect.rs`（每 session 一个 JSON，原子写入）、自动保存（结构变化即存，`kill-server` 时再存）、`resume [name]`/`list-saved`/`save-session`/`restore-session`/`delete-saved`、`autosave`/`restore-on-start`/`sessions-dir`/`WMUX_SESSIONS_DIR`、`prefix h/j/k/l`+`H/J/K/L`+`Tab`、`set -w synchronize-panes`（`prefix S`，状态栏 `S` 标记）。

## 第 21 轮（不计数）— 视角：机制通路 + 不变量

| # | 问题 | 修复 |
|---|------|------|
| 1 | e2e 服务器的自动保存写进用户真实目录（`%LOCALAPPDATA%\wmux\sessions` 出现 8 个测试 session 文件） | `WMUX_SESSIONS_DIR` 环境变量；e2e Harness 与 console 测试统一指向临时目录；已清理污染文件；全量测试后真实目录 0 文件 |
| 2 | `rename-session` 后旧名字的保存文件残留，`list-saved` 出现幽灵条目，可被 resume 出陈旧副本 | 重命名时把保存文件一并改名；e2e 验证旧名消失 |
| 3 | `delete-saved` 对正在运行的 session 在下一个 tick 被自动保存撤销 | 运行中拒绝删除并提示先 kill；e2e 覆盖 |
| 4 | （测试自身）kill 掉唯一 session 后 server 按设计退出，测试 `connect()` 无超时死等 | 测试先建保活 session，`connect()` 加 10s 超时 |
| 5 | **主循环 tick 永不触发**：`select!` 里每次迭代新建 `sleep(1s)`，只要事件持续到来（客户端轮询、pane 持续输出）就一直重置；自动保存、空闲退出全挂在 tick 上，繁忙时静默失效。由 rename 测试的 200ms 轮询暴露 | 改为循环外的持久 `interval`（`MissedTickBehavior::Delay`） |

真机数据（release，`WMUX_SESSIONS_DIR` 临时目录）：建 `work`（2 窗口，首窗口 pwsh|wsl/cmd 三格）+ `scratch` → 2s 内出现两个保存文件 → `Stop-Process` 强杀 server（模拟重启）→ `ls` 报无 server → `wmux resume work` 自动起新 server，`list-panes` 尺寸/命令与杀前逐项一致（40x23 %pwsh、39x11 %wsl、39x11 %cmd），WSL pane 提示符目录 `/mnt/c/Users/stebe/Documents/dfine` → `wmux resume` 补回 `scratch`；日志无 panic。

## 第 22 轮（计数 1/3，无发现）— 视角：可复现性 + 静态一致性

数据：`cargo test` 3 次指纹均为 `2EA9C8924603ED88`（81 项：lib 69 / console 2 / e2e 10）；clippy 0；全量测试后真实 sessions 目录 0 文件；工作树干净。README / `--help` / `wmux.conf.example` 中 `resume`、`list-saved`、`synchronize-panes`、`sessions-dir`、`restore-on-start`、`autosave` 共 14 处提及，与解析器/选项表一一对应；README 按键表已改为 h/j/k/l、Tab、S、C-s/C-r。

发现：无。

## 第 23 轮（计数 2/3，无发现）— 视角：机制通路（release 二进制）

做了什么：建 `work`（pwsh | wsl）+ `keep`；客户端每 100ms 轮询 3 秒（制造第 21 轮第 5 项那种持续事件流）；`rename-session work work2`；`delete-saved keep`；强杀 server；`resume work2`。

数据（原样）：轮询期间两个 session 仍在 3s 内保存（`list-saved` 两条均 `(running)`）；rename 后 `list-saved` 只有 `work2:`，无 `work:`；`delete-saved keep` → `session keep is running; kill it first (or set autosave off)`，退出码 1；强杀后 `resume work2` → `restored 1 session(s)`，`list-panes` 为 `40x23 pwsh` / `39x23 wsl (active)`，与杀前一致；日志无 panic/ERROR/WARN。

发现：无。

## 第 24 轮（计数 3/3，无发现）— 视角：边界与退化输入（恢复路径）

核对：`active` 越界回落到首个 pane；`current` 越界夹到最后一个窗口；窗口列表为空报错；`argv` 为空回落默认 shell；已不存在的 `cwd` 被忽略；`sizes` 与子节点数不符时重置为等分；`.json.tmp` 残片被 `list()` 按扩展名忽略（原子写入被强杀也无损）；不同名字的文件名经 FNV 哈希后缀不碰撞（单元测试）；resume 已运行的 session 等价于 attach。

数据：全量 81 项通过。

发现：无。

## 结论（第四次验收）

第 22、23、24 轮连续零发现，验收通过。本次新增修复 5 项（含一个影响全局的 tick 饥饿 bug），新增测试 5 项；最终 81 项自动化测试。

---

# 第五次验收：pane 目录跟随（set-cwd / OSC 7 / OSC 9;9）

新增：`set-cwd [-t target] [dir]`（无参用调用方 cwd；无 `-t` 时优先 `WMUX_PANE` 所在 pane）；vt100 回调解析 OSC 7（`file://host/path`）与 OSC 9;9，`/mnt/<盘>/...` 映射回 Windows 路径，纯 Linux 路径忽略；`#{pane_current_path}`；`list-panes` 显示目录。

## 第 25 轮（不计数）— 视角：机制通路

| # | 问题 | 修复 |
|---|------|------|
| 1 | （测试自身）用 `echo` 在 cmd 里打 ESC 序列，ESC 键被 cmd 行编辑当作"清行"，OSC 根本没发出 | 改为 `pwsh -Command Write-Host ([char]27+...)`；证实 OSC 9;9 能穿过 ConPTY 到达 server |
| 2 | clippy 3 处（`?` 简化、`is_empty`、`+ 0`） | 修 |

## 第 26 轮（不计数）— 视角：真机 + 边界输入

真机数据（release，pwsh prompt 发 OSC 9;9）：pane 起始目录 `C:\Users\stebe` → `cd Documents` 后 `list-panes` 显示 `[C:\Users\stebe\Documents]` → 强杀 server → `resume w` → pane 提示符 `PS C:\Users\stebe\Documents>`；`set-cwd -t keep`（无参）记录为 CLI 的当前目录；日志无 panic。

| # | 问题 | 修复 |
|---|------|------|
| 1 | `set-cwd 相对路径` 相对 server 进程目录检查，而非调用方目录 | 相对路径拼到客户端 cwd 上；e2e 覆盖 |

## 第 27 轮（计数 1/3）— 视角：可复现性 + 静态一致性

（待填）
