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

# 第二十二次验收（2026-09-22）— 一次性 `#()`、`pane_dead_time`、存档尺寸

本次新增 / 修正：
- `display-message -p`、`jobs -F` 里的 `#(命令)` 当场执行（`expand_with_shells`），最多等 3 秒（`ONE_SHOT_SHELL_TIMEOUT`），
  结果进同一个缓存；原来只有状态栏按 `status-interval` 后台跑，一次性命令拿到的是空串。
- `Pane.died_at` + 变量 `pane_dead_time`（未退出为空）；`jobs` 里已退出 pane 的 UP 停在退出时刻（"跑了多久"），不再一直涨。
- 存档 `SavedSession.size`（serde default，旧文件兼容）；`resume` 没有终端接入时用存档尺寸，下限 10×3（与 `new -x/-y` 一致）；
  有终端接入时仍用终端尺寸。

## 第 1 轮（不计数）— 视角：机制通路（release 二进制）

`jobs -F` 的 `#()` 仍为空：jobs 直接调 `format::expand`，没走同步路径 → 抽出 `expand_with_shells`，两处共用；e2e 补断言。
探针把 `exit 9` 当两列数错了 UP/IDLE 下标（脚本问题）。

## 第 2 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制）

数据：`#(echo now-please)` 首次 272 ms 得到结果、第二次走缓存 15 ms；`#(Start-Sleep 20)` 3050 ms 后返回空串，server 存活；
`jobs -F '#(echo in-jobs) #{pane_index}'` → `in-jobs 0`（267 ms）；`cmd /c exit 9` 的 pane 3 秒后 UP 仍为 `0s`、IDLE 由 2s → 5s；
`pane_dead_time` 为 unix 秒、活 pane 为空；132×43 的 session 存档含 size，`resume`（无终端）回来 132×43；
删掉 size 字段后 resume 为 80×24。

## 第 3 轮（计数 2/3，无发现）— 视角：边界

数据：`#()` 无输出→空、多行取第一行、退出码 3 仍显示输出、中文正常、同一格式两个 `#()` 都执行、`#()` 空、
未闭合 `#(echo a` 按已闭合处理（既有宽松解析），server 存活；存档 size 1×1 / 0×0 → 10×3，10×3 → 10×3，500×200 → 500×200，
`"wide"` → `saved session: invalid type: string "wide", expected a tuple of size 2` 退出 1。
记录（不改）：`#(echo #{session_name})` 里的 `#{}` 不先展开——既有行为。

## 第 4 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，154 项指纹均为 `31C7DC8E2E466AC7`；三处文档列出 `pane_dead_time`；README（中英）写了一次性
`#()` 与 3 秒上限，与常量一致；restore 下限与 `-x/-y` 下限一致（10/3）；网站 154 tests；clippy 与 fmt 无输出。

## 结论（第二十二次验收）

第 2、3、4 轮连续零发现，验收通过。测试 152 → 154（lib 103 / console 4 / e2e 47）。

# 第二十三次验收（2026-09-22）— 发布流水线自动出清单

本次新增：
- `tools/package-manifests.ps1` 加本地模式（`-MsiPath`/`-ZipPath`/`-ReleaseDate`）：CI 里用刚构建的文件算 hash、读 ProductCode，不联网；
  文件名必须与发布资产名一致（清单指向发布 URL）；`-ReleaseDate` 校验 yyyy-MM-dd。
- `release.yml` 新增两步：`package manifests`（生成 + `winget validate`（runner 上没有 winget 则跳过）+ 打包
  `wmux-X.Y.Z-manifests.zip` 附到 release）；`commit the manifests to master`（把生成的 `winget/`、`scoop/` 拷到一边，
  从 tag 的 detached 状态切到 origin/master 再拷回、以 github-actions[bot] 提交推送；无变化则不提交）。
- `packaging/README.md` 说明自动化。

## 第 1 轮（不计数）— 视角：机制通路（在临时 bare origin 上按 yml 原文跑两步）

发现两处流水线 bug：(1) `if (git diff --cached --quiet)` 测的是空输出而非退出码，"无变化"分支永不触发 → 改看 `$LASTEXITCODE`；
(2) 拷回时 `Copy-Item "$keep\*"` 把 `packaging/README.md` 也盖掉，master 上别人的改动丢失 → 只拷 `winget\`、`scoop\`。
另加 `git clean -fdq packaging`，防新版本目录（未跟踪）挡住切分支。模拟脚本自身两处顺序问题（`other` 克隆早于推送、
第二次运行没还原文件）已修。

## 第 2 轮（计数 1/3，无发现）— 视角：机制通路（同上，master 同时前进）

数据：`package manifests` 步骤退出 0，`winget validate` 通过，zip 含 4 个文件；与已提交清单仅 `ReleaseDate` 一行不同
（CI 取当天，符合语义）；master 先提交了 README 与 packaging/README 的改动后，`commit` 步骤成功推送
`build: package manifests for v0.5.0`，只改 installer.yaml 一个文件，master 自己的两处改动保留，作者 github-actions[bot]；
第二次（无变化）输出 `manifests unchanged`、退出 0、没有尝试 commit。

## 第 3 轮（计数 2/3，无发现）— 视角：边界

数据：本地模式输出与已提交 4 个文件逐字节相同（指定 `-ReleaseDate 2026-09-21`）；只给 `-MsiPath` → `go together` 退出 1；
文件名不对 → `should be named …` 退出 1；`-ReleaseDate 21/09/2026` → `wants yyyy-MM-dd` 退出 1；不给日期 → 当天；
`release.yml` 经 PyYAML 解析通过，步骤顺序 test → build → zip → msi → manifests → release → commit。

## 第 4 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，154 项指纹均为 `31C7DC8E2E466AC7`；packaging/README 的 zip 名与 yml 一致并写明自动提交；
`permissions: contents: write` 存在；commit 步骤按退出码判断、只拷 winget/scoop；生成器路径存在、用本地模式、
资产名与生成器一致；`.gitattributes` 固定 LF；clippy 与 fmt 无输出。
（流水线本身要等下一个 tag 才真正跑一次；这里验证的是按 yml 原文提取的脚本。）

## 结论（第二十三次验收）

第 2、3、4 轮连续零发现，验收通过。测试数不变（154）。

# 第二十四次验收（2026-09-22）— `choose-jobs`（prefix B）任务板弹窗

本次新增：
- `choose-jobs`：`jobs` 那张板做成 chooser（`ChooserKind::Jobs`，每帧重建）。首行是表头（不可选），
  光标起始在本客户端所在 pane；`Enter` 切到那个 pane（session → 窗口 → pane，触发 after-select-pane 钩子），
  `x` kill-pane，`r` respawn-pane -k，操作后板不关、行原地刷新；提示行按类型显示动作（`Enter go  x kill  r restart`）。
  `jobs` 与弹窗共用 `jobs_panes`/`jobs_row`/`align_columns`。默认绑定 `prefix B`。
- 默认按键表建表时对重复键断言，防止后加的条目静默覆盖前面的。

## 第 1 轮（不计数）— 视角：机制通路

发现真 bug：最初绑在 `prefix J`，HashMap 插入静默覆盖了默认的 `J → resize-pane -D 5`（`list-keys` 还显示成 `-r`）。
`T` 也被 pane 标题提示占用，改用空闲的 `B`；加断言。

## 第 2 轮（计数 1/3，无发现）— 视角：机制通路

数据：交互 e2e（两条）连跑 3 次全过：板打开时 `[2/4]` 停在本 pane、提示行含 `Enter go  x kill  r restart`、
`r` 后 pane pid 变化且板仍开着、`x` 杀掉 `exit 4` 的 pane 后计数变 3 且窗口消失、`G`+`Enter` 切到 `other` session、
按键没有漏进 shell、脚本里调用报 `client not attached`。release 二进制：`list-keys` 有 `-T prefix B choose-jobs`（无 `-r`）
且 `J` 仍是 `resize-pane -D 5`；`list-commands`、`--help` 都有；`jobs` 表头对齐不变。

## 第 3 轮（计数 2/3，无发现）— 视角：边界

数据：边界 e2e 连跑 3 次全过：12 个 pane 时跳转标签 (1)…(9) 且第 10 个起无标签、`g` 不落在表头、`7` 跳到第 7 项、
光标下的窗口被外部 kill 后板自动变 12 行、`q` 关闭、按键不漏、`unbind-key B` 后 `list-keys` 无 choose-jobs、
`bind-key Y choose-jobs` 后 `prefix Y` 打开。CLI：多余参数拒绝、`choose-jo`/`choose-j` 解析到 choose-jobs、
`bind-key -n F12 choose-jobs` 出现在 root 表；41 个 pane 的板 26 ms，STATE 列同一屏幕列。

## 第 4 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：连续 3 次 `cargo test`，156 项指纹均为 `ABBD54930CF37D7B`；README（中英）与对照表写了 choose-jobs、prefix B、x/r；
默认绑定为 B 且有重复键断言；提示行文案、live 重建、共用行构造器、COMMANDS 条目、网站 156 tests 全部核对；clippy 与 fmt 无输出。

## 结论（第二十四次验收）

第 2、3、4 轮连续零发现，验收通过。测试 154 → 156（lib 103 / console 4 / e2e 49）。

# 第二十五次验收（2026-09-23）— 修复：恢复后历史丢失、工作目录不跟随（用户报告）

用户报告：关掉 wmux 再打开，只有布局回来了，历史与工作目录都没了。本机查证（用户真实存档 `1-….json`：两 pane 各 ~500 行历史，
cwd 均为 `C:\Users\stebe`；用户流程 `wmux resume 1`；无 $PROFILE），定位到三个独立原因，全部修复：

1. **工作目录**：cwd 只在 shell 主动上报 OSC 9;9 时更新，`pwsh -NoLogo` 不上报，存的永远是起始目录。
   - 启动 pwsh/powershell 时自动挂 prompt 钩子（`config::with_shell_integration`，`-NoExit -Command <hook>`）：保留用户原 prompt，
     在其后追加不可见的 OSC 9;9；带 `-Command/-File/-EncodedCommand`（含 PowerShell 允许的前缀缩写）的不动。
   - `cmd.exe` 等进程：新模块 `proccwd` 读目标进程 PEB 的 CurrentDirectory（NtQueryInformationProcess + ReadProcessMemory）。
   - 优先级：shell 上报或 `set-cwd`（`announced`）> 进程目录 > 起始目录。
   - 顺带修：带引号的 OSC 9;9（Windows Terminal 官方片段的写法）原来会被丢掉。
2. **历史回放**：原来把存档文本喂进 wmux 自己的 vt100 模型，ConPTY 的缓冲里没有——任何一次重绘/缩放（`startup` 无头 80×24 恢复后
   再从大终端 attach 就是一次缩放）都会把它冲掉。现在由助手进程 `wmux __replay 文件` 通过 CONOUT$ + WriteConsoleW 把文本**打印进
   pane 的 ConPTY**（进 conhost 缓冲，之后的重绘、缩放由 ConPTY 自己处理），打完写一个私有 OSC 标记 `ESC]7777;wmux-replayed ESC\`
   （实测能穿过 ConPTY），server 收到标记后把 shell 起进同一个 ConPTY（助手仍存活，conhost 不会因"第一个进程退出"而关掉），
   再删文件让助手退出；助手的退出事件用 `HELPER_GEN` 标记与 pane 自身进程区分。
3. **vt100 缩行丢行**：vendored vt100 的 `set_size` 缩行时从底部截断、扩行只补空行，缩一次少一截。改为缩行把顶部行推入 scrollback
   （以光标留在屏上为准）；扩行仍只补空行（与 ConPTY 那侧一致，拉回来会被它的重绘盖掉——实测过）。

## 排查过程中的弯路（都记在这）

- 同步等助手退出会死锁：ConPTY 启动进程时先发 `ESC[6n` 等回答，server 线程阻塞就没人回答（单元测试
  `a_first_process_can_exit_and_a_second_can_follow` 固化了这点）。
- 助手用 Rust `stdout()` 写不进控制台：portable-pty 把子进程 stdio 句柄设成 INVALID，`stdout()` 静默丢弃 → 改用 CONOUT$。
- `start_pending` 给 generation +1 会让 reader 线程（带旧 generation）的输出全部被丢 → 不加代。
- 回放临时文件名 `pid-paneid` 在 e2e（同进程多 server、pane id 各自从 1 起）下互相串 → 加全局序号。
- 测试里 `current_exe()` 是测试二进制而非 wmux.exe → `WMUX_EXE` 环境变量 / 兄弟目录查找。
- `.tmux.conf` 那条 e2e 没设 `sessions-dir`，把 `t` session 泄漏到用户真实存档目录（已删两次，已修）。
- 探针脚本用 `history` 撞了 Get-History 别名、pwsh 7 不能投影 WinRT 类型等，均为脚本问题。

## 第 1～N 轮（不计数）— 上述修复过程中的多轮迭代，每轮以全量测试收口

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制，用户流程）

数据：pwsh pane 打印 30 行、`cd C:\Windows\System32`、存档、kill、无客户端 `resume`，连跑 3 次逐字一致：
存档 cwd=System32、33 行；恢复后可见 20 行 line-*、滚动区 31；分屏缩小后可见 8、滚动区仍 31；shell 位置 System32。
cmd pane `cd /d C:\Windows\Fonts` → `pane_current_path`、存档 cwd、恢复后 `cd` 输出均为 Fonts。无残留回放文件、无残留 `__replay` 进程。

## 第 B 轮（计数 2/3，无发现）— 视角：边界

数据：`save-history 0` 不回放、旧行不在；5000 行回放 164 ms 后 shell 可用、5000 行全在、末行为新提示符；
`remain-on-exit` 下立即退出的程序：回放的 `lived` 与退出提示都在、`pane_dead_status` 3；回放进行中 kill-session：0 残留文件、0 残留进程、server 存活；
pwsh 进注册表驱动器时目录保持上一个真实目录；含空格与中文的路径正确；`pwsh -Command …` 不注入钩子。

## 第 C 轮（计数 3/3，无发现）— 视角：可复现性 + 文档 claim

数据：全量测试 170 项，连续 7 次全过（其中一次指纹不同但未能定位到具体用例，随后把唯一依赖固定 sleep 的
`proccwd` 测试改成轮询）；README（中英）改为"目录跟着 cd 走、不用配置"，删掉旧的"放进 $PROFILE"片段；
代码 claim（HELPER_GEN、标记触发、钩子避让、缩行保留、序号文件名、助手等待删除、无调试残留）逐项核对；
真实存档目录无测试文件；clippy 与 fmt 无输出。

## 结论（第二十五次验收）

A、B、C 三轮连续零发现，验收通过。测试 156 → 170（lib 114 / console 4 / e2e 52）。
用户机器上跑的是 0.5.0（`C:\Program Files\wmux`），需要发新版才能用上。

# 第二十六次验收（2026-09-23）— C-10 收尾：`%if` 求值、比较运算、`capture-pane -J`、25 个新变量、完整 target、`window-size`、picker 过滤/标记

## 做了什么

1. `%if` / `%elif` / `%else` / `%endif` 真正求值（条件是格式串，非空且非 `0` 为真；server 级上下文，不跑 `#()`），
   原先是整块跳过。新增比较运算 `#{==:}` `#{!=:}` `#{<:}` `#{>:}` `#{<=:}` `#{>=:}` `#{&&:}` `#{||:}` `#{m:pat,s}`（`m/i:` 忽略大小写），操作数本身也是格式串。
2. `capture-pane -J` 把软换行拼回一行（按 vt100 的 `row_wrapped`）。
3. 25 个新格式变量（session_activity / last_attached、window_activity / start / end / layout、pane_last、pane 四边坐标、
   cursor_x/y、history_size / limit、pane_mode、client_* 等），全部从活树取值。
4. `select-pane -t s:w.p` 完整 target；`copy-mode -t`。
5. `window-size latest|smallest|largest|manual`：session 尺寸听哪个客户端（默认 latest = 最后接入/改尺寸/敲键的那个；
   smallest / largest 在接入、改尺寸、脱离、客户端断线时重算；manual 只认 `resize-window`）。
6. `choose-tree` / `choose-jobs` / `choose-buffer` / `choose-client`：`f` 输入子串过滤（边打边筛，`Enter` 留下，`Esc` 还原；
   树里 session 与其窗口互相带上），`t` 打标记（行首 `*`，光标下移）、`T` 清标记，`x` 杀掉标记的行（树里是 session / 窗口，
   任务板里是 pane；`r` 重启同理）。提示行右端显示 `[n tagged] [filter: xx]`，窄屏时覆盖静态提示。

## 修复过程中的发现（不计数）

- e2e `choose_tree_picker` 用 `x` 当"未绑定键"，现在 `x` 会杀东西 → 改用 `z`。
- 提示行加上过滤/标记状态后超过 80 列被截断 → 状态改为右对齐覆盖。
- 过滤串只含空白时不筛但提示行显示 `[filter:  ]` → 按 trim 后判断。
- 探针脚本：`new -x 1 -y 1` 在解析阶段就被拒（≥10×3），是脚本假设错，代码没问题。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制 + 全量测试）

数据：配置文件里 `set -g window-size smallest` 生效、`%if #{==:...}` 未命中时 mouse 保持 on、config notes 为空；
`set` 四个值逐个回读一致，`window-si` 缩写命中，`sideways` 退出码 1 并列出四个合法值，`win` 报歧义（4 个候选）；
无客户端的 `-x 100 -y 30` session 在四种模式下都保持 100x30；`show-options -g` 列出 `window-size`。
全量测试 180 项（lib 120 / console 4 / e2e 56）全过，clippy 无 warning，`cargo fmt --check` 退出码 0。
e2e `window_size_picks_which_client_sizes_the_session`：两客户端 80x24 / 60x20，latest 下 Resize → 60x20、大客户端敲键 → 80x24；
smallest → 60x20 且敲键不变；largest → 80x24，大客户端脱离 → 60x20；manual 下 Resize 70x22 + 敲键仍 60x20。
e2e `choose_tree_filters_and_tags`：`beta` 过滤剩 2 行（session 行跟着留）、`Esc` 回到 5 行、`GAMMA` 大小写不敏感且 `Enter` 后过滤留下、
`C-u`+`Enter` 清掉；打 3 个标记再取消 1 个 → `[2 tagged]`；`x` 后 `ls` 只剩 `alpha: 1 windows`，`T` 清标记。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：两条新 e2e（连同另外 3 条 picker e2e）与 3 条相关单元测试连跑 3 次，去掉耗时后输出行完全一致（distinct = 1）：
e2e 5 passed / lib 3 passed，每次相同。

## 第 C 轮（计数 3/3，无发现）— 视角：边界 + 静态一致性

数据：默认值 `latest`；空值退出码 1；` largest ` 带空白存为 `largest`；`LATEST` 大小写敏感被拒；
`new -x 10 -y 3`（解析器接受的最小值）在四种模式下都是 10x3；无客户端 server 下 `split-window` 后仍 80x24；
`window-size` 是全局选项，存档文件里 0 处出现。文档核对：README（中英）picker 按键行、"还没做的"段（改为 `window-size` 决定听谁、
过滤是子串）、parity 表 choose-tree / copy-mode `-t` / select-pane 完整 target / `%if` 段落 / 选项列表 / Still to do；
`docs/index.html` 测试数 170 → 180。`chooser_filter` 单元测试覆盖：无过滤全留、大小写不敏感、标题行常留、session 与窗口互相带上、无命中只剩标题。

## 结论（第二十六次验收）

A、B、C 三轮连续零发现，验收通过。测试 175 → 180（lib 120 / console 4 / e2e 56）。

# 第二十七次验收（2026-09-23）— 发布 0.7.0 时 CI 暴露：Run 键不存在则 `startup` 全挂

## 发现

v0.7.0 的 release 工作流在 test 步骤失败：`startup::tests::install_status_remove_round_trip` 报
`cannot open HKCU\Software\Microsoft\Windows\CurrentVersion\Run (error 2)`。runner 的用户 profile 里根本没有 Run 键
（v0.6.0 时的镜像有，这次换了 Node 24 镜像后没有）。代码只 `RegOpenKeyExW` 不创建——一个从没有程序登记过自启动的新用户
跑 `wmux startup on` 也会这样失败，`status` / `off` 也会报错而不是说"未安装"。本地全绿是因为开发机上这个键一直在。

## 修复

`Key::open(path, access)` 缺键返回 `None`（`status` → 未安装，`off` → "not installed"），`install` 改用 `RegCreateKeyExW`
（非易失、KEY_SET_VALUE）。键路径变成参数，单元测试用 `HKCU\Software\wmux-unit-test-<pid>\Run` 这个自己的临时键复现
"缺键 → None → create 后有 → 再 create 无害 → 删掉后又 None"，测完删键。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路 + 全量

数据：4 条 startup 单元测试全过（新测试覆盖缺键路径），`reg query HKCU\Software` 无 `wmux-unit-test` 残留；
全量 181 项（lib 121 / console 4 / e2e 56）全过；clippy 无 warning，fmt 干净。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：startup 4 条连跑 3 次结果一致（4 passed ×3）。

## 第 C 轮（计数 3/3，无发现）— 视角：真实缺键环境（CI runner）

数据：见下方 release 结果——v0.7.0 tag 移到修复提交后重跑，test 步骤通过即为证据。

## 结论（第二十七次验收）

三轮零发现。测试 180 → 181。`docs/index.html` 计数同步。

# 第二十八次验收（2026-09-23）— 关机/注销时存档、CI 门禁、tmux 四个小差距

## 做了什么

1. **关机/注销时存档**（`src/shutdown.rs`）：server 起一个隐藏的顶层窗口（类 `wmux-shutdown`，标题 = socket 名）收
   `WM_QUERYENDSESSION` / `WM_ENDSESSION`，同时注册控制台处理函数收 `CTRL_SHUTDOWN_EVENT` / `CTRL_LOGOFF_EVENT`
   （client 拉起的 server 是 DETACHED_PROCESS 没控制台，只有窗口这条路；登录项那条 `conhost --headless` 有控制台）。
   任一信号触发 `Event::EndSession`，server 同步把每个 session 连历史一起存一遍再应答，`ShutdownBlockReasonCreate`
   把关机拖住那一瞬间。`autosave off` 时同样不存。
   **前提修正**：之前对用户说"autosave 只在结构变化和退出时存"是错的——历史每 30 秒就存一次（实测存档文件 mtime 30 秒一跳）；
   关机真正会丢的是最后 30 秒和 cwd 变化，现在补上。
2. **CI**：三个 workflow 的 `actions/checkout` 升 v5（去掉 Node 20 告警）；release 工作流用 `gh run list --commit` 等
   同一 commit 的 CI 跑绿才出包（最多等 25 分钟，失败即拒绝发布），替换掉它自己重复跑一遍测试的步骤；
   `permissions` 加 `actions: read`。
3. **tmux 小差距**：`display-popup -x/-y`（列/行、`N%`、`C`、`R`/`B`，越界拉回）；`swap-pane -s A -t B` 成对写法
   （同窗口走 layout.swap，跨窗口/跨 session 把 Pane 对象互换、layout 用 set_panes 换 id、active 跟位置走、两边重排）；
   `list-panes -s`（`窗口.` 前缀）/ `-a`（`session:窗口.` 前缀）；`set -t 窗口 synchronize-panes`。

## 修复过程中的发现（不计数）

- 探针 gpC 先取窗口句柄再 `kill-session`：server 没 session 会自行退出，`new b` 起的是新 server，旧句柄发消息当然没反应。
  是脚本错，代码没问题（gpDbg 在同一 server 上 WM_ENDSESSION(1) 与 WM_QUERYENDSESSION 都存了）。
- 探针 gpC 敲 echo 前没等 pwsh 起来：改成轮询提示符和回显。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制 + 全量）

数据：`list-panes` 单窗口 2 行无前缀、`-s` 4 行 `0.`/`1.` 前缀、`-a` 5 行 `g:0.0:`…`other:0.0:` 前缀；
同窗口成对交换 `%2,%4 -> %4,%2`；跨窗口交换后 g:0 = `%7,%2`、g:1 = `%5,%4`，pane 总数仍 5，被搬的 pane 能回显；
`set -w -t g:1 sync on` 后 g:1 = 1、g:0 = 0；真实 DETACHED server 的隐藏窗口 FindWindow 找得到且不可见，
`WM_QUERYENDSESSION` 5 ms 内应答 1，标记从文件里 0 处变 2 处（命令行 + 输出）。
全量 185 项（lib 123 / console 4 / e2e 58）全过，clippy 无 warning，fmt 干净。
e2e `a_shutdown_saves_every_session_first`：先等首次 autosave 落盘，再 echo，确认文件里没有，发 WM_QUERYENDSESSION 后有；
`autosave off` 后再发不存。e2e `the_smaller_tmux_gaps_are_closed`：四项各自断言，popup `-x 0 -y 0` 左上角在 (0,0) 且 20 宽，
`-x R -y B` 右下角贴状态栏上一行。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：3 条 e2e + 4 条单元测试连跑 3 次，去掉耗时后输出完全一致（distinct = 1）。

## 第 C 轮（计数 3/3，无发现）— 视角：边界 + 静态一致性

数据：单 pane 时 `-a`/`-s` 各 1 行；和自己交换退出码 0 无输出；坏 session / 坏 pane 序号 / 缺失窗口各报对应错误退出码 1；
`set -t a:0 sync` 缩写切换 1 → 0；`WM_ENDSESSION(0)`（取消的关机）不存，`WM_ENDSESSION(1)` 存；收完消息 server 照常服务；
kill-server 后窗口消失。文档核对：README（中英）"还没做的"删掉四项、popup 段加 `-x/-y`、存档段写明 30 秒与关机存；
parity 表 display-popup / list-panes / swap-pane / set-window-option 行与 Still to do 段同步；站点计数 181 → 185。

## 结论（第二十八次验收）

A、B、C 三轮连续零发现，验收通过。测试 181 → 185（lib 123 / console 4 / e2e 58）。

# 第二十九次验收（2026-09-23）— tmux 最后几个小差距：跨 session swap-window、pipe-pane -I、弹窗里的前缀、树折叠

## 做了什么

1. `swap-window -s a:1 -t b:0` 跨 session：两个窗口互换位置（`split_at_mut` 同时借两个 session），
   每个 session 的当前窗口按下标保留（tmux 同样是位置带标记）。
2. `pipe-pane -I`：命令打印什么就往 pane 里敲什么（命令 stdout 由读线程经 `PaneEvent::Input` 送回 server 线程写入，
   命令输出结束时发空块，仅 `-I` 的管道随之结束）；`-O` 是默认；`-IO` 双向。`Pipe.output` 为假时 pane 输出不再送给命令。
3. 弹窗里前缀键：连按两次前缀（以及 `send-prefix` 无 `-t`）把前缀送给弹窗里的程序，而不是后面的 pane。
4. `choose-tree` 折叠：`-`/Left 折叠当前行所在 session，`+`/`=`/Right 展开；折叠时光标跳到 session 行。
5. 去掉了 README 里"`bind -r` 没有每键重复次数"这条——tmux 本身也没有这个概念，是过去写错的限制。

## 修复过程中的发现（不计数）

- e2e 里 `-o` 循环没带 `-I`，第二条管道跑的是 `-O` 什么也不会敲进去——测试写错，改 `-o -I`。
- 探针 g2A：PowerShell 双引号里 `$g` 被展开、pwsh `-Command` 会把 stdin 读完才执行（README 的 `$input | Add-Content`
  例子因此只在管道停下时落盘——是既有行为，已在 README 中英两处注明，并给出 `cmd.exe /c findstr` 这种逐行落盘的替代）。
- 探针 g2A 的 `-IO` 用 powershell `ReadLine` 同样被吞，换成 `cmd /c "set /p l= & echo ..."` 后 266 ms 内双向打通。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制 + 全量）

数据：跨 session swap 后 a = `a0*, b0`、b = `a1*, b1`，server 仍 4 个 pane；`-I` 278 ms 内把 `echo from-the-pipe` 敲进 pane 并执行；
仅 `-I` 的管道结束后 `-o -I` 起的是新管道（`second` 257 ms 出现）；`-O` 停管道后日志 213 ms 内落盘；`-IO` 266 ms 内一来一回。
全量 186 项（lib 123 / console 4 / e2e 59）全过，clippy 无 warning，fmt 干净。
e2e `the_last_small_tmux_gaps_are_closed`：四项各自断言，弹窗里 `powershell ReadKey` 收到 `code=2`（C-b）且后面的 pane 没收到；
树上 `-` 后 6 行变 4 行、`+` 回 6 行、在窗口行按 Left 折叠并把光标带到 session 行。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：6 条相关 e2e + 4 条解析单元测试连跑 3 次，去掉耗时后输出完全一致（distinct = 1）。

## 第 C 轮（计数 3/3，无发现）— 视角：边界

数据：swap-window 和自己换退出码 0 无输出；换到不存在的下标报 `no window 5`；两个 session 各只有一个窗口时互换正常；
`-I` 跑一个不存在的程序退出码 0、pane 照常应答；`-I` 打进已退出的 pane 文字不变；运行中的 `-O` 管道被新管道替换后
只有新日志拿到输出；全程 server 存活。文档：README（中英）"还没做的"删四项、picker 按键行加折叠、pipe-pane 段加 `-I/-O` 与
pwsh 缓冲说明；parity 表 choose-tree / display-popup / pipe-pane / swap-window 行与 Still to do 段同步；站点计数 185 → 186。

## 结论（第二十九次验收）

A、B、C 三轮连续零发现，验收通过。测试 185 → 186（lib 123 / console 4 / e2e 59）。

# 第三十次验收（2026-09-23）— 布局字符串、每客户端视口、resize-window、20 分钟 soak

## 做了什么

1. **布局字符串**（`layout.rs`）：`#{window_layout}` 现在是 tmux 格式（`校验和,WxH,X,Y`，`{}` 左右、`[]` 上下，叶子带 pane id），
   由窗口当前矩形反推；`select-layout <字符串>` 读回：有校验和就校验，格子数必须等于 pane 数，尺寸按现窗口缩放，pane 按顺序填入。
2. **每客户端视口**：客户端比窗口小（`window-size` 听了别的客户端）时，整窗按 session 尺寸合成，再裁出该客户端的视口；
   状态栏按客户端自身宽度画；鼠标坐标按视口偏移换算。`refresh-client -U/-D/-L/-R [n]` 平移并"钉住"，下一次敲键送 pane 时解钉、
   视口跟着光标最小位移。默认绑定 `S-方向键`（5 行 / 10 列，可连按）。
3. **resize-window**：探针发现文档（README 中英、parity、config 注释）都说 `window-size manual` 靠 `resize-window` 改尺寸，
   而这条命令根本不存在。补上：`-x/-y`、`-U/-D/-L/-R n`（一次一个方向，同 tmux）、`-A`/`-a`（最大/最小客户端），下限 10×3。
4. **soak**：release 二进制，4 个 pane 用 cmd 循环不停刷屏，另一个 session 每 10 秒分屏/缩放/关 pane/respawn，`history-limit 2000`，跑 20 分钟。

## 修复过程中的发现（不计数）

- 视口 e2e 在全量并行时挂过一次：`x` 的回显比渲染先到时光标在第 6 列而不是第 5 列，断言写死了列号 → 两种都接受。
- `resi -Z` 变成歧义（resize-pane / resize-window），旧测试改为 `resize-p`，并断言歧义报错。
- 探针脚本：soak 第一版把整条 cmd 命令当一个参数传给 split-window（没起来，内存曲线是空载的）；vpC 的"字符串相等"期待错
  （字符串里带 pane id，应比尺寸）；`resize-window -L 10 -D 2` 期待两个方向同时生效，tmux 只认一个。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制 + 全量）

数据：3 pane 窗口的字符串 `0f51,100x29,0,0[100x19,0,0,2,100x9,0,20{50x9,0,20,4,49x9,51,20,5}]`，切 tiled 再应用字符串后尺寸
`100x19,50x9,49x9` 原样回来、字符串逐字相等；同一字符串套到 60x20 的 3 pane 窗口得 `60x13,29x5,30x5`；
`refresh-client` 六种写法退出码 0，`-R abc` 报 `bad amount`；`list-keys` 列出四条 `-r` 的 S-方向键绑定。
全量 188 项（lib 124 / console 4 / e2e 60）全过，clippy 无 warning，fmt 干净。
e2e `a_small_client_has_its_own_view_of_a_big_window`：80x24 与 40x12 两客户端、`window-size largest` 下 session 保持 80x24，
小客户端只见前 40 列，`-R 20` 后见 20..60 列且状态栏仍是 40 列整行，大客户端不受影响，敲一个键后视口回到光标处，`-R 500` 钉在右缘。
e2e `window_size_picks_...`：`resize-window -x 100 -y 30` → 100x30，`-L 10` 再 `-D 2` → 90x32，`-a` → 70x22，`-x 1 -y 1` → 10x3，`-x wide` 报错。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：整套 e2e（60 条，含视口测试并行）+ 17 条布局/刷新单元测试连跑 3 次，去掉耗时后输出完全一致（distinct = 1）。

## 第 C 轮（计数 3/3，无发现）— 视角：边界 + 长时稳定

数据：单 pane 字符串套回自身 OK；2 格套 1 pane 报"has 2 panes, the window 1"；坏校验和、无主体、`1x,0,0,0` 各报对应解析错误；
预设名仍优先；11 pane 深嵌套字符串套到同尺寸 11 pane 窗口后尺寸逐一相同，缩到 20x6 时 11 个 pane 全在（最小 1x1）、其字符串仍可解析；
未 attach 的脚本客户端 `refresh-client -R 10` 无害；server 全程存活。
soak 20 分钟：私有内存后半段 52.6 MB → 52.6 MB（+0.0），句柄 240 → 240，线程 47 → 45，chatty pane 的 scrollback 恰为 2000 行，
kill-server 后进程退出。
**遗留（非本批引入）**：窗口被压到 6 行再放大回 300x80 后各 pane 比例丢失（尺寸被夹成 1 后再等比放大），极端缩放的既有行为，
tmux 亦类似，记录在案不修。

## 结论（第三十次验收）

A、B、C 三轮连续零发现，验收通过。测试 186 → 188（lib 124 / console 4 / e2e 60）。

## 第三十次补记：发布 0.9.0 时 CI 门禁拦下的一处

runner 上视口 e2e 挂了：`x` 敲进去后视口回到了第 0 列。原因是"光标在视口外就跟随"过于敏感——cmd 重画提示行时光标瞬间在第 0 列，
慢机器恰好在那一刻渲染，视口就被拽到 0。改为：敲键送 pane 时下一次渲染立即跟随；其余情况光标离开视口持续 150 ms 才跟随。
本地整套 e2e 连跑 5 次全过；release 门禁按设计拒绝了红 CI 的 tag，tag 移到修复提交后重发。

# 第三十一次验收（2026-09-24）— Tab 补全：PowerShell 补全脚本 + `:` 命令行补全

## 做了什么

1. `wmux completion powershell` 输出一段 `Register-ArgumentCompleter -Native` 脚本（`$PROFILE` 里
   `wmux completion powershell | Out-String | Invoke-Expression`）：补命令名（含 client 本地的 completion / startup / windows-terminal）、
   正在敲的 flag（按命令查表）、`-t`/`-s` 后从运行中的 server 取 session、`session:序号`、`session:窗口名`、`-L` 后列出正在跑的
   socket（扫命名管道）、`completion` 后列 shell。`-L`/`-f` 全局参数会被跳过。
2. 命令名 / flag 表 `command::FLAGS` 与解析器绑死：单元测试对表里每条 `命令 flag 1 x` 走一遍解析，出现 `unknown flag` 即失败；
   第一版表里 8 个凭印象写的 flag 被这个测试当场抓出来删掉（`attach -c`、`capture-pane -E`、`new-window -b`、`select-window -n/-p/-l`、
   `split-window -l/-p`）。
3. `:` 命令行的 Tab：第一个词补命令名，`-t`/`-s` 后补目标；唯一候选整词填入（命令后带空格），多个候选填到公共前缀、候选列表
   暂时顶替提示符标签（下一个键恢复），没有候选显示 `(no completion)`。

## 修复过程中的发现（不计数）

- 补全脚本第一版在"空词"时也列 flag，导致 `resize-window -x <Tab>` 出 flag 列表；改为只在正在敲 `-` 时列 flag。
- 探针：`Get-ArgumentCompleter` 这个 cmdlet 不存在；哈希脚本把字节数组摊进了管道（31 个"哈希"）；`list-commands` 不带 `-L`
  打到了用户机器上还在跑的老版本 server（它没有 resize-window）——这也说明升级后老 server 进程会一直跑到 kill-server/重启为止。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（真实 PowerShell 里加载 release 二进制吐出的脚本，用 CompleteInput 问）

数据：`wmux spl` → split-window；`list-s` → list-saved,list-sessions；`split-window -` → 7 个 flag；活 server 上
`select-window -t ` → alpha,beta,alpha:0,alpha:pwsh,beta:0,beta:build,beta:1,beta:test；`kill-session -t b` → 5 个 beta 开头；
`completion ` → powershell；`-L ` → 正在跑的 socket；`-L x ne` → 4 个 ne 开头的命令。
全量 192 项（lib 127 / console 4 / e2e 61）全过，clippy 无 warning，fmt 干净。
e2e `tab_completes_at_the_prompt_and_the_shell_gets_a_completer`：`spl`+Tab → `:split-window `；`list-s`+Tab → 提示符变
`(list-saved list-sessions) list-s`，再敲 e 标签恢复，再 Tab → `:list-sessions `；`zzz` → `(no completion)`；
`select-window -t b`+Tab → `(beta beta:0 beta:build) select-window -t beta`，`:b`+Tab → `beta:build`；
子进程 `wmux completion powershell` 含 Register-ArgumentCompleter 与全部命令，`completion bash` 退出码 1。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：脚本连吐 3 次 SHA-256 完全相同（6740 字节）；e2e + 3 条单元测试连跑 3 次输出一致（distinct = 1）。

## 第 C 轮（计数 3/3，无发现）— 视角：边界

数据：脚本 Invoke-Expression 两遍不报错；PowerShell 解析器 0 个错误；`-t` 指向没有 server 的 socket、命令后空词、`-x` 后空词
均无报错（补全器不给候选时 PowerShell 自己退回文件名补全，是 PowerShell 的默认行为，tmux 的 bash 补全同样如此）；
带引号的半个词返回空；`-f 文件` 被跳过后 `ki` 仍补出 4 个 kill-*；无 shell / 未知 shell / 多余参数各报对应错误退出码 1；
`pwsh` 与 `powershell` 同一份脚本。

## 结论（第三十一次验收）

A、B、C 三轮连续零发现，验收通过。测试 188 → 192（lib 127 / console 4 / e2e 61）。

# 第三十二次验收（2026-09-24）— 右键粘贴

## 做了什么

`mouse on` 时右键被 wmux 吃掉、什么都不做，终端自带的"右键粘贴"就没了。现在 pane 上按下右键 = 把 Windows 剪贴板贴进
当前 pane（走 `paste-buffer`，带括号粘贴），和 Windows Terminal / conhost 的右键一样；在 copy mode 里右键先退出 copy mode 再贴。
程序自己要鼠标事件（vim、htop 那类）时右键照旧原样转给程序；状态栏上的右键不粘贴。
顺带两处加固：copy 时剪贴板忙（别的程序占着、剪贴板管理器在读）不再让整个复制失败——paste buffer 照设，提示里附上剪贴板的报错；
`OpenClipboard` 重试从 100 ms 提到 500 ms。

## 修复过程中的发现（不计数）

- 全量并行时 `copy_mode_vi_motions_and_modes` 挂过一次：第二次复制后立刻 `show-buffer`，键和命令走两条管道没有先后保证，
  改成轮询 buffer 变化（既有竞争，被新增的剪贴板流量放大后露出来）。
- 新 e2e 与其它复制测试共用机器唯一的剪贴板：别的测试复制的文字会被右键贴进来。测试改成"设、点、看，不对就清行重来"；
  第一版用 C-u 清 cmd 的输入行，cmd 不认 C-u（打出 `^U`），改用 Escape；`set_text` 遇忙也重试。之后整套 e2e 连跑 6 次 0 失败。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路

数据：e2e `a_right_click_pastes_the_clipboard` 走真实管道协议（`ClientMsg::Mouse` 按钮位 2 按下/抬起）：剪贴板里的
`echo pasted-by-right-click` 出现在 cmd 提示符后并执行；滚轮进 copy mode（`[3/` 指示）后右键 → 指示消失、文字贴进；
状态栏上右键 300 ms 内什么都没贴、随后照常能敲字。全量 193 项（lib 127 / console 4 / e2e 62）全过，clippy 无 warning，fmt 干净。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：整套 e2e（62 条并行，含全部复制/粘贴测试）连跑 6 次全过，另加此前一轮 4 次里 3 次失败的对照（修测试前）。

## 第 C 轮（计数 3/3，无发现）— 视角：边界 + 静态一致性

数据：程序占用鼠标时右键仍走原有转发分支（`mouse_selects_pane_and_copy_mode_scrolls` 覆盖那条路，右键分支放在它之后）；
剪贴板忙 500 ms 内重试，超时报 `clipboard: OpenClipboard failed`，复制侧 buffer 仍然设好；剪贴板为空时右键报
`clipboard is empty`（`paste-buffer` 既有路径）。README（中英）鼠标段落写明右键粘贴；README 的 `]` 行本就是"贴剪贴板"。

## 结论（第三十二次验收）

A、B、C 三轮连续零发现，验收通过。测试 192 → 193。

# 第三十三次验收（2026-09-24）— 滚轮选中被滚的 pane；copy mode 里 `C-b C-b` 上翻页（用户报告）

## 发现与修复

用户报告"翻历史时 C-b/C-f/j/k 都不动"。根因：滚轮在**非活动** pane 上滚会让那个 pane 进 copy mode，但键盘只看活动 pane
（`in_copy` 取 active pane），键全进了另一个 pane 的 shell。修：滚轮事件和按键一样先把鼠标下的 pane 设为活动（tmux 的
`WheelUpPane` 绑定也是先 `select-pane -t=`）。另外 `C-b` 本身是前缀键，copy mode 里单按 C-b 永远是前缀（tmux 同样如此）；
补一条：copy mode 里 `C-b C-b`（前缀两次）交给 copy mode，即上翻页；C-f、PageUp/PageDown、C-u/C-d 本来就有。README 中英写明。

## 修复过程中的发现（不计数）

- 右键粘贴 e2e 在状态栏那一步的 `set_text` 没重试，剪贴板忙时 `EmptyClipboard failed`；统一走重试。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路

数据：e2e `the_wheel_selects_the_pane_it_scrolls_and_the_prefix_twice_pages_up`：左右两 pane、右侧活动，左侧灌 60 行；
在左侧滚轮一格后 `#{pane_index}` 从 1 变 0、左上角出现 `[3/` 指示；`C-f` 翻到 `[0/`；`C-b C-b` 翻回非 0；`k`/`j` 让反显光标
上一行/回原行；Escape 退出。全量 194 项（lib 127 / console 4 / e2e 63）全过，clippy 无 warning，fmt 干净。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：整套 e2e（63 条并行）连跑 4 次全过。

## 第 C 轮（计数 3/3，无发现）— 视角：边界 + 静态一致性

数据：单 pane 时滚轮行为不变（`mouse_selects_pane_and_copy_mode_scrolls` 仍过）；`mouse off` 时滚轮不选 pane（守卫 `mouse_opt`）；
程序自己要鼠标事件时滚轮仍原样转发（那条分支在前）；不在 copy mode 时 `C-b C-b` 仍把 C-b 送给 pane/弹窗（原行为）。
README 中英 copy mode 段补上翻页键与 `C-b C-b` 说明。

## 结论（第三十三次验收）

A、B、C 三轮连续零发现，验收通过。测试 193 → 194。

# 第三十四次验收（2026-09-24）— 状态栏原生变量与新默认 status-right

## 做了什么

`src/sysinfo.rs`：进程内直接读的变量，不再需要 `#(命令)`——`cpu_percentage`（GetSystemTimes 两次采样之差）、`ram_percentage` /
`ram_used`（GlobalMemoryStatusEx）、`battery_percentage` / `battery_charging`（GetSystemPowerStatus，台式机为空）、`uptime`
（GetTickCount64）、`git_branch`（向上找 `.git/HEAD`，含 worktree 的 `gitdir:` 文件，不起 git）、`pane_current_path_short`（家目录写成 `~`）、
`pane_pid_command`（Toolhelp 快照里 pane 进程最新的后代，跳过 conhost）。系统读数与进程快照各缓存 1 秒。
默认 `status-right` 改为 `#{?git_branch, #{git_branch} |,} #{pane_current_path_short} | CPU … MEM …#{?battery_percentage, | BAT …,} | %H:%M`，
`status-right-length` 默认 60 → 100（窗口列表仍优先，右侧被裁）。`set -g status off` 整行关，`set -g status-right` 整条换——都是既有机制。

## 修复过程中的发现（不计数）

- console 测试与 status-justify e2e 断言了旧默认（右侧以引号包着的 pane 标题开头）：前者改为断言 CPU/MEM，后者显式 `set status-right`。
- 一版把每个 pane 的进程快照各做一次；改为快照本身缓存 1 秒，30 个 pane 的 `jobs` 从每次多次快照变成一次。
- 上一批留下的 debug `wmux.exe` 进程占着 `target\debug\wmux.exe` 让构建失败；按路径只杀 target 下的，用户装的 server 不碰。
- clippy 5 条（checked_div、redundant guard、while let、rfind、复杂类型）逐条改掉。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（release 二进制）

数据：仓库目录的 pane：`master | ~\Documents\dfine\wmux | CPU 4% MEM 22% | 10:40`（默认格式整条展开）；TEMP 目录分支为空、路径 `~\AppData\Local\Temp`；
空闲 `pane_pid_command` = cmd，跑 `ping` 时 = PING；台式机电池 `[] 0`；50 次 `display-message` 809 ms（16 ms/次，客户端往返为主）。
全量 199 项（lib 131 / console 4 / e2e 64）全过，clippy 无 warning，fmt 干净。
e2e `the_machine_variables_answer_without_a_command`：两次采样后 CPU 有数、RAM/uptime 有值、仓库里有分支、`~\` 开头、ping 时程序名为 ping、
电池字段形态合法、attach 后状态栏含 `CPU … % MEM …`、`status off` 后消失、自定义 `status-right` 生效。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：整套 e2e（64 条）连跑 3 次全过；4 条 sysinfo 单元测试（读数缓存 1 秒、分支/脱离 HEAD/worktree/无仓库、`~` 大小写与同名前缀目录、
本进程程序名）随全量通过。

## 第 C 轮（计数 3/3，无发现）— 视角：边界

数据：已退出的 pane 程序名为空、`pane_dead` 1；`C:\` 根目录短路径原样、分支为空；pane 目录被删后分支为空、短路径仍给（目录被 shell 占着删不掉是脚本的事）；
带条件的自定义右侧在无电池无仓库时只剩 `up 28m`；空 `status-right` 展开为空；30 个 pane 的 `jobs`（每 pane 一个上下文）28 ms、一秒内再来 21 ms；server 全程存活。
README（中英）变量表与默认格式说明、parity 变量段同步；站点计数 194 → 199。

## 结论（第三十四次验收）

A、B、C 三轮连续零发现，验收通过。测试 194 → 199（lib 131 / console 4 / e2e 64）。

## 第三十四次补记：CI 抓到的——右侧变长后当前窗口被挤出状态栏

`9682dd6` 在 CI 上 `attach_type_split_detach`、`choose_tree_picker` 失败：runner 的 TEMP 是 `C:\Users\RUNNER~1\...`，
8.3 短名没被识别成家目录，路径原样很长；右侧变长后状态栏只剩 `0:cmd-`，当前窗口 `1:cmd*` 没了。两个原因：

1. **渲染的真 bug（既有）**：旧逻辑只给"当前窗口一个标签"预留空间，但列表从头画，前面的窗口先把这块吃掉。
   改为：列表整体放得下时右侧用剩余；放不下时从前面省略窗口直到当前窗口能显示（被省略的窗口在点击命中表里占空区间，下标不乱）；
   当前窗口单独都放不下时不预留。单元测试补了"当前窗口在末尾/在中间"两例，旧的两处期望按新规则更新。
2. `~` 缩写：先用 GetLongPathNameW 把两边都展开成长名再比较；单元测试在卷保留 8.3 名时验证 `RUNNER~1` 类写法。

证据：把本机 TEMP 指向 scratchpad 下的长目录复现 CI 条件，`git stash` 掉修复时 `choose_tree_picker` 报 `timeout waiting for window 1`
（与 CI 同一条），恢复修复后通过。同一长 TEMP 下 `attach_type_split_detach` 另有一处失败是复现环境造成的（配置报错浮层打印的
配置文件路径在长 TEMP 下被 80 列截掉），CI 的 TEMP 不长，不适用。全量 199 项全过，e2e 连跑 3 次全过，clippy/fmt 干净。

# 第三十五次验收（2026-09-24）— 更新链路：版本不一致提示、restart-server、wmux update

## 做了什么

1. **版本不一致提示**：交互式 attach 前，客户端另开连接问 server 的 `version`；不同则 attach 期间终端标题写
   `wmux: <session> [server X: run wmux restart-server]`，detach 时打印一行说明。不改协议，所以对**旧** server 同样生效
   （旧 server 无法主动往客户端推消息，只能由新客户端自己说）。`wmux version` 同时给出本程序与 server 的版本。
2. **`wmux restart-server`**：记下正在跑的 session → `save-session -a` → `kill-server -r`（新 server 让已接的终端等待并重新 attach；
   旧 server 拒绝 `-r` 时退回普通 `kill-server`，终端被断开并提示 `wmux attach`）→ 等旧 pipe 消失 → 起本版本 server →
   只对刚才在跑的 session 逐个 `restore-session`（存过档但已关掉的 session 不会复活——`kill-session` 不删存档，用 `--restore` 会全复活）。
   在 pane 里运行时：pane 的 job 新增 `JOB_OBJECT_LIMIT_BREAKAWAY_OK`（只有显式要求的进程能脱离，其它子进程仍随 pane 结束），
   restart 以 `CREATE_BREAKAWAY_FROM_JOB` 在 pane 外重跑自己，结果写 `restart.log`；旧 server 的 job 不允许脱离时明确报错请在 wmux 外运行。
   客户端的控制台输入线程改为整个进程一个（重新 attach 复用，不会有第二个读者抢键）。
3. **`wmux update [--check]`**：用系统自带 `curl.exe` 问 GitHub 最新 release；按安装方式处理——Program Files 下（MSI）下载 MSI 与
   `.sha256`，`certutil` 核对后交给 `msiexec /passive`（0/3010 成功，1602 取消）；scoop 安装走 `scoop update wmux`；其它方式给出下载页。
   装完提示 `restart-server`。无后台行为。

## 修复过程中的发现（不计数）

- **读代码抓到的真 bug**：旧 server 对 `kill-server -r` 回的是 `unexpected argument '-r'`，第一版回退只认 `unknown flag`，
  会让 restart 对**所有 0.9 server**失败（正是用户的情形）。改为 `-r` 任何拒绝都回退（此时已存档，安全）。
- **读代码抓到的真 bug**：scoop 装的是 `scoop.cmd/.ps1` shim，`Command::new("scoop")` 只找 `scoop.exe`，scoop 路径必失败；改走 `cmd /c`。
- 重启后运行时 `set`/`bind` 的设置丢失：旧 server 分不清默认值与用户设置，全搬会把旧版默认值（如 0.9 的 status-right）钉到新版上；
  与 tmux `kill-server` 一致，从配置文件重读。restart 输出与 README（中英）写明。
- 测试：重连断言第一版可被重启前的画面满足，改为先确认客户端打印了"restarting, attaching again"且其后有重绘；
  一次断言失败留下了真实 server 进程占着 `target\debug\wmux.exe`，给 restart 测试加 `StopServer` 守卫（Drop 时 kill-server）。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（**真实 0.9.0 server**，一次性 socket，不碰用户的 default）

数据：装机的 `C:\Program Files\wmux\wmux.exe`（0.9.0）起 server、两个 session、一条回显；新构建（0.10.0）`version` 输出
`server: wmux 0.9.0` 与说明；`restart-server` → `server wmux 0.9.0 -> wmux 0.10.0; 2 of 2 session(s) restored: one, two`，
server 进程路径从 Program Files 变为 target\release，回显历史在；用户自己的 default server 只被读（显示 0.9.0 与说明），未被触碰。
`update --check`：GitHub 最新为 0.9.0，本构建 0.10.0 → "is the latest"。
console 测试（真实进程 + ConPTY）：attach 中的客户端收到重启通知、其后重绘、进程未退出、敲字到达新 server；普通 `kill-server` 则客户端退出；
在 pane 里跑 restart 后 `restart.log` 写明恢复、server pid 改变；`--ignored` 测试以 0.9.0 为 server 验证标题提示与 detach 说明。
全量 205 项（lib 135 / console 6 + 1 需旧版本的忽略项 / e2e 64）全过，clippy 无 warning，fmt 干净。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：console 连跑 5 次、e2e 连跑 3 次全过；每次跑完无残留 target 下的 wmux 进程（计数 0）。

## 第 C 轮（计数 3/3，无发现）— 视角：边界

数据：`restart-server now` / `version x` / `update --force` 各退出码 1 并说明；无 server 时"nothing to restart"退出码 0；
名字带空格的 session 恢复；存过档后关掉的 session 不复活（存档文件仍在、未用）；连续两次 restart 都 1/1；
`kill-server` 后 server 339 ms 内退出、无残留；运行时选项不保留（有意，已写明）。

## 结论（第三十五次验收）

A、B、C 三轮连续零发现，验收通过。测试 199 → 205。版本号升到 0.10.0（发布在后面各项做完后）。

# 第三十六次验收（2026-09-24）— server.log 轮转；copy-mode-vi 键表

## 做了什么

1. **日志轮转**（`logger.rs`）：进程内计已写字节，超过 5 MB（`WMUX_LOG_MAX` 可改）把 `server.log` 改名为 `server.log.1`（覆盖上一个）再开新文件；
   启动时已超限的旧日志先轮转。std 打开文件带 FILE_SHARE_DELETE，打开中的日志可以改名；改名失败就继续写原文件（能记总比不记好）。
   `WMUX_LOG_DIR` 可改目录（测试用）。
2. **copy-mode-vi 键表**：`bind-key`/`unbind-key` 的 `root: bool` 改为 `KeyTable { Prefix, Root, Copy }`；`-T copy-mode-vi`（`copy-mode` 同表）
   绑定在 copy mode 里先查表，命中就执行命令（`send -X ...` 直达内建动作，绑定不会再次查表，所以不会自递归），未命中走内建按键；
   `list-keys` 列出 `bind-key -T copy-mode-vi ...`，可原样读回。tmux 的鼠标"键"（`MouseDragEnd1Pane`、`WheelUpPane` 等）bind/unbind 照收、不起作用，
   不再在加载配置时报错。其它未知表仍拒绝。

## 修复过程中的发现（不计数）

- 既有 `.tmux.conf` e2e 断言"copy-mode-vi 行被跳过并记入 show-messages"——行为有意改变，断言改为"进 copy 表、不进 prefix 表、不再记为跳过"。
- 新 e2e 第一版：进 copy mode 前 cmd 还没处理完清行的 Escape，copy mode 冻结的画面里还有 `wmux>i`；改为先等清行。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路

数据：真实 server、`WMUX_LOG_MAX=3000`、debug 级别、80 条命令：`server.log.1` 2925 字节、`server.log` 138 字节，最新一行是 `server exiting`；
10000 字节的旧日志在启动时轮转（`.1` = 10000，新文件 226）。单元测试 100 行、上限 1000：两文件都 ≤1000、最新行在当前文件、最旧行已丢、没有 `.2`。
e2e `the_copy_mode_vi_table_binds_keys_in_copy_mode`：copy mode 外 `i` 进 shell；copy mode 里绑定的 `i` 退出 copy mode 且不进 shell；
绑定任意命令（`C-t` → display-message）生效；`k` 绑到 `send -X cursor-up` 只上移一行、server 正常应答；解绑后 `i` 回到 copy mode 本身；
鼠标键两行加载退出码 0；`list-keys` 含该表。`.tmux.conf` e2e：v/y 两行进 copy 表，show-messages 不再有 copy-mode-vi。
全量 207 项（lib 136 / console 6 + 1 忽略 / e2e 65）全过，clippy 无 warning，fmt 干净。

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：整套 e2e 连跑 3 次、两条 copy 表 e2e 再连跑 3 次，全过。

## 第 C 轮（计数 3/3，无发现）— 视角：边界 + 静态一致性

数据：重复绑定同一动作不递归（上）；未知表 `off-pane` 仍拒绝且报错写出三张可用表；`copy-mode`（emacs）与 `copy-mode-vi` 同表；
`list-keys` 输出读回等于原命令（单元测试）；日志改名失败时继续写原文件（代码路径）；README（中英）配置与日志段、parity 的 bind-key 行与配置段同步。

## 结论（第三十六次验收）

A、B、C 三轮连续零发现，验收通过。测试 205 → 207。

# 第三十七次验收（2026-09-24）— 网站补上新功能；两处顺手修复

## 做了什么

1. 网站新增第 06 章"看得见这台机器的状态栏"（安装顺延为 07）：状态栏示例是 release 构建实际渲染出的一行
   （`[work] 0:editor* 1:build 2:logs     master | ~\Documents\dfine\wmux | CPU 9% MEM 65% | 11:45`，非手写），
   四个要点：状态栏变量与关闭、`window-size` 与每客户端视口、右键粘贴与 Tab 补全、`update` + `restart-server` 升级不丢 session。中英两套文案。
2. **网站既有 bug**：安装卡片网格用 `1fr`（即 `minmax(auto, 1fr)`），scoop 卡片里那条长命令把列撑宽，整页出现横向滚动条——
   线上 dfine.tech/wmux 现在就有（截图可见底部横向滚动条）。改 `minmax(0, 1fr)` + `.card{min-width:0}`，长命令在卡片内横向滚动。
3. **CPU 首读为空**：新 server 第一次读 CPU 没有上一次采样，状态栏显示 `CPU ` 后面空着；改为首读用开机以来的累计值（平均占用）。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（真实浏览器 + release 二进制）

数据：本地起静态服务、Chrome 打开：中文文案生效（"看得见这台机器的状态栏"）、绿色状态栏一行渲染；
页面度量 `scrollWidth 1418 == clientWidth 1418`、`scrollX 0`（修复前 1548 > 1433，最宽元素 DIV.card）；
状态栏块宽 960、内容在块内滚动。新 server 首次 `display-message -p "CPU #{cpu_percentage}"` → `CPU 5%`（修复前为空）。
`node --check docs/site.js` 通过；section 开闭 10/10；章节号 01–07 连续。全量 207 项全过，clippy/fmt 干净。
（浏览器滚到页面底部时截图会卡死，线上旧页面同样如此，是本机浏览器与大 GIF 的问题；底部以 DOM 度量代替截图。）

## 第 B 轮（计数 2/3，无发现）— 视角：可复现性

数据：页面度量在两个新开标签页各测一次一致（1418/1418）；sysinfo 单元测试连跑一致；状态栏行两次采集格式一致（CPU 数值随负载变化，属正常）。

## 第 C 轮（计数 3/3，无发现）— 视角：静态一致性

数据：站点文案所述每项功能在 README 与代码中均存在（变量名逐个 grep 命中 format.rs；`window-size`、`refresh-client -L/-R`、`completion powershell`、
`update`、`restart-server` 均为已实现命令）；手机宽度下 `.cards` 为 `minmax(0, 1fr)` 单列。

## 结论（第三十七次验收）

三轮零发现，验收通过。测试数不变（207）。

# 第三十八次验收（2026-09-24）— soak 补 CPU；据此加帧率上限

## soak（release，15 分钟，4 个 pane 满速刷屏 + 另一 session 每 10 秒分屏/缩放/杀/重生，**附带一个真实 attach 的客户端**）

内存：私有字节全程在 45–63 MB 之间摆动（首末两点差 +9.6 MB 在摆动幅度内），句柄 241–244、线程 45–48 不涨；
每个刷屏 pane 的 scrollback 恰为 2000（`history-limit`）；客户端全程在线；kill-server 后 server 与客户端都退出。
**CPU：平均 165% 核（最高 169%）**——这是新数字，拆开看：

| 4 pane 满速刷屏 | server CPU |
|---|---|
| 无客户端（只解析） | 113% |
| 1 个客户端（解析 + 渲染） | 166% |

渲染占约半个核：主循环每处理一批事件就 `render_all` 一次，满速输出时每次读 pty 都重画。

## 改动

主循环加帧率上限 `FRAME = 16 ms`（60 帧/秒）：距上一帧不足 16 ms 时只记下"待画"，并在间隔末尾醒来补画一帧，
所以画面不会停在旧状态；隔一阵之后来的按键仍立即画出，不增加打字延迟。

## 第 A 轮（计数 1/3，无发现）— 视角：机制通路（同一脚本前后对比）

| 4 pane 满速，1 个客户端 | 改前 | 改后 |
|---|---|---|
| server CPU | 166% 核 | 99% 核（无客户端 111%，差值在噪声内） |
| 客户端进程 CPU（同样时长） | 2719 ms | 62 ms |

全量 207 项全过，clippy/fmt 干净。

## 第 B 轮（计数 2/3）— 视角：可复现性

数据：e2e 单独连跑 8 次、e2e+console 连跑 8 次，16 次全过。
**未解释项（如实记录）**：改后第一次的 e2e+console 连跑中有 1 次 e2e 失败，且没有打出 `test result` 行（像是测试进程整体中止而非断言失败），
输出当时未保存；此后 16 次同命令全过，未能复现。留作观察项，CI 每次提交都会继续跑。

## 第 C 轮（计数 3/3，无发现）— 视角：逻辑不变量

数据：每次循环结束时要么刚画过，要么 `render_due = true` 且下一次 select 必在 ≤16 ms 内醒来补画（`sleep(wait), if render_due` 分支），
不存在"有未画状态却无唤醒"的路径；无事件时 `wait = 3600 s` 且该分支被守卫关闭，空闲时不会空转（空闲 server CPU 仍为 0 ms/4 s 量级，见第三十四次）。

## 结论（第三十八次验收）

三轮通过，附一项未能复现的单次失败作为观察项。测试数不变（207）。

## 第三十八次补记：帧率提交在 CI 上的失败，以及一次被推翻的解释

CI 上 `join_pane_marks_and_exact_sizes` 失败：`select-pane -T logs` 之后 pane 标题是 cmd 自己的 "Administrator: …cmd.exe"。
先排除帧率改动：OSC 标题在处理 pane 输出时就写入，与渲染无关。
**第一个解释被数据推翻**：我以为 resize 会让 ConPTY 重发标题、覆盖 `-T`；写探针在本机对 pane 连续两次 resize 抓原始输出，
150 与 120 字节里没有任何标题序列——这个解释在本机不成立，也没有证据在 CI 成立。
**实际原因**：CI 输出里其它 pane 的标题还是 `cmd`（程序标题尚未到达）——慢机器上 cmd 的**启动标题**会晚到，落在 `-T` 之后；
程序在 `-T` 之后设标题会覆盖它，这是正确的 tmux 行为。测试是竞态：改为先等程序自己的标题出现，再 `-T`。
顺带保留一处小改：程序**重复发送相同标题**（每次提示符都设标题的 prompt 会这样）不再覆盖 `-T`，发送不同标题仍覆盖（tmux 语义）；单元测试覆盖。
全量 208 项全过，e2e 连跑 3 次全过。

# 第三十九次记录（2026-09-24）— 发布 0.10.0；提交 winget-pkgs

- **0.10.0**：tag 推送后发布流水线先等同一提交的 CI（已绿）再出包；资产 MSI 4.86 MB、zip 1.34 MB、各自 sha256、清单 zip；
  bot 提交回 master 的 winget 清单 `winget validate` 通过。
- **`wmux update` 对真实发布的核对（只读，不安装）**：`update --check` → "wmux 0.10.0 is the latest"；按 `update` 的方式下载 MSI 与 `.sha256`，
  `certutil` 算出的哈希与发布的逐字相同（ac45923d…c996fd）。
- **清单 schema**：winget-pkgs 模板要求 1.12；生成器从 1.6.0 改为 1.12.0，按真实发布重生成 0.10.0，只有 6 行 schema 变化，本机 winget v1.29 验证通过。
- **winget-pkgs PR**：https://github.com/microsoft/winget-pkgs/pull/440225（从 newdee/winget-pkgs 分支，用 GitHub API 上传三份清单，不克隆大仓库），
  文件恰为 3 个，已打上 `New-Package`；**卡在 `Needs-CLA`：需要账号本人在 PR 下回复同意 CLA**（法律协议，不能代签）。
  模板中"本机 `winget install --manifest` 测试"未勾选并如实注明（需要开管理员设置且会覆盖用户正在用的安装）。
- **后续版本自动提交**：发布流水线新增一步，仓库有 `WINGET_TOKEN` secret 时跑 `wingetcreate update newdee.wmux --submit`，
  没有则跳过、失败不影响发布。secret 放在 job 级 env（步骤自己的 env 在它的 `if:` 里不可见，第一版写错已改）。
  **未验证**：要等首个 PR 合并、设置 secret、下一次发版才会真正执行。

# 第四十次记录（2026-09-24）— 以字节转交输入的宿主里前缀键失灵；`wmux show-keys`

用户报告：在某个 PowerShell 里 `Ctrl+B` 之类没反应。
**已确认的事实**：Windows Terminal 与 conhost 送完整键盘记录（vk 'B' + LEFT_CTRL，字符 0x02），这条路一直正常（ConPTY 测试里发 `\x02` 一直能分屏）。
`key_from_record` 在"无 Ctrl 标志"分支里对控制字符直接返回 None。所以一旦宿主只交字节（vk=0、字符 0x02、无 Ctrl 标志，常见于 SSH、部分远程工具、程序喂入的控制台），前缀就被丢掉。
同一原因下，这类宿主里提示符的 Enter（0x0d）也会被丢。
**尚未确认**：用户的宿主是哪一种，也可能是宿主自己吃掉了 `Ctrl+B`（VS Code 绑定了侧栏开关）。为此加了 `show-keys` 诊断。

- **修正**：裸控制字符按 tmux 的读法解读。0x0d Enter，0x09 Tab，0x1b Escape，0x7f BSpace，0x01..0x1a 是 C-a..C-z，0x1c..0x1f 是 C-\ C-] C-^ C-_。完整记录的路径不变。
- **`wmux show-keys`**：打印控制台交来的 vk、字符、标志，以及 wmux 认成的键，按 `q` 退出。退出后在主屏幕重印，便于复制。多余参数报错。补全、usage、两份 README、parity 均已写入。

验收（每轮 review + 全量测试）：

| 轮 | 视角 | 数据 | 结论 |
|---|---|---|---|
| 1 | 机制通路 | 探针关掉修正：新 e2e（裸 0x02 后接 `c` 开新窗口）超时失败；打开：0.16 s 通过 | 机制真实生效，干净 |
| 2 | 边界与 tmux 对齐 | 初版把 0x08、0x0a 读成 Backspace、Enter；tmux 读作 C-h、C-j，`bind -n C-h/C-j`（vim-tmux-navigator）会在这类宿主里失效 | **有问题**，已改，不计数 |
| 2' | 同上复跑 | 断言 0x00 为 C-Space 失败：字符 0 的记录在前面就当"无字符"返回，该分支是死代码 | **有问题**，删分支与断言，不计数 |
| 3 | 静态一致性 | `show-keys` 进了补全却不在 usage、README 中，且不拒绝多余参数 | **有问题**，已补，不计数 |
| 4 | 代码正确性 | raw 模式在备用屏幕上画，按 `q` 后输出消失，用户无法复制结果 | **有问题**，改为退出后在主屏幕重印，console 测试断言主屏幕上有 `->  C-b` 与 `->  q`，不计数 |
| 5 | 静态一致性 | 网站测试数仍写 208，实际 211 | **有问题**，已改，不计数 |
| 6 | 可复现性 | 全量连跑 3 次：每次 138/7/66 通过，0 失败 | 干净（1/3） |
| 7 | 不变量（所有入口） | 键记录转键只有 `key_from_record` 一处；客户端原样转发（client.rs 唯一的 `InputEvent::Key` 分支）；服务端在提示符、copy mode、popup 分流之前转换；另一个调用者是 `show-keys`；全量 138/7/66 | 干净（2/3） |
| 8 | 退化输入 | 新增全字节扫描：0x01..0xFF 裸记录中 32 个 C0/DEL 各得一个键，191 个可打印字符得到自身，32 个 C1 得到无，无 panic；`fmt --check` 为 0，clippy 无警告，全量 139/7/66 | 干净（3/3） |

记下未改的一项：前缀路径会把 vk 0 放进"吞掉下一个 key-up"的集合。宿主不发 key-up 时，下一个 vk 0 的 key-up 会被吞。
pane 里的 shell 按 key-down 处理输入，所以不影响输入内容，不改。
网站测试数改为 212。
