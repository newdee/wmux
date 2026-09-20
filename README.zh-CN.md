# wmux

[![CI](https://github.com/newdee/wmux/actions/workflows/ci.yml/badge.svg)](https://github.com/newdee/wmux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/newdee/wmux)](https://github.com/newdee/wmux/releases)

Windows 上的 tmux。

用过 tmux 的人换到 Windows，最想念的大概就是它：关掉终端窗口，里面跑的东西还在；一个窗口切成几块，各干各的；`prefix d` 走人，回来 `attach` 接着干。wmux 把这套搬到了 Windows 上，而且不是靠 Cygwin 或 MSYS 模拟出来的，是直接用 ConPTY 和 Win32 控制台 API 写的，PowerShell、WSL、cmd 都能在里面正常跑。

几个要点：

- **按键原样送达。** wmux 把键盘事件按 Windows 原生的格式（Windows Terminal 用的那套 win32-input-mode）转给每个 pane，所以 PSReadLine 的组合键、`Ctrl+Space`、`Shift+Enter`、带修饰键的方向键、中文输入法、WSL 里的 vim 和 htop，表现和不用 wmux 时一模一样。
- **tmux 的肌肉记忆直接用。** `Ctrl+b` 前缀，`%` 和 `"` 分屏，`c` 开窗口，`d` 脱离，`[` 进 copy mode，`:` 敲命令。命令行也是那些名字：`new-session`、`attach`、`ls`、`send-keys`……配置文件是 `.tmux.conf` 的语法。
- **重启电脑也不怕。** 每个 session 的布局会自动存盘，开机后 `wmux resume` 就回来了。
- **能装插件。** 和 tmux 一样，插件就是一个目录加几个脚本，用 `run-shell`、hook 和状态栏格式串往里挂东西。

## 安装

到 [Releases](https://github.com/newdee/wmux/releases) 下载，两种都有：

- `wmux-<版本>-windows-x86_64.msi`：双击装到 `Program Files`，自动加进系统 `PATH`，以后在“应用和功能”里卸载。要静默装就 `msiexec /i wmux-<版本>-windows-x86_64.msi /qn`。
- `wmux-<版本>-windows-x86_64.zip`：就是一个 `wmux.exe`，解压放哪都行。

想自己编译的话（需要 Rust 1.88 以上）：

```powershell
cargo install --git https://github.com/newdee/wmux --locked   # 直接装最新的 master
cargo install --path .                                         # 本地克隆
```

需要 Windows 10 1809 或更新（ConPTY 是那时候加的）。

自己打 MSI 也不用先装什么，脚本发现本机没有 WiX 会自己下一份临时用：

```powershell
cargo build --release
pwsh -File installer/build-msi.ps1        # 产物在 target\wmux-<版本>-windows-x86_64.msi
```

## 日常用法

```powershell
wmux                      # 新开一个 session 并进入
wmux new -s work          # 起个名字
wmux new -d -s bg wsl.exe # 后台开一个跑 WSL 的 session
wmux ls                   # 看看有哪些 session
wmux attach -t work       # 接回去（换一个终端窗口也行）
wmux send-keys -t work "git status" Enter
wmux capture-pane -p -t work   # 把 pane 上的文字打印出来（-S -200 连带回滚）
wmux kill-server
```

进了 session 之后，先按前缀键 `Ctrl+b`，再按：

| 按键 | 作用 |
| --- | --- |
| `c` / `n` / `p` / `Tab` / `0-9` | 新窗口 / 下一个 / 上一个 / 刚才那个 / 按编号跳 |
| `,` / `&` | 重命名 / 关掉当前窗口 |
| `%` / `"` | 左右分 / 上下分 |
| `h` `j` `k` `l`（或方向键）/ `o` / `;` | 在 pane 之间移动（vim 键位）/ 下一个 pane / 刚才那个 pane |
| `H` `J` `K` `L`、`Alt`+方向键 / `Ctrl`+方向键 | 调整当前 pane 大小，每次 5 格 / 1 格 |
| `z` | 当前 pane 放大到整个窗口，再按一次还原 |
| `x` | 关掉当前 pane |
| `{` / `}` | 和前一个 / 后一个 pane 交换位置 |
| `!` | 把当前 pane 拆成一个独立窗口 |
| `S` | 开关 `synchronize-panes`：敲的东西同时进这个窗口的所有 pane，状态栏会多个 `S` |
| `[` / `PgUp` | copy mode：翻回滚，`Space` 开始选，`Enter` 复制 |
| `]` | 粘贴剪贴板 |
| `:` | 命令行（`:split-window -h -c C:\src`、`:set mouse off` 之类） |
| `C-s` / `C-r` | 手动保存当前 session / 恢复保存过的 session |
| `d` | 脱离 |
| `?` | 列出所有按键 |
| `s` / `w` | 弹出 session / 窗口列表挑一个：`j` `k`（或方向键）上下，`g` `G` 到头到尾，数字直接跳，`Enter` 选中，`q` 取消 |
| `(` / `)` | 切到上一个 / 下一个 session |

鼠标也管用：点一下选 pane，拖边框调大小，点状态栏上的窗口名切窗口。滚轮在普通界面上会进 copy mode 往回翻，在全屏程序里变成方向键，程序自己要鼠标事件的话就原样转过去。拖选一段文字，松手就复制到 Windows 剪贴板了。

## 重启之后接着用

每个 session 的"形状"——有哪些窗口、每个窗口怎么分的、每块里跑的是什么命令、在哪个目录——都会存成一个文件，放在 `%LOCALAPPDATA%\wmux\sessions` 下面。结构一变就存一次，`kill-server` 的时候也存。所以不管是重启、崩溃还是手滑 `kill-session`，文件都还在：

```powershell
wmux resume              # 把存过的 session 全恢复出来，进第一个
wmux resume work         # 只恢复 work（要是它本来就在跑，那就直接进去）
wmux list-saved          # 有哪些能恢复，最新的排前面
wmux delete-saved old    # 不要了
wmux save-session -a     # 现在就全存一遍（prefix C-s 存当前这个）
```

恢复出来的是布局和每个 pane 的启动命令，程序当时跑到哪、屏幕上有什么，这些是回不来的，tmux-resurrect 也一样。想让 server 一启动就自己恢复，配置里写 `set -g restore-on-start on`；不想存就 `set -g autosave off`；`sessions-dir` 可以换目录。

### pane 记住自己在哪个目录

每个 pane 一开始记的是创建时的目录。想让它跟着 `cd` 走，有两个办法。

一是让 shell 自己报告。终端界有个通用做法，prompt 里带一段 OSC 9;9 序列（Windows Terminal 也认），wmux 看到就更新，一点开销都没有：

```powershell
# 放进 $PROFILE
function prompt { "`e]9;9;$PWD`e\" + "PS $PWD> " }
```

WSL 里的 bash 用 OSC 7，`/mnt/c/...` 这样的路径会自动映射回 `C:\...`：

```bash
PROMPT_COMMAND='printf "\e]7;file://%s%s\e\\" "$HOSTNAME" "$PWD"'
```

二是手动记一下，在 pane 里敲 `wmux set-cwd`，当前目录就记下了；也可以 `wmux set-cwd -t work:0.1 D:\proj` 指定。

`list-panes` 能看到每个 pane 记的目录，状态栏里用 `#{pane_current_path}` 显示。

## 配置

配置文件是 `%USERPROFILE%\.wmux.conf`（也可以放 `%USERPROFILE%\.config\wmux\wmux.conf`，或者用 `WMUX_CONFIG` 环境变量指定），一行一条命令，就是 tmux 那种写法：

```tmux
set -g prefix C-a
set -g default-shell wsl          # pwsh（默认）、powershell、wsl、cmd，或者可执行文件路径
# set -g default-command "wsl.exe -d Ubuntu"
set -g mouse on
set -g history-limit 10000
set -g status-position top
set -g status-style fg=black,bg=colour39
set -g pane-active-border-style fg=colour39
set -g base-index 1

bind | split-window -h
bind - split-window -v
bind -n M-Left previous-window     # -n：不用按前缀
bind -n M-Right next-window
bind r source-file ~/.wmux.conf
```

`.tmux.conf` 里常见但 wmux 用不上的选项（`escape-time`、`default-terminal` 这些）会被接受然后忽略，所以现成的 tmux 配置可以直接拿来改。

所有窗口和 pane 命令都支持 tmux 风格的 `-t`：`session`、`session:window`、`:window`、`session:window.pane`，窗口那一段可以是编号、名字、`+`、`-` 或 `!`。

### 状态栏

`status-left`、`status-right`、`window-status-format`、`window-status-current-format` 接受 tmux 的格式串：`#S` session 名，`#W` 窗口名，`#I` 窗口编号，`#P` pane 编号，`#T` pane 标题，`#H` 主机名，`#F` 标记，`#{session_name}` 这种长写法，`%H:%M` 之类的时间字段，`#[fg=colour39,bg=black,bold]` 改样式，还有 `#(命令)`——每隔 `status-interval` 秒（默认 15）跑一次，取输出的第一行。`status-left-length` / `status-right-length` 限制长度。

```tmux
set -g status-right "#[fg=yellow]#(pwsh -NoProfile -c (Get-Date).ToString('HH:mm'))#[default] #H"
```

## 插件

插件的玩法和 tmux 一样：一个目录，里面放一个 `<名字>.wmux`（或 `plugin.wmux`）写 wmux 命令，再放上它需要的脚本，什么语言都行。脚本要和 wmux 说话就调命令行：环境变量 `WMUX` 是 socket 名，`WMUX_PANE` 是所在 pane，所以脚本里写 `wmux -L $env:WMUX display-message ...` 就能找到对的 server。

```tmux
# ~/.wmux.conf
set -g plugin-path ~/.wmux/plugins       # 默认就是这里
set -g @plugin demo                       # 加载 ~/.wmux/plugins/demo/demo.wmux
set -g @plugin C:\src\my-plugin           # 也可以直接给路径（目录或文件）
```

插件文件里能用配置文件的全部命令，再加上：

- `run-shell [-b] [-t target] 命令`：通过 pwsh 跑一条命令，带着 wmux 的环境变量，跑完把输出显示出来（`-b` 就不管输出了）。
- `set-hook -g <钩子> <命令>`：某件事发生时执行一条命令。钩子有 `after-new-session`、`after-new-window`、`after-split-window`、`after-select-window`、`after-select-pane`、`after-kill-pane`、`client-attached`、`client-detached`、`pane-exited`。`set-hook -gu <钩子>` 取消，`show-hooks` 查看。
- `set -g @随便什么 值` 存一个插件自己用的选项，`show-options -gqv @随便什么` 读回来（脚本里：`wmux -L $env:WMUX show-options -gqv @随便什么`）。
- 状态栏里的 `#(命令)`，见上面。
- 运行时 `load-plugin 名字或路径`、`list-plugins`。

举个例子，一个把 agent 任务进度显示在状态栏、按 `prefix A` 弹出完整日志的小插件：

```tmux
# ~/.wmux/plugins/agent-status/agent-status.wmux
set -g status-right "#[fg=cyan]#(pwsh -NoProfile -File ~/.wmux/plugins/agent-status/summary.ps1)#[default] %H:%M"
set -g status-interval 5
bind A run-shell "pwsh -NoProfile -Command Get-Content $env:TEMP\agent.log -Tail 30"
```

窗口和 pane 相关的命令都接受 `-t 目标`，写法和 tmux 一样：`session`、`session:窗口`、`:窗口`、`session:窗口.pane`，窗口那段可以是编号、名字，也可以是 `+`、`-`、`!`。只有 `select-pane` 例外，它的 `-t` 跟的是要跳到哪个 pane（`next`、`last` 或编号），和 `-L` `-R` `-U` `-D` 是一类。

## 它是怎么工作的

`wmux` 这个命令本身是个客户端。第一次运行时它会拉起一个后台 server（`wmux __server`），所有 session 都归 server 管；客户端和 server 之间走一条按用户隔离的命名管道（`\\.\pipe\wmux-<用户名>-<socket>`，`-L` 可以换 socket）。每个 pane 是一个 ConPTY，server 这边用 `vt100` 维护一份终端画面；server 把可见的 pane、边框、状态栏拼成一帧，只把变化的格子发给接上来的客户端，客户端用 VT 序列写到控制台。最后一个 session 结束，server 就退出。

pane 里能看到两个环境变量：`WMUX`（socket 名）和 `WMUX_PANE`（pane 编号）。server 的日志在 `%LOCALAPPDATA%\wmux\server.log`，`WMUX_LOG=debug` 会记得更详细。

命名管道带了只允许当前用户（和 SYSTEM）访问的 DACL，相当于 tmux 那个 0700 的 socket 目录。每个 pane 都跑在一个 kill-on-close 的 job object 里，所以 `kill-pane`、`kill-session`、server 退出都会把整棵进程树带走，不留孤儿。客户端写控制台慢的时候，server 不会无限缓冲帧，而是直接改成全量重绘。

`vendor/vt100` 是 vt100 0.16.2 加了一处修复：pane 缩小时刚好切到一个宽字符（比如中文）会越界，详见 `vendor/vt100/WMUX-PATCH.md`。server 里任何一条命令 panic 都会记到日志里然后继续跑，不会把 session 全丢了。

## 开发

```powershell
cargo test              # 单元测试 + 管道层的端到端测试 + 真实控制台测试（会拉起 cmd.exe）
cargo clippy --all-targets
```

`tests/console.rs` 会把真的 `wmux.exe` 塞进一个 ConPTY 里跑，所以控制台那条路（raw 模式、备用屏幕、脱离时的清理）不用人坐在键盘前也能测到。

## 还没做的

和 tmux 比：`synchronize-panes` 只作用于当前窗口，不支持 `-t`；多个客户端接同一个 session 时看到的尺寸是一样的（以最后接入的为准，没有每个客户端自己的视口）；钩子只有上面列的那几个；格式串不支持 `#{?条件,a,b}`；没有命名的粘贴缓冲区（只有 Windows 剪贴板）；`choose-tree` 只有列表，不能单独折叠某个 session、不能打标记、不能过滤；`swap-pane` 只能和上一个/下一个交换（`-U` / `-D`，`-s` 和 `-t` 都是指“要交换的那个 pane”，没有 tmux 那种成对指定）；没有 `select-layout` 布局预设；`bind -r` 会被接受但不会重复触发；`list-panes -a` / `-s` 只列目标窗口。
