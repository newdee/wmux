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

真机数据（补丁后重跑，`shots3/`）：探测行原样到达（`echo probe-abc xyz`）；`Write-Host` 输出正确；`prefix %`/`"` 得到 3 个 pane（50x29 / 49x14 / 49x14），WSL 在 pane 里跑出 `Linux HOST 6.18.33.2-microsoft-standard-WSL2`；`prefix z` 状态栏出现 `*Z`；`prefix c` + `prefix ,` 得到 `1:renamed*`；`prefix ?` overlay `[28 of 49 lines] press any key`；`prefix d` 后 `ls` 显示 `demo: 2 windows`（未 attached）；日志无 panic。

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

真机数据（release，`WMUX_SESSIONS_DIR` 临时目录）：建 `work`（2 窗口，首窗口 pwsh|wsl/cmd 三格）+ `scratch` → 2s 内出现两个保存文件 → `Stop-Process` 强杀 server（模拟重启）→ `ls` 报无 server → `wmux resume work` 自动起新 server，`list-panes` 尺寸/命令与杀前逐项一致（40x23 %pwsh、39x11 %wsl、39x11 %cmd），WSL pane 提示符目录 `/mnt/c/Users/me/proj` → `wmux resume` 补回 `scratch`；日志无 panic。

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

真机数据（release，pwsh prompt 发 OSC 9;9）：pane 起始目录 `C:\Users\me` → `cd Documents` 后 `list-panes` 显示 `[C:\Users\me\Documents]` → 强杀 server → `resume w` → pane 提示符 `PS C:\Users\me\Documents>`；`set-cwd -t keep`（无参）记录为 CLI 的当前目录；日志无 panic。

| # | 问题 | 修复 |
|---|------|------|
| 1 | `set-cwd 相对路径` 相对 server 进程目录检查，而非调用方目录 | 相对路径拼到客户端 cwd 上；e2e 覆盖 |

## 第 27 轮（计数 1/3，无发现）— 视角：可复现性 + 静态一致性

数据：`cargo test` 3 次指纹均为 `2C49C316E391BE33`（83 项：lib 71 / console 2 / e2e 10）；clippy 0；真实 sessions 目录 0 文件；`set-cwd`/`#{pane_current_path}`/OSC 在 README、`--help`、解析器、server、pane、format 六处共 19 次提及且一致。

发现：无。

## 第 28 轮（计数 2/3，无发现）— 视角：机制通路（release 二进制）

数据：在 `C:\Users\me` 下 `set-cwd -t w Documents` → `[C:\Users\me\Documents]`；无参 → `[C:\Users\me]`；不存在的目录 → `set-cwd: not a directory: ...`，退出码 1；自动保存文件随之更新；日志无 panic。

发现：无。

## 第 29 轮（计数 3/3，无发现）— 视角：边界输入（路径转换）

核对 `windows_path_from_announced` 的 12 个用例（单元测试）：`file:///C:/x`、`file://host/C:/x%20y`、`C:/x`、`D:`、`/mnt/c/x`、`/mnt/d`、`file://host/mnt/c/x` 全部映射为 Windows 路径；`/home/x`、`/mnt/wsl/x`、空串、`file://` 返回 None 保留旧值；`%zz` 非法百分号编码原样保留。全量 83 项通过；工作树干净。

发现：无。

## 结论（第五次验收）

第 27、28、29 轮连续零发现，验收通过。本次新增修复 1 项（相对路径解析），新增测试 2 项；最终 83 项自动化测试。

---

# 第六次验收：choose-tree 选择界面（prefix s / w）

新增：`choose-tree [-s|-w]`（别名 `choose-window` / `choose-session`，接受 tmux 的 `-Z/-N/-G/-r` 组合写法），
默认绑定 `prefix w` = `choose-tree -Zw`（所有 session 展开到窗口）、`prefix s` = `choose-tree -Zs`（只列 session）。
交互：`j`/`k`（或方向键、`C-n`/`C-p`）移动，`g`/`G`（或 Home/End）到头到尾，PgUp/PgDn 与 `C-f`/`C-b`/`C-d`/`C-u` 翻页，
数字键跳到带 `(n)` 标签的前 10 项，`Enter` 选中（切 session + 选窗口，走 `switch-client` 并触发 `after-select-window`），
`q`/`Esc`/`C-c` 取消；鼠标滚轮移动光标、单击把光标放到那一行。列表每帧按活的会话树重建，光标跟着条目身份走。

## 第 1 轮（不计数）— 视角：静态一致性

做了什么：对照新命令在解析器、默认绑定、`--help`、两份 README 的一致性；检查 `needs_server` 是否会因新命令误启服务端。

| # | 问题 | 修复 |
|---|------|------|
| 1 | picker 与 `:` 命令行、overlay 可同时存在，按键会喂给看不见的 prompt | 打开 picker 时清掉 prompt/overlay，保证同一时刻只有一个模态 |

## 第 2 轮（不计数）— 视角：机制通路（真实键盘路径）

做了什么：新增 `tests/console.rs::choose_tree_through_the_real_keyboard`，用真实 `wmux.exe` 在 ConPTY 里走
ConPTY → conhost → `ReadConsoleInputW` → win32 输入记录 → server 的完整链路，验证 j/k/g/G/Enter 真的生效。

| # | 问题 | 修复 |
|---|------|------|
| 1 | （测试自身）新窗口用默认 shell（pwsh），断言写成 `cmd`；断言里的 pane 标题在提权终端下多了 `管理员:` 前缀 | 测试显式指定窗口命令；标题断言改为不依赖提权前缀 |

## 第 3 轮（不计数）— 视角：逻辑正确性与不变量

做了什么：通读 `chooser_key`/`chooser_go`/`handle_mouse`/`render_client` 的新分支，逐条对照 copy mode 的既有语义。

| # | 问题 | 修复 |
|---|------|------|
| 1 | picker 把前缀键也吞了（copy mode 不是这样），`prefix d` 无法从 picker 里脱离，且 `C-b` 被当成翻页 | picker 改成和 copy mode 同级：先处理前缀，再交给模式 |
| 2 | overlay 画在 picker 底下，但按键归 overlay：用户看不到却按一下就"没反应" | 渲染顺序改为 picker 在下、overlay 在上 |
| 3 | 状态栏右侧（pane 标题）可以把整个窗口列表挤没——提权时 cmd 标题变成 `管理员: C:\WINDOWS\...`，60 列下 `[r] 0:cmd*` 消失 | `compose` 为当前窗口标签预留位置，宁可裁剪右侧；放不下时退回旧行为；新增单元断言 |

## 第 4 轮（不计数）— 视角：边界与退化输入

做了什么：新增 `choose_tree_degenerate_sizes_and_wide_names`：中文 session/窗口名（双宽）、20x3 与 1x1 终端、
游标下的 session 被 `kill-session` 掉。

| # | 问题 | 修复 |
|---|------|------|
| 1 | （测试自身）条目数算错（2 session + 2 窗口 = 4 项，写成 3） | 修断言 |

## 第 5 轮（不计数）— 视角：代码正确性（通读 diff）

做了什么：`git diff -- src` 全文复核，盯溢出、借用、panic 路径、确定性。

| # | 问题 | 修复 |
|---|------|------|
| 1 | 状态栏新算式 `x + need + 1` 是普通加法，极长窗口名会在 debug 下整数溢出 panic | 改 `saturating_add` |

## 第 6 轮（计数 1/3，无发现）— 视角：机制通路 + release 二进制

数据：`cargo test --release --all-targets` 89 项全过（lib 73 / console 3 / e2e 13）；
picker 专项 6 项单独跑全过（含真实 ConPTY 键盘路径）；release 二进制 `choose-tree` 未 attach 时退出码 1、
提示 `choose-tree: client not attached`。

## 第 7 轮（计数 2/3，无发现）— 视角：静态一致性终审

数据：release 二进制 `list-keys` 输出 `bind-key -T prefix s choose-tree -s` / `w choose-tree -w`（可被解析器原样回读）；
`choose-tree` / `choose-window` / `choose-session` / `-Zw` 四种写法均被接受，`-x` 报 `unknown flag '-x'` 且退出码 1；
两份 README 的按键表与"未实现"段、`--help` 三处口径一致；无残留的"列出 session / 窗口"旧说法。

## 第 8 轮（计数 3/3，无发现）— 视角：可复现性 + 无残留

数据：连续 3 次 `cargo test --all-targets` 结果逐项相同（73/0/3/13）；测试名指纹 `736D60A2B88738B0`（89 项）；
真实 `%LOCALAPPDATA%\wmux\sessions` 运行前后均为 2 个文件（未被测试污染）；测试进程无残留；
TEMP 下的 `wmux-*` 目录只剩手工调试留下的（已清），通过的测试自身不留目录。

观察（非缺陷）：`tests/console.rs` 的临时目录清理写在测试末尾，测试 panic 时会留下一个目录；e2e 侧走 `Drop` 不受影响。

## 结论（第六次验收）

第 6、7、8 轮连续零发现，验收通过。本次修复 5 项（含一个既有的状态栏挤压 bug），新增测试 6 项；最终 89 项自动化测试。

---

# 第七次验收：全屏下的方向切换 + 补齐 `-t`

起因：用户反馈“全屏的时候不能切换窗格”。确认是 bug——`select-pane -L/-R/-U/-D` 从 `w.rects` 找邻居，
而 zoom 时 rects 只剩铺满整窗的那一个矩形，于是永远找不到邻居，返回 `no such pane`；
`o` / `;` / `select-pane -t N` 走布局顺序，所以那几个在全屏下一直是好的。

修复：zoom 时用真实布局重新算一遍矩形来找邻居，选中后照常取消 zoom（和 `o`、`;` 的行为一致）。
回归测试 `zoomed_pane_still_navigates_by_direction` 先验证过“撤掉修复就超时失败”。

## 第 1 轮（不计数）— 视角：不变量

数据：`zoomed` 的 8 处写入点逐一核对，不变量“zoomed ⇒ 窗口里不止一个 pane，且 rects 由 relayout 重建”成立；90 项测试通过。

发现：无（本轮只做审查）。

## 第 2 轮（不计数）— 视角：机制通路（release 二进制）

| # | 问题 | 修复 |
|---|------|------|
| 1 | `resize-pane -t` / `swap-pane -t` 报 `unknown flag '-t'`，但 README 写着“每个窗口和 pane 命令都接受 `-t`” | 两个命令补上 `-t`（`swap-pane` 同时接受 `-s`） |
| 2 | `break-pane` 压根不解析 flag，`-t` 被静默吞掉，作用在当前 pane 上 | 正常解析 `-t`/`-s`，未知 flag 报错 |

## 第 3 轮（不计数）— 视角：边界（全屏下的隐藏 pane）

| # | 问题 | 修复 |
|---|------|------|
| 1 | 全屏时 `list-panes` 把被藏起来的 pane 报成 `[0x0]`（它其实还在原尺寸正常跑，`send-keys` + `capture-pane` 可证） | 改为显示 pane 自己的尺寸而不是布局矩形；e2e 补断言：隐藏 pane 仍是 40x23 且能收到按键 |

## 第 4 轮（不计数）— 视角：静态一致性（解析器口径）

| # | 问题 | 修复 |
|---|------|------|
| 1 | 8 个命令（`next/previous/last-window`、`paste-buffer`、`list-keys`、`list-plugins`、`clear-history`、`version`）不解析参数，`next-window -x` 之类的笔误被静默忽略 | 统一 `none_left` 校验；顺带给三个窗口切换命令加上 tmux 的 `-t <session>` |
| 2 | `version` / `help` 在 `main.rs` 里前置处理，`wmux version -v` 照样成功 | 多余参数报错退出 1；`tests/console.rs` 增加真实二进制的守卫测试 |
| 3 | MSI 构建脚本失败时会留下临时 stage 目录 | `try/finally` 清理，失败与成功两条路径都验证过 |
| 4 | README 没说清 `swap-pane` 的 `-s`/`-t` 只是“要交换的那个 pane”，没有 tmux 的成对形式 | 两份 README 的“未实现”段补上 |

## 第 5 轮（计数 1/3，无发现）— 视角：clippy + 全量

数据：`cargo clippy --all-targets` 零告警；93 项测试通过（lib 75 / console 4 / e2e 14）。

## 第 6 轮（计数 2/3，无发现）— 视角：机制通路（release 二进制走完所有改动路径）

数据：全屏时隐藏 pane 仍是 `[40x23]`；左右布局下 `select-pane -U` 正确地说“no such pane”且保持全屏，`-L` 则切走并取消全屏（尺寸回到 40/39）；
`resize-pane -L 10 -t w:0` 生效（30/49）；`next-window -t`、`break-pane -t` 均作用在指定目标上；
`next-window -x`、`paste-buffer junk`、`version -v`、`break-pane -x`、`swap-pane -q` 全部退出码 1。

## 第 7 轮（计数 3/3，无发现）— 视角：可复现性

数据：连续 3 次 `cargo test --all-targets` 结果逐项相同（75/0/4/14）；测试名指纹 `DE62C34C9AE61A79`（93 项）；
真实 sessions 目录仍是 2 个文件（用户自己的），测试与构建脚本均无临时目录残留。

## 结论（第七次验收）

第 5、6、7 轮连续零发现，验收通过。本次修复 7 项（1 项来自用户反馈，6 项为验收过程中发现），新增测试 3 项；最终 93 项自动化测试。

---

# 第八次验收：连按（`bind -r`）与 pane 内的 `wmux`

起因：用户反馈“ctrl b 之后 jkl 不能连续跳”。确认是缺失功能——`bind -r` 一直只是被解析器接受然后丢掉，
README 的“未实现”里也这么写着。另外在录制演示时发现：pane 里敲 `wmux list-panes` 连到了本机 `default` socket 上
**另一台** server（用户自己正在跑的那个），而不是管着这个 pane 的 server。

新增：
- `bind -r`：按一次前缀后，同一个键在 `repeat-time`（默认 500ms，设 0 关闭）内可以连着按。
  默认给 `h j k l`、`H J K L`、方向键、`C-`/`M-` 方向键、`n p`、`o`、`{ }` 都开了。
- 客户端默认 socket 取自环境变量 `WMUX`（tmux 用 `$TMUX` 是一个道理），所以 pane 或插件脚本里
  `wmux ls` 直接就是“这台 server”。

## 第 1 轮（计数 1/3，无发现）— 视角：静态一致性 + 全量

数据：clippy 零告警；95 项（lib 75 / console 4 / e2e 15 / 录制 1 个 ignored）；
`list-keys` 输出 10 条 `-r` 绑定且能被解析器回读；`show-options -gv repeat-time` 默认 500，`set` 后为 250；
两份 README 的按键表、配置示例、“未实现”段与实现一致。

## 第 2 轮（计数 2/3，无发现）— 视角：机制通路（release 二进制）

数据：pane 里 `wmux ls > 文件` 写出的是本 server 的 `s: 1 windows`（修复前会列出另一台 server 的 session）；
`bind -r C-y select-pane -U` 回读为 `bind-key -r -T prefix C-y select-pane -U`；`set -g repeat-time 0` 退出码 0。
真实键盘路径在 `tests/console.rs` 里验证：ConPTY 中发 `C-b h` 再发一个裸 `h`，活动 pane 从 2 跳到 0。

## 第 3 轮（计数 3/3，无发现）— 视角：可复现性

数据：连续 3 次 `cargo test --all-targets` 结果逐项相同（75/0/4/0/15）；测试名指纹 `3126F1F62B3250D4`；
真实 sessions 目录仍是用户自己的 2 个文件。

## 结论（第八次验收）

三轮连续零发现，验收通过。本次新增 2 项能力，新增测试 3 项（1 个单元、1 个 e2e、1 个真实键盘断言）；
最终 94 项自动化测试 + 1 个按需运行的演示录制。

---

# 第九次验收：`frame too large` 的真因 + `pane-base-index`

起因：用户之前报过 `wmux` / `wmux attach` 直接输出 `[frame too large: 538976288]`。这次查到根因。

538976288 = `0x20202020`，四个空格。客户端把**屏幕内容当成了帧长**，也就是帧流错位了。排查顺序与结论：

1. 老客户端（安装版 0.1.0）连新 server、新客户端连老 server，`ls` 与 `list-panes` 都正常，**版本不兼容排除**。
2. 服务端每条连接只有一个写任务，客户端的写也都串行在一个 `select!` 里，**并发写排除**。
3. 真因在 `src/client.rs` 的 attached 循环：`m = read_frame::<_, ServerMsg>(rd)` 直接写在 `tokio::select!` 的分支里。
   `read_frame` 是「先读 4 字节头，再读 body」两次 await，而 **`read_exact` 不是 cancellation-safe**：
   只要同一轮里按键分支或那个 500ms 的尺寸轮询先就绪，这个 future 就在两次读之间被丢弃，4 字节头已经从管道里消费掉、body 还留着。
   下一轮就把 body 的前 4 字节当帧头。用户那台 session 是 208x55，attach 时首帧整屏重绘很大、必然分多次到达，屏幕上又全是空格，于是读出 `0x20202020`。

修复：帧读取移到独立任务，用有界 channel（256）交给 `select!`，channel 的 recv 是 cancellation-safe 的。
回归测试 `frames_survive_a_select_loop`：在 duplex 上把每帧拆成「头 / 停 2ms / body」发送，同时有 1ms 的 ticker 抢占。
把读帧改回内联写法，测试立刻失败（`deserialize frame: io error: unexpected end of file`）。

同时发现并修复：`pane-base-index` 一直在「接受但忽略」名单里（而 `base-index` 是生效的），
所以 `set -g pane-base-index 1` 之后 pane 仍从 0 编号。现在 `list-panes`、`-t session:window.pane` 解析、`select-pane -t N`、`#P` 四处统一。

## 第 1 轮（计数 1/3，无发现）— 视角：静态一致性 + 全量

数据：clippy 零告警；97 项（lib 76 / console 4 / e2e 16 / 录制 1 个 ignored）；`show-options -gv pane-base-index` 默认 0；两份 README 的配置示例同步更新。

## 第 2 轮（计数 2/3，无发现）— 视角：机制通路（release 二进制）

数据：`set -g pane-base-index 1` 之后 `list-panes` 编号为 1 和 2；`send-keys -t s:0.1` 退出码 0；`-t s:0.0` 报 `no pane 0`；`show-options` 读回 1。

## 第 3 轮（计数 3/3，无发现）— 视角：可复现性

数据：连续 3 次 `cargo test --all-targets` 结果逐项相同（76/0/4/0/16）；测试名指纹 `C6F18B7C36796B3F`。

## 结论（第九次验收）

三轮连续零发现，验收通过。本次修复 2 项（其中一项是用户报过的帧错位真因），新增测试 2 项；最终 96 项自动化测试。
