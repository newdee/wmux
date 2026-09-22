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

---

# 第十次验收：补齐缺的 tmux 功能

用户要求「缺的功能都加了吧」，另外点名要命令简写（`tmux att` 那种）。本次新增：

- **命令名前缀匹配**：任何不产生歧义的前缀都当完整命令名用（`att`、`lsp`、`splitw`、`choose-t`）。
  有歧义就报错并列出候选（`kill` → kill-pane / kill-server / kill-session / kill-window），不猜。
- **`select-layout`**：五种命名布局（even-horizontal、even-vertical、main-horizontal、main-vertical、tiled），
  名字同样支持前缀（`til`），`-n` / `-p` 轮换，默认绑定 `prefix Space`。
- **copy mode 搜索**：`/` 往回找、`?` 往前找、`n` / `N` 重复，大小写不敏感，命中后把该行放到视野中间；找不到时状态栏提示。
- **`display-panes`（`prefix q`）**：每个 pane 中间显示大号编号，按数字直接跳过去，其他键关掉。
- **`swap-window` / `move-window`**：同一 session 内重排窗口，当前窗口跟着走。
- **格式串条件 `#{?cond,a,b}`**：条件可以是变量名，也可以是 `var==值` / `var!=值`；分支本身还能再套格式和样式。
- **`set -a`**：往选项原值后面追加（`set -ag status-right " | x"`），组合写法 `-ag` 也认。
- **主题**：`themes/` 下四套配色（Nord、Gruvbox dark、Dracula、Catppuccin Mocha）。
  按之前的判断没有新造 `set theme`，它们就是普通的命令文件，`source-file` 加载即可。

## 第 1 轮（不计数）— 视角：clippy + 全量

| # | 问题 | 修复 |
|---|------|------|
| 1 | clippy 两处（`n % cols == 0` 可用 `is_multiple_of`；`display-panes` 的嵌套 `if` 可合并） | 修 |

## 第 2 轮（不计数）— 视角：机制通路（release 二进制逐条跑新命令）

| # | 问题 | 修复 |
|---|------|------|
| 1 | `display-message -p "#{?window_flags,busy,idle}"` 原样输出。根因：`display-message`、`command-prompt`、`confirm-before` 走的是一个只会替换 `#S/#W/#P/#T/#I` 的简化函数，没走状态栏那套完整格式引擎 | `expand_format` 改为构造 `format::Context` 后调用 `format::expand`，`#{...}`、`#{?...}`、`#(...)`、`%H` 在所有地方口径一致 |

## 第 3 轮（计数 1/3，无发现）— 视角：clippy + 全量

数据：clippy 零告警；103 项测试通过（lib 80 / console 4 / e2e 19）。

## 第 4 轮（计数 2/3，无发现）— 视角：机制通路（release 二进制）

数据：`select-layout -t s main-v` 得到 39/40 两列；`swap-window -s s:0 -t s:1` 后窗口顺序与 `*` 标记都跟着换；
`display-message -p "#{?window_flags,busy,idle}"` 输出 `busy`；`set -g X` + `set -ag Y` 读回 `XY`；`lsw` 退出码 0。

## 第 5 轮（计数 3/3，无发现）— 视角：可复现性

数据：连续 3 次 `cargo test --all-targets` 结果逐项相同（80/0/4/0/19）；测试名指纹 `677DD406E21AB251`（104 项，含 1 个按需的录制）。

## 结论（第十次验收）

第 3、4、5 轮连续零发现，验收通过。本次新增 8 项能力，修复 3 项（含一处格式引擎口径不一致），
新增测试 5 项；最终 103 项自动化测试。

---

# 第十一次验收：照着 tmux 手册逐条补齐

用户要求「仔细读 tmux 官方手册，对着里面的命令和快捷键一条条实现」。

做法：先把 tmux master 的 `tmux.1`、`cmd.c`（命令表）、`key-bindings.c`（默认绑定表）、
`options-table.c` 抓到本地，据此写出 `docs/tmux-parity.md`——tmux 的每条命令、每个默认前缀键、
copy mode 的按键，逐条标注 wmux 是「有 / 部分 / 待做 / 明确不做（附理由）」，并排出实现顺序。
然后按那个顺序实现。

本次新增（命令 27 条，默认绑定从 47 涨到 81）：

- **命令名前缀匹配**（`att`、`lsp`、`splitw`），有歧义时列出候选而不是猜。
- **小命令**：`rotate-window`（`C-o` / `M-o`）、`next-layout` / `previous-layout`、`refresh-client`（`r`）、
  `last-pane`、`list-clients`、`list-commands`、`send-prefix`、`show-messages`（`~`）、
  `set-environment` / `show-environment`、`respawn-pane` / `respawn-window`、`detach-client -a/-s`、
  `switch-client -l`、`M-1`..`M-5` 直选布局。
- **粘贴缓冲区**：`set-buffer`、`list-buffers`（`#`）、`show-buffer`、`delete-buffer`（`-`）、
  `save-buffer`、`load-buffer`、`paste-buffer -b/-p`、`choose-buffer`（`=`）。copy mode 复制时
  同时进缓冲区和 Windows 剪贴板。
- **pane / window 搬运**：`join-pane` / `move-pane`（含「标记 pane」`select-pane -m` / `-M`）、
  `resize-pane -x/-y`（支持百分比）、`select-pane -T`、`find-window`（`f`）。
- **copy mode**：`w` `b` `e`（及 `W` `B` `E`）、`^`、`H` `M` `L`、`{` `}`、计数（`3j`）、
  `C-v` 矩形选择。
- **其他**：`clock-mode`（`t`）、`if-shell`（`-F` 用格式串，否则看 shell 退出码）。

## 第 1 轮（不计数）— 视角：机制通路（自测新命令）

| # | 问题 | 修复 |
|---|------|------|
| 1 | `respawn-pane -k` 之后 pane 一片空白。根因：老进程的退出事件晚到，服务端按 pane id 把**刚起来的**新 pane 删掉了 | pane 加代次号，`PaneEvent` 带上代次，非当前代次的输出和退出一律忽略；e2e 覆盖 |
| 2 | `select-pane -m -t j:1` 报 `bad target`：`-t` 的值被无条件当成 pane 选择器解析 | 带 `-m`/`-M`/`-T` 时 `-t` 作为目标解析；两种都不匹配才报错，并说明可用写法 |
| 3 | （测试自身）用 pane 尺寸判断 `rotate-window` 是否生效，但旋转恰恰保持几何不变 | 改为比较各位置上的 pane id，并额外断言几何不变 |
| 4 | （测试自身）`switch-client -t` 从 CLI 调用时作用在未 attach 的 CLI 客户端上 | 测试改从会话内部驱动（`prefix )` 和 `:switch-client -l`） |

## 第 2 轮（不计数）— 视角：静态一致性

| # | 问题 | 修复 |
|---|------|------|
| 1 | 默认绑定里有两条的 Display 输出没人验证过能否被解析器读回 | 新增单元测试 `default_bindings_round_trip`：81 条绑定逐条 Display → tokenize → parse → Display 必须完全一致 |
| 2 | `choose-session`、`choose-window`、`version` 三个命令没出现在对照表里 | 归入「wmux 独有」一行并说明 |

## 第 3 轮（计数 1/3，无发现）— 视角：静态一致性

数据：clippy 零告警；109 项测试通过（lib 81 / console 4 / e2e 24）；
`list-commands` 的 80 条命令全部能被解析器接受，且全部在对照表里有条目；`list-keys` 81 条。

## 第 4 轮（计数 2/3，无发现）— 视角：机制通路（release 二进制）

数据：`rotate-window`、`next-layout`、`join-pane -s/-t`（跨窗口搬运后 pane 数 1→3）、
`set-buffer` / `list-buffers` / `show-buffer`、`set-environment` / `show-environment`、
`if-shell` 两个分支、`resize-pane -x 30`（得到 `[30x23]`）、`clock-mode`、
`find-window` 未命中的报错，全部符合预期。

## 第 5 轮（计数 3/3，无发现）— 视角：可复现性

数据：连续 3 次 `cargo test --all-targets` 结果逐项相同（81/0/4/0/24）；
测试名指纹 `3D9A118BF99F31FA`（110 项，含 1 个按需的录制）；
真实 sessions 目录未被测试污染；测试结束无残留进程。

## 结论（第十一次验收）

第 3、4、5 轮连续零发现，验收通过。本次新增 27 条命令、34 个默认绑定、copy mode 一整套 vi 动作，
修复 2 个产品缺陷（respawn 的代次竞态、`select-pane -t` 的目标解析），新增测试 6 项；
最终 109 项自动化测试。

# 第十二次验收（2026-09-21）— 排队中的 tmux 功能 + 后台任务与持久化

本次做的：把 `docs/tmux-parity.md` 里排队的功能全部做完——`send-keys -X`、
`display-menu` / `display-popup`、`choose-client`、`pipe-pane`、`wait-for`、
`select-layout -E`、跨 session 的 `move-window`、`capture-pane -e`、
告警标记（`monitor-activity` / `monitor-bell` / `monitor-silence` /
`visual-bell` / `visual-activity`、`#` `!` `~` 三个 `#F` 标记、`M-n` / `M-p`），
外加用户直接问到的两件事：`remain-on-exit`（程序退出后 pane 留着）和
`save-history`（每个 pane 的输出随 session 存盘、`resume` 时贴回屏幕）。
同时把 copy mode 的 `/` 和 `?` 方向改成和 tmux 一致（`/` 往新、`?` 往旧）。

## 第 1 轮（不计数）— 视角：静态一致性

| # | 问题 | 修复 |
|---|------|------|
| 1 | `Pipe.dropped` / `Pipe.command` 只写不读：pipe-pane 的下游跟不上时会**静默丢输出** | `pipe_write` 在第一次丢字节时返回命令名，服务端写 `server.log` 并进 `show-messages`；缓冲区从 256 降到 64 块（≤4 MiB） |
| 2 | 两处 `messages.push_back` 没有裁剪，`show-messages` 可无界增长 | 统一走新的 `note_message`，固定保留最近 100 条 |

## 第 2 轮（不计数）— 视角：机制通路（配置项左右都拨一次）

| # | 问题 | 修复 |
|---|------|------|
| 1 | 没有客户端 attach 时，窗口变成当前窗口后**仍留着 `#` 标记**（`0: cmd*#`），因为清除只发生在渲染路径 | 清除挪进每帧都跑的 `sweep_alerts`：不变量「session 的当前窗口永远没有告警标记」只有一处执法；补 e2e（detached session 也验证） |

## 第 3 轮（不计数）— 视角：边界与退化输入

| # | 问题 | 修复 |
|---|------|------|
| 1 | 脚本里 `wmux display-popup -C` 只关「自己这个 CLI 客户端」的弹窗（等于什么都没干） | 调用方自己没有弹窗时，关掉该 session 上所有客户端的弹窗；新增 e2e |

## 第 4 轮（不计数）— 视角：代码正确性（错误路径 / 资源 / 并发）

| # | 问题 | 修复 |
|---|------|------|
| 1 | 弹窗吞掉了按键的 key-up（`swallow_up`），与普通 pane 的输入路径不一致 | 只有「已结束的弹窗被任意键关掉」那次才吞 key-up；其余 down/up 都照常转发 |
| 2 | 跨 session 的 `move-window` 用下标记录当前窗口：窗口插到前面时，**目标 session 正在看的窗口被换掉**；源 session 同理 | 两边都按窗口 id 记住「原来在看哪个」，搬完再按 id 找回下标；新增 e2e，并把修复临时撤掉验证过该测试会失败（`b still looks at the same window: 0: travels`） |
| 3 | 弹窗只在客户端 resize 时重新定位，`set -g status off/on` 改变窗口区域后会画歪 | `refit_popup` 改成每帧渲染前跑一次（`Pane::resize` 本身对尺寸不变是空操作） |
| 4 | `pipe_to` 启动失败时旧 pipe 还在跑；`self.clients[&cid]` 直接索引会 panic | 先停旧 pipe 再启动新的；索引改成守卫取值 |

## 第 5 轮（不计数）— 视角：静态一致性（文档 claim vs 代码）

| # | 问题 | 修复 |
|---|------|------|
| 1 | 新命令只在 `--help` 和 `list-commands` 里出现，两个 README 都没提 `choose-client` / `display-menu` / `wait-for`；对照表没有 options 一节 | 两个 README 加「脚本常用命令」小节（6 条）；对照表补 Options 一节（29 项逐条对齐 `show-options`） |

## 第 6 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制）

数据（`target/release/wmux.exe`，8 项全部符合预期）：
1. 无任何客户端 attach 时 `send-keys -t one:0 -X search-backward/begin-selection/next-word-end/copy-selection` → `show-buffer` = `alpha`；
2. `pipe-pane` 把 pane 输出写进文件（含 `piped-line`），空命令停止后文件落盘；
3. 另一个进程 `wait-for chan` 阻塞，`wait-for -S chan` 后返回 `woken:0`；
4. `move-window -s one:1 -t two:1` 后 `two = 0: cmd* | 1: mover`（当前窗口未被顶掉）；
5. `select-layout -E`：`[60x23] / [19x23]` → `[39x23] / [40x23]`；
6. `capture-pane -p` 不含 ESC，`-e` 含 `ESC[31m`；
7. `remain-on-exit on` 时 `cmd /c exit 7` 的窗口仍在，`show-messages` 有 `pane %10 (cmd) exited with 7`；
8. `choose-client` / `display-menu` / `display-popup` 从脚本调用时 exit=1 并说明 `client not attached`。
同轮 `cargo test`：126 项全绿（lib 86 / console 4 / e2e 36）。

## 第 7 轮（计数 2/3，无发现）— 视角：可复现性

数据：连续 3 次 `cargo test`，126 项测试名+状态指纹均为 `6ECB65FD1BC6C558`；
三个全新 server 的 `list-keys` 各 87 行，指纹均为 `F21C2959A7E1855A`
（含两条 282 字符的 `display-menu` 绑定，说明菜单定义的 Display→parse 往返稳定）。

## 第 8 轮（计数 3/3，无发现）— 视角：静态一致性

数据：7 个新配置项 set→`show-options -gv` 全部一致，非法值被拒且旧值保留；
`list-commands` 85 条命令逐条调用，没有一条落到 `unknown command`；
9 个新功能关键词在 README.md / README.zh-CN.md / docs/tmux-parity.md 三处均有；
对照表 `todo` 行数 0；`Window::flags` 的 7 个标记字符齐全；
`show-options` 的 29 个选项名在对照表里全部有条目；clippy 与 `cargo fmt --check` 均无输出。

## 结论（第十二次验收）

第 6、7、8 轮连续零发现，验收通过。本次补齐 tmux 3.5 命令表里最后的空缺
（`display-popup` 之后对照表已无 `todo` 行），新增 5 个告警类选项和 2 个持久化选项，
修复 8 个自测中发现的缺陷（其中「跨 session move-window 顶掉当前窗口」「弹窗吞 key-up」
「pipe-pane 静默丢输出」三个会被用户直接撞上）；测试从 109 项增加到 126 项
（lib 86 / console 4 / e2e 36）。

# 第十三次验收（2026-09-22）— 选项名缩写与开关切换

用户提的：`prefix :` 之后能不能像 `set sync` 这样写，直接开同步。当时只有命令名
支持前缀缩写（`set` = `set-option`），选项名必须写全，而且除了 `synchronize-panes`
以外，开关类选项不给值就报 `bad boolean ''`。

做了两件事：选项名支持不产生歧义的缩写（整体前缀 `sync`，以及按 `-` 分段各写前缀
`mon-act`、`w-s-f`），开关类选项不给值就翻转（`set mouse`、`set sync`）。名字在解析期
就展开成全名，所以 server、`list-keys`、配置文件三处看到的都是同一个名字。

## 第 1 轮（不计数）— 视角：逻辑正确与不变量

新增单元测试 `known_option_names_line_up_with_the_setter`：缩写表里的每个名字都必须
解析回自己、都必须被 `set` 认识、`SHOWABLE` 和开关表必须是缩写表的子集。

| # | 问题 | 修复 |
|---|------|------|
| 1 | `synchronize-panes` 能 set、能 `show -gv`，却不在 `show-options` 的列表里——能设却看不见 | 列表末尾按当前窗口补一行 |

## 第 2 轮（不计数）— 视角：机制通路（release 二进制）

| # | 问题 | 修复 |
|---|------|------|
| 1 | `set hist 1000` 报歧义（`history-limit` vs `history-file`），而后者只是为了让 `.tmux.conf` 能加载而"接受并忽略"的名字 | 缩写表拆成 `KNOWN`（真正生效的 35 个）和 `ACCEPTED`（兼容用的 15 个）；先在生效的里面找唯一匹配，兼容名字永远抢不走缩写，写全名仍然被接受 |

## 第 3 轮（不计数）— 视角：边界与退化输入

| # | 问题 | 修复 |
|---|------|------|
| 1 | `set ""` 列出全部 35 个候选（空串是所有名字的前缀），`set -` 列出 22 个 | 空名字、含空段的名字一律不当作缩写，直接落到 `unknown option` |
| 2 | 歧义提示最长列了 13 个名字 | 最多列 4 个，其余写成 `and N more` |

## 第 4 轮（计数 1/3，无发现）— 视角：机制通路

数据（release 二进制，16 条）：`set sync` → `show -gv sync` = on；`set -w syn on`；
`set mon-act on`；`set w-s-f #I:#W`；`set mou` 翻转；`set -ag s-r " | tail"` 追加成功；
`set mon` / `set s-p` 报歧义并列出候选；`set hist 1000` 成功；
`show-options` 列出 30 行且含 `synchronize-panes`。

## 第 5 轮（计数 2/3，无发现）— 视角：边界与退化输入

数据：`set`（无名字）→ `option name required`；`set ""` / `set -` → 干净的
`unknown option`；`set s on` → 歧义且只列 4 个 + `and 9 more`；`set SYNC` → unknown
（大小写敏感）；`@theme` 设/读正常；`set -a mou`、`set sav`（数字选项不翻转，报
`bad number ''`）符合预期；整份配置文件用缩写写成（`set mou on` / `set mon-act on` /
`set hist 4321` / `set sav 42`）在 server 启动时全部生效；`source-file` 里的歧义带
文件名和行号报出来。

## 第 6 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，128 项测试名+状态指纹均为 `805519A61590AEF6`；
README 和 `--help` 里承诺的 6 个例子（`set sync`、`set mon-act on`、`set w-s-f`、
`set mouse` 翻转、`set mon` 的歧义提示）逐条对着 release 二进制实测，全部相符；
clippy 与 `cargo fmt --check` 无输出。

## 结论（第十三次验收）

第 4、5、6 轮连续零发现，验收通过。测试从 126 增加到 128（lib 87 / console 4 / e2e 37）。

# 第十四次验收（2026-09-22）— 跨 pane 搜索、桌面通知、命令表补漏

起因：查了 psmux（3.5k star，90+ 命令、140+ format 变量、30 hooks、control mode、
monitor-* 和 remain-on-exit 一应俱全）之后确认，在"追 tmux 兼容"这条路上它已经比
wmux 全，继续追只是做第二个 psmux。所以改做两边都没有的东西。

本次新增：
- `find-text [-C] [-n 条数] [-t 目标] 关键词`：在**所有 pane 打印过的内容**里找，
  输出 `session:窗口.pane  -往回第几行  命中行`。tmux 的 `find-window` 只搜窗口名和
  标题，psmux 的 copy-mode 搜索只能在单个 pane 里。
- `notify [-T 标题] 消息` 和 `set -g notify on`：托盘气泡通知（Win10/11 会转成系统
  通知）。选托盘气泡而不是 WinRT toast，是因为 toast 需要安装快捷方式注册
  AppUserModelID，免安装 zip 版给不了。
- 命令表补漏：`copy-mode` / `unbind-key` 能解析却不在 `COMMANDS` 里（`wmux unb` 报
  unknown，`list-commands` 也看不到）；另补 `setw` / `showw` / `start-server` 三个
  tmux 用户常打的名字。命令数 85 → 92。

## 第 1 轮（不计数）— 视角：静态一致性

| # | 问题 | 修复 |
|---|------|------|
| 1 | `copy-mode`、`unbind-key` 在别名表里能解析，却没进 `COMMANDS` 常量：前缀解析不到、`list-commands` 不显示 | 补进常量，并新增单元断言：`COMMANDS` 每一项都必须能被解析且 `resolve_prefix` 解析回自己 |

## 第 2 轮（不计数）— 视角：边界与退化输入

| # | 问题 | 修复 |
|---|------|------|
| 1 | `find-text " "`（纯空白）被当成有效搜索，会命中几乎所有行 | pattern 去掉首尾空白后为空就报 `pattern required` |
| 2 | `-t b:99`（不存在的窗口）只报 `no pane has: xxx`，看不出是目标写错了 | 目标按 session → 窗口 → pane 逐层解析，各报各的错（`no window 99` / `no pane 5`） |
| 3 | `-t b:0.5` 的 **pane 部分被完全忽略**，实际搜了整个窗口 | 解析 pane 并只搜那一个 |
| 4 | 找不到时把整条 pattern 原样回显，300 字符的 pattern 刷屏 | 错误信息里截断到 60 字符 |

## 第 3 轮（计数 1/3，无发现）— 视角：机制通路 + 性能

数据（release 二进制）：4007 行 scrollback 的 pane 里命中最顶上那行 **17 ms**；
跨 session 搜索标签与对齐正确（`big:0.1  - 8` / `other:0.0  -20`）；
`-t big` 限定 session、`-t :1` 解析成当前 session 的窗口 1、`-t nosuch` 报
`can't find session`；4000 行都匹配的常见 pattern 因 `-n` 默认 3 只返回 3 条，16 ms。

## 第 4 轮（计数 2/3，无发现）— 视角：边界与退化输入

数据：空 pattern / 纯空格都报 `pattern required`；`a.*b[c] \` 按字面匹配（不是正则）；
中文、日文、emoji 都能命中；`-n abc`、`-x` 报错；200 字符 pattern 的错误信息被截断；
`-t b:99` → `no window 99`，`-t b:0.5` → `no pane 5`；没有输出的 pane 正常报未命中；
`notify` 空消息报错、400 字符消息和 200 字符标题都不崩、连发 10 条耗时 165 ms。

## 第 5 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，131 项指纹均为 `BE06DD36FD5D73A4`；
README 承诺的输出形状 `ft:0.0  -8  text` 用正则对着真实输出校验通过；
`list-commands` 92 条且含 `find-text` / `notify`；`show -gv notify` 可读；
clippy 与 `cargo fmt --check` 无输出。

## 结论（第十四次验收）

第 3、4、5 轮连续零发现，验收通过。测试 128 → 131（lib 89 / console 4 / e2e 38）。
桌面通知的视觉效果需要在有桌面的会话里人工确认（开发机跑在 session 0，看不到气泡）。

# 第十五次验收（2026-09-22）— 直读 .tmux.conf、开机自启

本次新增：
- 没有 `.wmux.conf` 时直接读 `~/.tmux.conf` / `~/.config/tmux/tmux.conf`，按 tmux 的读法：
  `\` 续行、`%if … %endif` 整块跳过（wmux 不求值 tmux 条件式，两个分支都应用比都不应用更糟）、
  用不了的行记进 `show-messages`，首次 attach 只给一行汇总。`bind -T` 只认 root / prefix，
  `copy-mode-vi` 这种表直接拒绝——**修了一个老 bug**：以前 `-T` 非 root 一律当 prefix，
  `.tmux.conf` 里的 `bind -T copy-mode-vi v …` 会被静默绑到 prefix 表。
- `wmux startup on|off|status`：登录时拉起 server 并恢复全部 session。用当前用户的 `Run`
  注册表键（`schtasks` 的 ONLOGON 触发器非管理员建不了，实测 `Access is denied`），
  命令前面套 `conhost --headless` 所以登录时不闪窗口；server 新增 `--restore` 参数。
- `server::run_with(RunOptions { force_restore, config })`：测试可以显式指定配置文件，
  不再通过环境变量——同进程并行的 e2e 曾因 `WMUX_CONFIG` 互相污染挂掉 4 个。

## 第 1 轮（不计数）— 视角：机制通路

| # | 问题 | 修复 |
|---|------|------|
| 1 | `conhost --headless` 用 `<nul` 喂 stdin 时 server 起不来，误以为机制不可用 | 按登录的真实方式（ShellExecute、不继承句柄）再测：server 起来、session 连输出一起回来、conhost 无窗口。方案没错，测法错了 |
| 2 | 我之前的临时探测没设 `WMUX_SESSIONS_DIR`，往用户真实 sessions 目录写了 14 个垃圾 session，`--restore` 一跑全冒出来 | 逐个核对名字后删除（全是我的：0/1/2/alpha/b/big/c/doc/k/other/s/t/t1/x），之后的探测脚本全部隔离目录 |

## 第 2 轮（不计数）— 视角：边界与退化输入

| # | 问题 | 修复 |
|---|------|------|
| 1 | 配置文件 `source-file` 自己 → 无限递归，**server 直接死** | 正在 source 的文件栈（canonical 路径）做环检测，报 `source-file loop`；前面已应用的行保留 |
| 2 | 没闭合的 `%if` 把后面整个文件静默吞掉 | `logical_lines` 返回未闭合的行号，报 `%if without %endif; the rest of the file was skipped` |
| 3 | 记事本存的文件带 UTF-8 BOM，第一行变成 `\u{feff}set` 报 unknown command | 读文件先剥 BOM；单元测试覆盖 BOM + CRLF |

## 第 3 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制）

数据：真实形状的 tmux.conf（含 TPM、copy-mode-vi、`%if`、续行）经 `WMUX_CONFIG` 加载：
`prefix=C-a mouse=on base-index=1 status=on`；续行拼成的 `| split-window -h -c "#{pane_current_path}"`
绑定存在；prefix 表里没有 `v`；`show-messages` 里有 copy-mode-vi 与 `@plugin tmux-plugins/tpm` 两条；
`list-windows` 首行 `1: pwsh*`。`startup on/status/off` 往返，Run 值最终不存在。
登录链路（隔离目录）：保存 `ops`（2 窗口）→ kill-server → 按登录方式启动 → `ls` 见 `ops: 2 windows`，
pane 0 里 `survived-the-reboot` 仍在，conhost `MainWindowHandle=0`。

## 第 4 轮（计数 2/3，无发现）— 视角：边界与退化输入

数据（9 份退化配置，server 全部存活）：BOM+CRLF 生效；空文件；只有 `%if` 块；未闭合 `%if`（1 条提示）；
EOF 处续行；纯垃圾（2 条提示）；`bind -T copy-mode` / `unbind -T copy-mode-vi`（各拒绝 1 条，`-T root` 通过）；
`%hidden`/`%elif`；自引用 `source-file`（1 条提示，`mouse off` 已生效）。
`startup` 八种写法：on/install/off/disable 退出码 0，未安装时 status 退出码 1，`weird` 报错。

## 第 5 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，136 项指纹均为 `ADF37BBFA26A6E3D`；配置搜索路径含两个 tmux 位置；
两份 README、`--help`、对照表都提到 `startup` 与 `.tmux.conf`；
"登录不闪窗口"由代码里的 `conhost.exe --headless` 支撑，"不需要管理员"由只写 `HKEY_CURRENT_USER` 支撑；
clippy 与 `cargo fmt --check` 无输出。

## 结论（第十五次验收）

第 3、4、5 轮连续零发现，验收通过。测试 131 → 136（lib 93 / console 4 / e2e 39）。

# 第十六次验收（2026-09-22）— 会话录制、pane 边框标题、format 变量

本次新增：
- `record [-t 目标] [文件]`：把 pane 输出写成 asciinema v2（`o` 事件 + 尺寸变化的 `r` 事件），
  写文件走独立线程（有界通道 1024 条，跟不上丢事件不丢输出）；不带路径就停，
  pane 被杀时录制器随之关闭，文件完整。
- `pane-border-status top|bottom` + `pane-border-format`：布局阶段每个 pane 让出一行给边框文字，
  `compose` 在那一行画 `pane-border-format` 展开的内容（当前 pane 用活动边框色），其余格子仍是边框线。
- format 变量从 9 个扩到 39 个（session/window/pane/client/server 五组），修饰符
  `=N:` `=-N:` `b:` `d:` `t:` `s/a/b/:` 可嵌套；两处各自手写的 `Context` 字面量合并成一个
  `Server::context()`，状态栏、`display-message`、边框文字、菜单标题都从这一处取值。

## 第 1 轮（不计数）— 视角：机制通路（e2e）

| # | 问题 | 修复 |
|---|------|------|
| 1 | `#{b:pane_current_path}` 对 `C:\…\Temp\`（`temp_dir()` 带尾部反斜杠）取出空串 | `b:` / `d:` 先剥掉尾部分隔符；单元测试覆盖 |
| 2 | （测试自身）以为 cmd.exe 的提示符在第 0 行、pane 高度算成 21 | 先探出提示符所在行再断言；高度 = 24 − 状态栏 − 边框行 = 22 |
| 3 | （测试自身）以为新 pane 天生有 `pane_current_path` | 先 `set-cwd` 再查 |

## 第 2 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制）

数据：录制文件头 `version=2 size=80x23`，事件序列 `ooro`（回显、输出、分屏引起的 resize、新提示符），
含 `in-the-cast` 的 `o` 事件存在，时间单调；`pane-border-status top` 使两个 pane 高度 11→10，`off` 回到 11；
`display-message -p` 一次展开 11 个变量/修饰符：`1|2|1|System32|C:\Windows|a|NE|C:\WINdows\System32\|Tue Sep 22 …|alive|Syst`，
逐项对上（含带尾部反斜杠的 `b:`、`=-2:host`、嵌套 `=4:b:`、`t:` 时间、条件）。

## 第 3 轮（计数 2/3，无发现）— 视角：边界与退化输入

数据：`record` 到不存在的盘报 `cannot create … (os error 3)`；未录制时停报 `not recording`；
两次 `record` 第二个文件替换第一个（旧文件只剩头）；多余参数报 `unexpected argument`；
2000 行输出爆发后 1992 个事件、末行 JSON 完整；录制中的 pane 被 `kill-window`，文件末行完整；
`pane-border-status sideways` 拒绝并列出三个合法值；`-x 20 -y 4` 的小 session 下 top/bottom/status off/on
来回切、空 format、`#{nosuch}#{b:}#[bold` 残缺 format，server 全部存活；
7 个畸形修饰符（`=abc:`、`=-0:`、`s//x/:`、`b:` 空值、`t:` 非数字、`s/a:`、`:`）全部得到空串或原值，
20 层嵌套 `s/a/b/:` 正常，`==1` / `==` 空比较正常。

## 第 4 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，141 项指纹均为 `8C92D0A2B4D2D05F`；README 列出的 39 个变量
在 `Context::var` 里逐个存在，对照表同样齐全；`--help` 含 `record`；`show-options` 含两个边框选项；
clippy 与 `cargo fmt --check` 无输出。

## 结论（第十六次验收）

第 2、3、4 轮连续零发现，验收通过。测试 136 → 141（lib 95 / console 4 / e2e 42）。

# 第十七次验收（2026-09-22）— save-history all、new -x/-y、display-message -t、边框态分屏修正

本次新增 / 修正：
- `set -g save-history all`：整段 scrollback（受 `history-limit` 约束）随 session 存盘，
  行里保留颜色与属性（`capture-pane -e` 的形式），`resume` 时逐字节贴回；`show-options` 显示 `all`。
- `Pane::lines_from(start, escapes)`：一趟走完 scrollback 取行；`capture-pane -S -` 也改用它
  （原来逐行调 `line_text`/`line_escapes`，每行重算一次 scrollback 长度，5000 行就是 5000 次全扫）。
- `new-session -x cols -y rows`：给 detached / 脚本建的 session 定尺寸（tmux 同款；最小 10×3）。
- `display-message -t 目标`：format 按指定 pane 展开（原来所有 flag 连值一起被吞掉，`-t x` 的 x 会混进消息）。
- **修 bug**：`pane-border-status` 开着时再 `split-window`，两半不均（23 行的格子切成 11/9 而不是 10/10；
  7 行切成 1/2）。原因是分屏把"已扣掉边框行的绘制 rect"当布局输入，树里少了一行，布局把它补给第一个 pane。
  现在 `Window` 另存一份布局格子 `layout_rects`，分屏与 join-pane 按格子切，最小高度检查也算上边框行。

**更正第十六次记录**：第 3 轮里"`-x 20 -y 4` 的小 session 下 top/bottom … server 全部存活"这句不成立——
当时 `new` 根本不认 `-x`（报 `unknown flag '-x'`，探针把输出吞了），session 没建出来，
后面只验证了 server 没死。本次补上参数后小 session 的检查才真正跑到（见第 3 轮）。

## 第 1 轮（不计数）— 视角：静态一致性 + diff 自审

| # | 问题 | 处理 |
|---|------|------|
| 1 | `cargo fmt --check` 一处差异 | fmt |
| 2 | `capture-pane -S -` 仍是逐行 O(n²) 取法 | 改用 `lines_from` |
| 3 | `line_escapes` 无人再用 | 删除 |
| 4 | 网站统计仍写 126 tests（实际 143） | 改 143 |

## 第 2 轮（不计数）— 视角：机制通路（release 二进制）

wmux 数据无问题；发现的是探针脚本的问题（`$t[0]` 取到字符串首字符、双引号里 `$g` 被 PowerShell 展开成空）。

## 第 3 轮（不计数）— 视角：边界

发现 `new -x/-y` 不存在（上一批的小 session 检查因此从未跑过）→ 补上；`all `/`ALL` 被拒 → 与 `parse_bool` 一致，
去空白、忽略大小写。补 e2e 时又发现 `display-message -t` 不生效（输出 `small:0 30x4`）→ 实现。
用 `-x/-y` 建小 session 后再查边框态分屏：24 行 11/9、7 行 1/2、9 行 2/3，不开边框全部正常 → 定位并修复（见上）。

## 第 4 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制）

数据：`save-history 500` 存 498 行 14059 B；`all` 存 5021 行 136160 B，`save-session` 20 ms；
`capture-pane -S -` 18 ms 取 5023 行（line-980 … line-6000）；`resume` 33 ms，回来 5019 行 line-* 加提示符
（少 2 行是新 shell 自己的空行+提示符把最早两行挤出了 `history-limit`）；`save-history 0` 存 0 行。

## 第 5 轮（计数 2/3，无发现）— 视角：边界

数据：4 行"绿色中文提示符 + 蓝底 + 汉字"resume 后 4/4 逐字节相同；120 列存、80 列恢复，100 个 x 换行成
30+80+20=130 个字符不丢；空 pane 存 2 行；只打印转义序列的提示符行被丢掉不存；`history-limit 50` 下存 73 行
（≤ 50+23）；20×4 小 session：建成 20×3，开边框后 20×2，此时分屏被明确拒绝 `pane too small to split`，
resume 回来 80×23（存档不含尺寸，与 tmux-resurrect 一致），server 存活；`-1`、`1e3`、空值被拒，`all `、`ALL` 接受。
resume 后底部多一组"空行+提示符"（旧提示符行在存档里，新 shell 又打一个）：与 tmux-resurrect 行为相同，不算问题。

## 第 6 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，143 项指纹均为 `4592FD1A9969C1C4`；边框态分屏 6/7/8/9/10/24 行 →
1/1、2/1、2/2、3/2、3/3、10/10（第一个多一行，与不开边框时的规则一致）；`display-message -p -t s24:0.1` 答 `1 10`，
坏目标报 `can't find session: nosuch` 退出 1；`--help` 列出 `-x cols -y rows`；README（中英）、conf 示例、
对照表、网站六处都写了 `all`；对照表 new-session 行有 `-x`、`-y`，display-message 行有 `-t`；
网站 143 tests；`line_escapes` 无残留；clippy 与 fmt 无输出。

## 结论（第十七次验收）

第 4、5、6 轮连续零发现，验收通过。测试 141 → 143（lib 96 / console 4 / e2e 43）。

# 第十八次验收（2026-09-22）— `jobs` 任务板、respawn 环境修正

本次新增 / 修正：
- `jobs [-t 目标] [-F 格式]`：整个 server 每个 pane 一行——PANE、STATE（`running` / `exit N`）、UP（启动至今）、
  IDLE（上次输出至今）、PID、COMMAND、DIR，列按显示宽度对齐（中文占两格）。`-t` 到 session / 窗口 / 单个 pane；
  `-F` 走 format 引擎。新增变量 `pane_start_time`、`pane_activity`（unix 秒，可配 `t:`），`Pane.last_output`
  在 `note_output` 打点。`format::human_duration`：`42s` / `5m` / `2h13m` / `3d2h`。
- **修 bug（既有）**：`respawn-pane` 只传了 `set-environment` 的条目，没传 `pane_env()`（WMUX、WMUX_PANE、
  把本 exe 目录放最前的 PATH）。后果：respawn 过的 pane 里 `WMUX=default`，`wmux` 解析到 `C:\Program Files\wmux\wmux.exe`
  （0.5.0）——探针里 `jobs` 报 `unknown command: jobs`、空文件。现在 respawn 与新建 pane 用同一套环境。

## 第 1 轮（不计数）— 视角：机制通路（release 二进制）

`jobs` 输出形状正确；发现 respawn 后 pane 内 `wmux` 指向别的 exe（见上）→ 修；探针自身两处失误
（把 `cmd.exe /c exit 3` 当一个参数传、竖切第 4 次被拒后误以为有 40 个 pane）。

## 第 2 轮（不计数）— 视角：边界

发现：(1) 列按字符数补齐，中文 session 名在终端里错位 → 改按 unicode 显示宽度；(2) `-t s:w.p` 带 pane 时列的是整个窗口，
脚本问单个 pane 不方便，且 e2e 里那条 `-F` 时间检查其实拿到两行、凑巧通过 → pane 目标只列该 pane，e2e 改成明确断言一行。
另：`#(cmd)` 在一次性命令里展开为空（缓存按 `status-interval` 刷新，`display-message` 同样）——记录，不改。

## 第 3 轮（不计数）— 视角：可复现性 + 文档 claim

`list-commands` 检查 False 是脚本没先起 server（该命令不自动起 server）；改脚本后 True。

## 第 4 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制）

数据：4 个 200×50 session 各 10 个 pane，`jobs` 40 行 21 ms，全部 `running`，40 个 pid 互不相同；
`pane_activity` 在一次 `echo` 后前移（1790070884 → 1790070888），IDLE 显示 `0s`；`respawn-pane -k` 后
`pane_start_time` 前移、pid 改变（34088 → 6044）；`cmd.exe /c exit 7` 的窗口在 `remain-on-exit on` 下留一行 `exit 7`；
respawn 过的 pane 里跑 `wmux jobs -F …` 得到 42 行（本 server 全部 pane）。

## 第 5 轮（计数 2/3，无发现）— 视角：边界

数据：无 session 时退出 0、空输出；`中文会话` + 12 层深目录，STATE 列三行都从第 14 格开始；
`-t b:0` / `-t b:0.0` / `-t b:9`（`no window 9`，退出 1）；zoom 的窗口仍列出被藏的 2 个 pane；
`-F ""` 输出空行、`#{nosuch}|#{b:}|#[bold|…` 残缺格式不崩、`-F` 无值报 `-F: missing value`、`-x` 报 unknown flag、
`job` 缩写可用；带空格的标题一行内列位置不变。

## 第 6 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，145 项指纹均为 `96D4C038C0B1567D`；`--help` 与 `list-commands` 都有 `jobs`；
README（中英）与对照表都写了 `jobs` 及两个新变量；`Context::var` 应答两者；网站 145 tests；COMMANDS 含 `jobs`
且解析测试覆盖每一项；clippy 与 fmt 无输出。

## 结论（第十八次验收）

第 4、5、6 轮连续零发现，验收通过。测试 143 → 145（lib 97 / console 4 / e2e 44）。

# 第十九次验收（2026-09-22）— Windows Terminal 集成

本次新增：
- `wmux windows-terminal install|remove|status`（别名 `wt`）：往 `%LOCALAPPDATA%\Microsoft\Windows Terminal\Fragments\wmux\`
  写一个 profile 片段（`wmux.json`，非默认 socket 是 `wmux-<socket>.json`），Windows Terminal 自动合并，不碰 `settings.json`。
  profile 名 `wmux` / `wmux (<socket>)`，命令 `"<exe>" [-L socket] new-session -A -s main`，`startingDirectory` `%USERPROFILE%`。
  GUID 由 socket 名经两趟 FNV-1a 派生并整成 RFC 4122 v4 形状——重装不换身份，用户改的字体/配色保留。
  本地执行，不起 server（与 `startup` 同一分支）。
- 顺手：`startup` 与 `windows-terminal` 都拒绝动词之后的多余参数（原来 `startup on extra` 会照做）。

## 第 1 轮（不计数）— 视角：机制通路（真实目录）

在本机真实 Fragments 目录装/查/卸一遍全部正确；发现 GUID 版本位随机（`c5fe`），改成 v4 + RFC 变体位。

## 第 2 轮（不计数）— 视角：边界

发现 `windows-terminal install extra` 多余参数被无视地执行（探针那一段还把 LOCALAPPDATA 切回了真实目录，
真装了一次，立即卸掉；脚本改成最后才切回）→ 两个本地命令都拒绝多余参数，单元测试覆盖。

## 第 3 轮（计数 1/3，无发现）— 视角：机制通路（真实目录）

数据：装前 0 个文件、status 退出 1；install 退出 0，文件是合法 JSON，`commandline` 等于 `"<本 exe>" new-session -A -s main`，
`startingDirectory` `%USERPROFILE%`；重装 GUID 不变 `{ebada516-8620-45fe-90b0-7b3d893a0b99}`，符合 v4/RFC 正则；
`-L work wt install` 生成 `wmux-work.json`，名字 `wmux (work)`，GUID 不同，命令含 `-L work`；从别的目录执行 status 仍指向本 exe；
两个都 remove 后目录 0 个文件，status 退出 1，再 remove 报 `not installed`。

## 第 4 轮（计数 2/3，无发现）— 视角：边界（临时 LOCALAPPDATA）

数据：无目录时 status/remove/无动词各自正确；install 自建整条路径；文件损坏时 status 报 `… wmux.json is not JSON: key must be a string …`
退出 1，reinstall 覆盖成合法；是 JSON 但没有 profile → 视为未安装；socket `a b`、`x.y`、`中文` 生成对应文件名；
上级路径是文件时 install 报 `cannot create … (os error 183)` 退出 1 不 panic；LOCALAPPDATA 未设置报 `LOCALAPPDATA is not set`；
`install extra` / `instal` / `startup on extra` 全部拒绝且未写文件；真实目录未被触碰。

## 第 5 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，149 项指纹均为 `CFC906FA4D1E3F08`；`--help` 有 `windows-terminal install|remove|status`；
README（中英）写了命令与 Fragments 路径，对照表 wmux-only 行有条目；代码只写 `Fragments\wmux` 下、不含 `settings.json`；
网站 149 tests；main.rs 两个本地命令走同一分支；真实目录无残留；clippy 与 fmt 无输出。

## 结论（第十九次验收）

第 3、4、5 轮连续零发现，验收通过。测试 145 → 149（lib 101 / console 4 / e2e 44）。

# 第二十次验收（2026-09-22）— status-justify、window-status-separator

本次新增：
- `status-justify left|centre|right|absolute-centre`（也接受 `center` / `absolute-center`，存成 tmux 拼法）：
  窗口列表整体放得下时按此定位；放不下退回左对齐（保持"当前窗口一定可见"的既有预留逻辑）。
- `window-status-separator`（默认一个空格）：替换原来硬编码在标签之间的一格；按显示宽度计算，
  分隔符与其后的标签一起判断放不放得下——放不下就一起省略，不留悬空分隔符。
- render 层新增 `Justify` 枚举，`StatusLine` 带 `separator`/`justify`；两项进 SHOWABLE/KNOWN，可缩写（`s-j`、`w-s-s`）。

## 第 1 轮（不计数）— 视角：单元测试

新写的 render 测试抓到：12 列放不下第二个标签时分隔符先画出来了（`[s] 0:a|   R`）→ 改成分隔符+标签一起判断。
`set_options` 里歧义计数 "and 9 more" 因新选项变 10 → 更新。

## 第 2 轮（不计数）— 视角：边界（release 二进制）

`center` 接受但 `absolute-center` 拒绝，不一致 → 一并接受；单元测试覆盖两种拼法与 4 种坏值。
（`set w-s x` 报 unknown：缩写规则要求词数一致，`w-s-s` 可用，不算问题。）

## 第 3 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制 + e2e 客户端）

数据：`.tmux.conf` 写 `status-justify centre` / `window-status-separator ' | '` 启动后 show 为 `centre` / ` | `，
`show-messages` 无 skip 记录；set right/center/absolute-centre/s-j left 往返正确，`w-s-s ::` 与空分隔符往返正确；
`show-options -g` 列出两项。e2e（attach 的客户端）：`" | "` 出现在 `0:aa` 与 `1:bb` 之间且列表从第 4 格起；
`right` 时 `1:bb` 紧接一格再接右侧引号；`center` 时起点 > 6 且两侧都有空隙；`sideways` 被拒。

## 第 4 轮（计数 2/3，无发现）— 视角：边界

数据：`''`、`middle`、`CENTRE`、`left right`、`0` 拒绝且不改值；` centre ` 去空白接受；`absolute-center` 接受存为 `absolute-centre`；
200 字符、全角 `｜`、制表符、`#[bold]|#[default]`（按字面）、emoji 分隔符全部往返一致；
200 字符分隔符 + 3 个窗口 + absolute-centre 下 server 存活。渲染层边界由单元测试覆盖：
30 列四种对齐的逐格结果、` · ` 与 `│` 分隔符、12 列退回左对齐并丢掉放不下的窗口。

## 第 5 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，152 项指纹均为 `B99056DA91B556E2`；README（中英）、conf 示例、对照表都写了两项；
SHOWABLE 与 KNOWN 各列两项（脚本计数 3 是把单元测试里的 `o.set("status-justify", …)` 也数了，grep 行号核实）；
默认分隔符代码与文档一致为空格；render 从 frame 取 justify/separator；网站 152 tests；clippy 与 fmt 无输出。

## 结论（第二十次验收）

第 3、4、5 轮连续零发现，验收通过。测试 149 → 152（lib 103 / console 4 / e2e 45）。

# 第二十一次验收（2026-09-22）— WinGet / Scoop 清单

本次新增：
- `tools/package-manifests.ps1 -Version X.Y.Z`：从 GitHub 发布页取两个 `.sha256` 与 MSI（读其 ProductCode，
  因为 wxs 里 `Product Id="*"` 每次构建都变），生成 WinGet 三个清单（`packaging/winget/manifests/n/newdee/wmux/X.Y.Z/`，
  目录布局与 winget-pkgs 一致）和 Scoop 清单（`packaging/scoop/wmux.json`，带 `checkver`/`autoupdate`）。
  LF、无 BOM。0.5.0 的输出已提交。
- `packaging/README.md`：提交到 winget-pkgs / ScoopInstaller/Extras 的步骤；README（中英）安装段与网站安装卡片加了 Scoop 一行
  （仓库里的清单可直接 `scoop install <raw url>`），WinGet 写明"合并后才有 `winget install newdee.wmux`"，不做尚未成立的 claim。
- 提交 winget-pkgs / Extras 的 PR 需要用户自己的 GitHub 账号，这一步留给用户。

## 第 1 轮（不计数）— 视角：机制通路

生成器两处 PowerShell 插值错误（`"$msiName: …"` 被当作用域、`"$asset.sha256"` 被当属性）→ `${…}`；
不存在的版本只吐 GitHub 原始 404 JSON → 加 "no release vX.Y.Z on github.com/<repo>" 的明确报错。
探针脚本自身三处误判（`packaging/` 未跟踪时用 git status 判"是否改动"、Scoop 的 Write-Host 输出用 `2>&1` 抓不到、
"Checking hash of … ok." 被 Scoop 分段输出）→ 改成内容哈希比较、`*>&1`、以"installed successfully"为准。

## 第 2 轮（计数 1/3，无发现）— 视角：边界

数据：`0.5`、`v0.5.0`、`0.5.0.1`、`latest` 四种坏版本号在联网前被 ValidatePattern 拒绝（退出 1、报错含模式、未写文件）；
`9.9.9` 与不存在的仓库都报 `no release …` 退出 1、未写文件；4 个输出文件无 BOM、0 个 CRLF、以 LF 结尾；
installer 的 sha 为 64 位大写十六进制、ProductCode 带引号；Scoop 的 hash 为 64 位小写、`extract_dir` 等于 zip 内目录名、
autoupdate 的 hash 取 `$url.sha256`；三个 winget 文件名符合 winget-pkgs 要求；152 项测试通过。

## 第 3 轮（计数 2/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，152 项指纹均为 `B99056DA91B556E2`；README（中英）与网站含同一条 Scoop raw URL，
其路径在仓库中存在；README 指向 `packaging/README.md` 且写了 `winget install --manifest` 路径；packaging/README 写了生成器与两个输出；
网站有 Scoop 卡片且中文两个键齐全（52 个 i18n 键全部有中文）；三个 winget 文件的 PackageIdentifier 一致；
release.yml 的资产命名与生成器期望一致；clippy 与 fmt 无输出。

## 第 4 轮（计数 3/3，无发现）— 视角：机制通路（真实安装）

数据：重新生成 4 个文件与磁盘上的逐字节相同；`winget validate --manifest` 报"清单验证成功"（winget v1.29.20-preview）；
用仓库清单 `scoop install`：过 hash 闸门、解压、建 shim，`wmux --version` 经 shim 得 `wmux 0.5.0`，`--help` 含 attach-session；
把 hash 改成全 0 的副本被拒（`ERROR Hash check failed!`，未装、无 shim）；`scoop uninstall` 后 shim 与 app 目录都不在；
i18n 52/52；152 项测试通过。（`winget install --manifest` 要管理员并装到全机，未执行；validate 为准。）

## 结论（第二十一次验收）

第 2、3、4 轮连续零发现，验收通过。测试数不变（152）。
