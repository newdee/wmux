# tmux parity checklist

What tmux has, what wmux has, and what is deliberately left out. Kept next to
the code so "is that in yet?" has one answer.

Source of truth: the tmux manual (`tmux.1`) and the tables in `cmd.c`,
`key-bindings.c` and `options-table.c` from tmux master, read on 2026-09-21.
Anything marked *master only* (floating panes, `new-pane`, `move-pane -P`,
`switch-mode`) is newer than tmux 3.5 and not a goal.

Status: **yes** = works, **part** = works with a documented limit,
**todo** = planned, **no** = deliberately not doing it (reason given).

## Commands

| tmux | wmux | note |
| --- | --- | --- |
| attach-session | yes | |
| bind-key | yes | `-n`, `-r`, `-T root/prefix`; no other key tables |
| break-pane | yes | `-t`; no `-W` |
| capture-pane | part | `-p`, `-S`; no `-e` escapes, no buffer output |
| choose-buffer | yes | `prefix =`; Enter pastes |
| choose-client | todo | `list-clients` covers the looking part |
| choose-tree | part | `-s`, `-w`; no tagging, filter or per-session collapse |
| clear-history | yes | |
| clear-prompt-history | no | wmux keeps no prompt history |
| clock-mode | yes | `prefix t`, any key leaves |
| command-prompt | part | `-p`, `-I`, `%%` template; no `-k`, no numbered `%1` |
| confirm-before | yes | `-p` |
| copy-mode | yes | `-u` |
| customize-mode | no | a whole options UI; `show-options` covers the need |
| delete-buffer | yes | `prefix -` |
| detach-client | yes | `-a`, `-s`, `-t` |
| display-menu | todo | |
| display-message | yes | `-p`, formats |
| display-panes | yes | `prefix q`, digit selects |
| display-popup | todo | |
| find-window | yes | `prefix f`; one hit jumps, several open the picker |
| has-session | yes | |
| if-shell | yes | `-F` formats and shell exit status; `-b` runs inline |
| join-pane / move-pane | yes | `-h`, `-v`, `-b`, `-s`, `-t`, and the marked pane |
| kill-pane / kill-server / kill-session / kill-window | yes | `-a`, `-t` |
| last-pane / last-window | yes | |
| link-window / unlink-window | no | a window belongs to one session here; `move-window` covers the common use |
| list-buffers | yes | `prefix #` |
| list-clients | yes | |
| list-commands | yes | |
| list-keys | part | no `-N` notes, no `-T` filter |
| list-panes | part | target window only (`-a`/`-s` accepted, ignored) |
| list-sessions / list-windows | yes | |
| load-buffer / save-buffer | yes | |
| lock-client / lock-server / lock-session | no | no equivalent of a Unix screen lock here |
| move-window | part | inside one session |
| new-session | yes | `-s`, `-n`, `-c`, `-d`, `-A` |
| new-window | yes | `-n`, `-c`, `-d`, `-t` |
| next-layout / previous-layout | yes | also `select-layout -n` / `-p`, `prefix Space` |
| next-window / previous-window | yes | `-t`; `-a` (alert) accepted, ignored |
| paste-buffer | yes | `-b`, `-p`, `-t`; no `-s` separator |
| pipe-pane | todo | |
| refresh-client | part | redraws; no `-U`/`-D` viewport moves, no `-C` size |
| rename-session / rename-window | yes | |
| resize-pane | yes | `-L/-R/-U/-D`, `-x`, `-y`, `-Z`, `-t` |
| resize-window | no | the window is the client's terminal here |
| respawn-pane / respawn-window | yes | `-k`, `-t`, a command to run |
| rotate-window | yes | `prefix C-o`, `M-o` |
| run-shell | yes | `-b`, `-t` |
| select-layout | part | five named layouts; no layout strings, no `-E` |
| select-pane | yes | direction, `next`/`last`/index, `-T`, `-m`, `-M` |
| select-window | yes | index, name, `+`, `-`, `!` |
| send-keys | part | key names and `-l`; no `-H` (hex), no `-X` (copy commands) |
| send-prefix | yes | |
| server-access | no | single user by design; the pipe is owner-only |
| set-buffer / show-buffer | yes | `-a`, `-b` |
| set-environment / show-environment | yes | `-r` to remove |
| set-hook / show-hooks | part | nine hooks |
| set-option / show-options | part | the options wmux implements; unknown ones accepted and ignored |
| set-window-option / show-window-options | part | folded into `set-option` |
| show-messages | yes | `prefix ~`, the last 100 |
| source-file | yes | |
| split-window | yes | `-h`, `-v`, `-c`, `-d`, `-b`, `-f`, `-t` |
| start-server | part | any client starts it |
| suspend-client | no | no SIGTSTP on Windows |
| swap-pane | part | `-U` / `-D`; no `-s`/`-t` pair |
| swap-window | part | inside one session |
| switch-client | yes | `-n`, `-p`, `-l`, `-t` |
| unbind-key | yes | |
| wait-for | todo | |
| **wmux only** | | `resume`, `save-session`, `restore-session`, `list-saved`, `delete-saved`, `set-cwd`, `load-plugin`, `list-plugins`, `version` (tmux has `-V`), and `choose-window` / `choose-session` as names for `choose-tree -w` / `-s` |

## Default prefix keys

tmux's table, with what wmux does today.

| Key | tmux | wmux |
| --- | --- | --- |
| `C-b` | send-prefix | yes |
| `C-o` / `M-o` | rotate-window | yes |
| `C-z` | suspend-client | no (see above) |
| `Space` | next-layout | yes |
| `!` | break-pane | yes |
| `"` / `%` | split-window | yes |
| `#` | list-buffers | yes |
| `$` | rename-session prompt | yes |
| `&` | kill-window (confirm) | yes |
| `'` | prompt for window index | yes |
| `(` / `)` | switch-client -p / -n | yes |
| `,` | rename-window prompt | yes |
| `-` | delete-buffer | yes |
| `.` | move-window prompt | yes |
| `/` | describe key | no (`?` lists them all) |
| `0`-`9` | select-window | yes |
| `:` | command-prompt | yes |
| `;` | last-pane | yes |
| `=` | choose-buffer | yes |
| `?` | list-keys | yes |
| `D` | choose-client | todo |
| `E` | select-layout -E (spread) | todo |
| `L` | switch-client -l | wmux uses `L` to resize; `:switch-client -l` works |
| `M` / `m` | select-pane -M / -m | yes |
| `T` | pane title prompt | yes |
| `[` / `PPage` | copy-mode | yes |
| `]` | paste-buffer | yes |
| `c` / `d` | new-window / detach | yes |
| `f` | find-window | yes |
| `i` | display-message | yes |
| `l` | last-window | `l` is select-pane -R in wmux; `Tab` is last-window |
| `n` / `p` | next / previous window | yes |
| `o` | select-pane -t :.+ | yes |
| `q` | display-panes | yes |
| `r` | refresh-client | yes |
| `s` / `w` | choose-tree -Zs / -Zw | yes |
| `t` | clock-mode | yes |
| `x` | kill-pane (confirm) | yes |
| `z` | resize-pane -Z | yes |
| `{` / `}` | swap-pane -U / -D | yes |
| `~` | show-messages | yes |
| arrows | select-pane (repeat) | yes |
| `M-1`..`M-5` | select-layout presets | yes |
| `M-6` / `M-7` | mirrored layouts | no (master only) |
| `M-n` / `M-p` | next/previous window with alert | no (wmux has no alerts yet) |
| `C-`/`M-` arrows | resize-pane | yes |
| `S-`arrows | refresh-client -U/-D/-L/-R | no (no per-client viewport) |
| `<` / `>` | display-menu | todo |

wmux adds `h` `j` `k` `l` (move), `H` `J` `K` `L` (resize), `C-s` / `C-r`
(save / restore) and `S` (synchronize-panes) on top of that table.

## Copy mode

Movement: arrows and `h j k l`, `w` `b` `e` and their `W` `B` `E` forms,
`0` `^` `$`, `H` `M` `L`, `{` `}`, `g` `G`, `C-b` `C-f` `C-u` `C-d`, PgUp and
PgDn. A count works in front of a motion (`3j`, `2w`).

Selecting and copying: `Space` or `v` starts a selection, `C-v` makes it a
rectangle, `Enter` or `y` copies to both a new paste buffer and the Windows
clipboard, `q` or Escape leaves.

Searching: `/` back, `?` forward, `n` and `N` to repeat, case-insensitive.

Missing: `send-keys -X` (so a binding can drive copy mode), `copy-pipe`,
and the other tmux copy commands (`next-space`, `jump-to-forward`, ...).

## Still to do

In rough order of how much they would be missed: `send-keys -X`,
`display-menu` and `display-popup`, `choose-client`, `pipe-pane`,
`wait-for`, `select-layout -E`, cross-session `move-window`, `capture-pane
-e`, and the alert flags (`monitor-activity`, `M-n`, `M-p`).
