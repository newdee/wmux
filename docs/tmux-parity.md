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
| capture-pane | part | `-p`, `-S`, `-e`; no buffer output (`-b`) |
| choose-buffer | yes | `prefix =`; Enter pastes |
| choose-client | yes | `prefix D`; Enter detaches the client picked |
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
| display-menu | part | `-T` title, name/key/command triples, `""` separators; always drawn over the window (no `-x`/`-y`) |
| display-message | yes | `-p`, formats |
| display-panes | yes | `prefix q`, digit selects |
| display-popup | part | `-E`, `-C`, `-w`, `-h`, `-d`; always centred, and the prefix key stays wmux's |
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
| move-window | yes | inside a session and across sessions; a free index is taken as given |
| new-session | yes | `-s`, `-n`, `-c`, `-d`, `-A` |
| new-window | yes | `-n`, `-c`, `-d`, `-t` |
| next-layout / previous-layout | yes | also `select-layout -n` / `-p`, `prefix Space` |
| next-window / previous-window | yes | `-t`, `-a` (next window with an alert) |
| paste-buffer | yes | `-b`, `-p`, `-t`; no `-s` separator |
| pipe-pane | part | `-o`, `-t`, a command fed the pane output; no `-I` the other way |
| refresh-client | part | redraws; no `-U`/`-D` viewport moves, no `-C` size |
| rename-session / rename-window | yes | |
| resize-pane | yes | `-L/-R/-U/-D`, `-x`, `-y`, `-Z`, `-t` |
| resize-window | no | the window is the client's terminal here |
| respawn-pane / respawn-window | yes | `-k`, `-t`, a command to run |
| rotate-window | yes | `prefix C-o`, `M-o` |
| run-shell | yes | `-b`, `-t` |
| select-layout | part | five named layouts and `-E`; no layout strings |
| select-pane | yes | direction, `next`/`last`/index, `-T`, `-m`, `-M` |
| select-window | yes | index, name, `+`, `-`, `!` |
| send-keys | part | key names, `-l`, `-X` copy commands; no `-H` (hex) |
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
| wait-for | yes | `-L`, `-U`, `-S`; a waiting client is answered when the channel is signalled |
| start-server | yes | accepted; any command starts the server |
| **wmux only** | | `find-text` (search what every pane printed), `record` (a pane's output as an asciinema file), `notify` (a desktop notification), `startup on/off/status` (start at logon and restore, via the user's Run key), `resume`, `save-session`, `restore-session`, `list-saved`, `delete-saved`, `set-cwd`, `load-plugin`, `list-plugins`, `version` (tmux has `-V`), and `choose-window` / `choose-session` as names for `choose-tree -w` / `-s` |

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
| `D` | choose-client | yes |
| `E` | select-layout -E (spread) | yes |
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
| `M-n` / `M-p` | next/previous window with alert | yes |
| `C-`/`M-` arrows | resize-pane | yes |
| `S-`arrows | refresh-client -U/-D/-L/-R | no (no per-client viewport) |
| `<` / `>` | display-menu | yes (window menu / pane menu) |

wmux adds `h` `j` `k` `l` (move), `H` `J` `K` `L` (resize), `C-s` / `C-r`
(save / restore) and `S` (synchronize-panes) on top of that table.

## Copy mode

Movement: arrows and `h j k l`, `w` `b` `e` and their `W` `B` `E` forms,
`0` `^` `$`, `H` `M` `L`, `{` `}`, `g` `G`, `C-b` `C-f` `C-u` `C-d`, PgUp and
PgDn. A count works in front of a motion (`3j`, `2w`).

Selecting and copying: `Space` or `v` starts a selection, `C-v` makes it a
rectangle, `Enter` or `y` copies to both a new paste buffer and the Windows
clipboard, `q` or Escape leaves.

Searching: `/` forward (towards the newest line), `?` back through the scrollback, `n` and `N` to repeat, case-insensitive.

`send-keys -X <command>` drives all of it from a script or a binding:
`cursor-up/-down/-left/-right`, `next-word`, `next-word-end`,
`previous-word` (and the `-space` forms), `start-of-line`,
`back-to-indentation`, `end-of-line`, `top-line`, `middle-line`,
`bottom-line`, `previous-paragraph`, `next-paragraph`, `history-top`,
`history-bottom`, `page-up`, `page-down`, `halfpage-up`, `halfpage-down`,
`begin-selection`, `rectangle-toggle`, `copy-selection`, `search-forward`,
`search-backward`, `search-again`, `search-reverse` and `cancel`.

Missing: `copy-pipe` to a command (`copy-pipe-and-cancel` copies as
`copy-selection` does), and the jump commands (`jump-to-forward`, `f`/`t`).

## Config file

`~/.wmux.conf` first; with none, `~/.tmux.conf` or
`~/.config/tmux/tmux.conf` is read as tmux would read it: `\` continues a
line, a `%if` ... `%endif` block is left out whole (wmux does not evaluate
tmux's conditionals, and applying both branches would be worse than
neither), and every line wmux cannot use is skipped with a note in
`show-messages` plus a one-line count on the first attach. `bind -T` with
any table other than `root` or `prefix` is refused, so a `copy-mode-vi`
line never ends up bound under the prefix.

## Options

`show-options` prints what wmux implements; anything else common in a
`.tmux.conf` is accepted and ignored so an existing config still loads.
The ones with tmux meaning: `prefix`, `default-shell`, `default-command`,
`mouse`, `history-limit`, `status`, `status-position`, `status-style`,
`status-left`, `status-right`, `status-left-length`,
`status-right-length`, `status-interval`, `window-status-format`,
`window-status-current-format`, `pane-border-style`,
`pane-active-border-style`, `base-index`, `pane-base-index`,
`display-time`, `repeat-time`, `remain-on-exit`, `monitor-activity`,
`monitor-bell`, `monitor-silence`, `visual-bell`, `visual-activity`,
`pane-border-status`, `pane-border-format`.

## Formats

`#{...}` names: the session (`session_name` `session_id` `session_windows`
`session_attached` `session_created`), the window (`window_name`
`window_id` `window_index` `window_panes` `window_active`
`window_last_flag` `window_zoomed_flag` `window_width` `window_height`
`window_bell_flag` `window_activity_flag` `window_silence_flag`
`window_flags`), the pane (`pane_index` `pane_id` `pane_title`
`pane_current_command` `pane_start_command` `pane_current_path`
`pane_width` `pane_height` `pane_active` `pane_dead` `pane_dead_status`
`pane_synchronized` `pane_in_mode` `pane_pid`), the client
(`client_width` `client_height`) and the server (`host` `host_short`
`socket_path` `version` `pid`), plus the one-letter forms `#S #W #I #P #T
#H #F #D #h`. Modifiers: `=N:` `=-N:` `b:` `d:` `t:` `s/a/b/:`, nestable.
Conditionals: `#{?name,yes,no}`, `#{?name==value,…}`, `#{?name!=value,…}`.
Not there: tmux's `#{||:…}` / `#{&&:…}`, `#{m:pattern,var}`, `#{e|…}`
arithmetic, `#{l:…}` literals, `#{a:…}`, `#{C:…}` search, loops
(`#{S:…}` `#{W:…}` `#{P:…}`), and the client/cursor/mouse variables.
Accepted and ignored: `bell-action`, `escape-time`, `default-terminal`,
`terminal-overrides`, `focus-events`, `set-clipboard`, `renumber-windows`,
`allow-rename`, `automatic-rename`, `window-status-current-style`,
`mode-keys`, `aggressive-resize`, `set-titles`, `set-titles-string`,
`history-file`. wmux only: `save-history` (lines of each pane written into
the session file), `autosave`, `restore-on-start`, `sessions-dir`,
`plugin-path`, and `@user` options.

Not a global here: tmux's per-window and per-pane option scopes. `set -w`
and `set -p` are accepted and set the option for the server.

`notify` is a wmux option: with it on, every alert also raises a desktop
notification (a tray balloon, which Windows 10 and 11 turn into a real
notification), so a job that finishes while the terminal is behind other
windows still reaches you. tmux has nothing like it, having no desktop to
notify.

Two things wmux adds that tmux does not have: an option name may be
abbreviated as long as it stays unambiguous (`set sync`, `set mon-act on`,
`set w-s-f ...`, each dash-separated word taking a prefix), and an on/off
option with no value flips (`set mouse`, `set sync`). tmux only abbreviates
command names, and requires a value.

## Alerts

`monitor-activity`, `monitor-bell` (on by default) and `monitor-silence`
flag a window nobody is looking at: `#` for output, `!` for a bell, `~` for
silence, shown by `#F` in the status line and by `list-windows`. Looking at
the window clears its flags; `prefix M-n` / `M-p` walk to the next window
that has one. `visual-bell` / `visual-activity` put the alert on the status
line instead of ringing the terminal. Not there: `bell-action`,
`activity-action` and `silence-action` (accepted and ignored), and the
`alert-*` hooks.

## Still to do

Nothing from tmux 3.5's command table is outstanding; what is left are the
limits marked **part** above. The largest of them: no per-client viewport
(`refresh-client -U`/`-D`, `S-`arrows), no layout strings for
`select-layout`, `choose-tree` without tagging or a filter, `pipe-pane`
without `-I`, and `display-popup` without placement flags.
