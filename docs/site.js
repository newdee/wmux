// Language toggle for the wmux tour. Every translatable node carries
// data-i18n; the English text lives in the HTML, the Chinese here.
const ZH = {
  "nav.tour": "功能",
  "nav.download": "下载",
  "hero.eyebrow": "开源 · 原生 Windows",
  "hero.title": "Windows 上的 tmux。",
  "hero.lede":
    "把一个终端分成几个 pane，用 h j k l 在它们之间移动。关掉窗口，程序照样在跑；之后可以重新接上，也可以在手机上看一眼。PowerShell、cmd、WSL 都在 pane 里运行，按键原样传进去。",
  "hero.download": "下载 Windows 版",
  "hero.source": "看源码",
  "hero.meta": "MIT 许可 · Windows 10 1809 及以上 · 一个 3 MB 的 exe",
  "hero.caption": "一段完整录制：分屏、一次输入到所有 pane、移动、全屏、弹出 pane 菜单、切窗口、脱离，再接回来。",
  "stat.exe": "个文件就够",
  "stat.deps": "依赖 Cygwin / WSL",
  "stat.tests": "项自动化测试",
  "stat.license": "许可证",

  "c1.title": "分屏还是那套手感",
  "c1.sub":
    "<kbd>C-b %</kbd> 左右分，<kbd>C-b \"</kbd> 上下分。用 vim 键位或方向键移动，而且可以连着按：移动和调大小在半秒内不用再按前缀。",
  "c1.p1":
    "每个 pane 都是一个 ConPTY，所以 PSReadLine 的组合键、中文输入法和全屏程序，表现和不用 wmux 时一样。",
  "c1.p2": "鼠标可以用：点一下切换 pane，拖边框调大小，拖选文字直接复制到 Windows 剪贴板。",
  "cw.title": "在手机上看 pane、往 pane 里输入",
  "cw.sub":
    "跑长编译之前先开 <code>wmux web</code>，然后人就可以走开。它会打出一个二维码，手机连同一个 Wi-Fi 扫一下，浏览器里就列出所有 pane，点进去能看屏幕、能输入。手机上什么都不用装。",
  "cw.p1":
    "屏幕一有变化，wmux 就把新内容推过来，颜色保留，也能往上翻看已经滚过去的输出。列表上带着每个窗口的提醒标记（<b>#</b> 有输出，<b>!</b> 响铃，<b>~</b> 没动静），任务跑完了不用点进去也看得到。",
  "cw.p2":
    "屏幕下方有一排手机键盘上没有的键：Esc、Tab、Shift+Tab、方向键、Ctrl+C，以及 y、n、1、2、3。长一点的内容在输入框里打。<kbd>+</kbd> 菜单可以分屏、开新窗口或关掉当前 pane。",
  "cw.p3": "命令在电脑上执行。手机只能看 pane、往里输入、用那个菜单，别的做不了；加 <code>--read-only</code> 就只能看。",
  "cw.p4":
    "只有 <code>wmux web</code> 在运行时才能连，二维码里的 128 位密钥每次启动都重新生成。用的是普通 HTTP，在家里的网络没问题；在外面用，中间接一层 Tailscale 之类的私有网络。",
  "c1.p3":
    "<kbd>C-b S</kbd> 或 <code>:set sync</code>（Tab 能补全选项名）会把每个按键同时发给窗口里的所有 pane。<code>split-window -N 3</code> 一次加三个 pane 并平铺。",

  "c2.title": "铺满屏幕、从列表里挑，或者弹个菜单",
  "c2.sub":
    "<kbd>C-b z</kbd> 把当前 pane 放大到整个窗口，再按一次还原。<kbd>C-b w</kbd> 弹出 session 和窗口的树：<kbd>j</kbd> <kbd>k</kbd> 上下，<kbd>g</kbd> <kbd>G</kbd> 到头到尾，数字直接跳，<kbd>Enter</kbd> 进去。<kbd>C-b &gt;</kbd> 把 pane 相关的命令放进一个菜单，不用记快捷键。",

  "c2.p1":
    "<kbd>C-b Space</kbd> 在五种布局之间切换：等宽列、等高行、主 pane 在左或在上，还有平铺。<kbd>C-b E</kbd> 把当前 pane 旁边那一排调成一样大。",
  "c2.p2": "<kbd>C-b q</kbd> 在每个 pane 上显示一个数字，按那个数字就跳过去。",
  "c2.p3":
    "copy mode 里 <kbd>/</kbd> 往前搜、<kbd>?</kbd> 往回搜，<kbd>n</kbd> <kbd>N</kbd> 重复上一次搜索，匹配的那一行会滚到屏幕中间。",
  "c2.p4":
    "<code>display-popup</code> 在窗口上开一个框运行程序，程序退出后框就关掉。临时看一眼 <code>git log</code> 时很方便，不用动布局。",

  "c3.title": "关掉终端，程序照常运行",
  "c3.sub": "<kbd>C-b d</kbd> 脱离，里面的程序照常跑。换个终端窗口敲 <code>wmux attach</code> 就接回来了。",
  "c3.p1":
    "重启电脑之后也能恢复。每个 session 的布局一有变化就存到它自己的文件里，<code>wmux resume</code> 会恢复窗口、分屏布局，以及每个 pane 的命令和目录。",
  "c3.p2":
    "文件里还存着每个 pane 输出过的内容：<code>save-history</code> 默认保留最后 500 行，设成 <code>all</code> 就保留全部回滚内容，颜色也在。恢复出来的 pane 显示的是原来的输出，而不是一个空提示符。",
  "c3.p3": "<code>wmux list-saved</code> 列出能恢复的 session，<code>wmux resume work</code> 只恢复这一个。",

  "c35.title": "你没看着的时候，那个任务跑完了",
  "c35.sub":
    "没在看的窗口有动静时会被标出来：<code>monitor-activity</code> 在有输出时标 <b>#</b>，响铃时标 <b>!</b>，太久没有输出时标 <b>~</b>。<kbd>C-b M-n</kbd> 跳到下一个带标记的窗口。",
  "c35.caption":
    "窗口 1 在跑部署，窗口 0 照常干活；跑完之后状态栏和 <code>list-windows</code> 同时出现标记；失败的命令把 pane 和退出码留在原地。",
  "c35.p1":
    "开了 <code>remain-on-exit</code> 之后，程序退出了 pane 也不会关，上面写着 <code>[cmd exited with 3]</code>。凌晨三点崩掉的任务，早上还能看到。<code>respawn-pane</code> 在原位置重新启动它。",
  "c35.p2":
    "<code>pipe-pane \"$input | Add-Content build.log\"</code> 把 pane 输出的所有内容交给一个命令；那个命令处理不过来时，wmux 会提示你。",
  "c35.p3":
    "脚本之间可以互相等待：<code>wmux wait-for ready</code> 会一直等，直到另一个客户端执行 <code>wmux wait-for -S ready</code>；<code>-L</code> 和 <code>-U</code> 可以当锁用。",

  "c4.title": "你的 .tmux.conf，基本能直接用",
  "c4.sub":
    "命令行、: 命令提示符、配置文件里是同一套命令名。tmux 有而 wmux 没有的选项会被接受然后忽略，所以现成配置可以拿来当起点。",
  "c4.p0":
    "命令名可以只写不会混淆的前缀（<code>wmux att</code>、<code>wmux splitw -h</code>），选项名也一样：<code>set sync</code> 就是 <code>set synchronize-panes</code>，<code>set mon-act on</code> 就是 <code>set monitor-activity on</code>。开关类选项不给值就是切换。",
  "c4.p1":
    "<code>wmux send-keys</code>（包括 tmux 的 <code>-X</code> copy mode 命令）、<code>wmux capture-pane -p -e</code> 和 <code>wmux split-window</code> 可以在脚本里操作 session。在 pane 里运行时不用写 socket 名。",
  "c4.p2":
    "插件就是一个目录：放一个写着命令的 <code>&lt;名字&gt;.wmux</code> 文件，再加上任意语言的脚本，用 <code>set -g @plugin 名字</code> 加载。新开窗口、分屏、pane 退出和客户端接入时都会触发钩子。",

  "c6.title": "状态栏上有 git 分支、CPU 和内存",
  "c6.sub":
    "默认的状态栏显示当前 git 分支、pane 所在目录、CPU 和内存占用，有电池的话还有电量。这些都是 wmux 自己每秒读一次，不启动额外进程。整行可以换成自己的写法，也可以关掉。",
  "c6.p1":
    "<code>#{git_branch}</code>、<code>#{cpu_percentage}</code>、<code>#{ram_percentage}</code>、<code>#{battery_percentage}</code>、<code>#{pane_current_path_short}</code> 和 <code>#{pane_pid_command}</code>（pane 里此刻运行的程序）可以放在 <code>status-right</code> 的任意位置；<code>set -g status off</code> 隐藏整行。",
  "c6.p2":
    "两个终端以不同尺寸接入时，由 <code>window-size</code> 决定按哪个终端的尺寸来。较小的终端只看到窗口的一部分，用 <kbd>Shift</kbd> + 方向键平移。",
  "c6.p3":
    "右键粘贴。<kbd>:</kbd> 命令行里 <kbd>Tab</kbd> 可以补全命令、flag、目标和选项名，<code>wmux completion powershell</code> 让 PowerShell 也能这样补全。",
  "c6.p4":
    "<code>wmux update</code> 安装新版本，<code>wmux restart-server</code> 把正在运行的 session 连同历史和目录一起迁过去，已经接入的终端会自动重新连接。",

  "c5.title": "安装",
  "c5.msi": "安装包",
  "c5.msi.sub": "下载 .msi 双击，装到 Program Files，自动进系统 PATH，卸载在“应用和功能”里。",
  "c5.scoop": "Scoop",
  "c5.scoop.sub": "仓库里的清单直接装 zip，以后跟着新版本更新。",
  "c5.zip": "免安装",
  "c5.zip.sub": ".zip 里就是同一个 exe，解压到哪都能跑。",
  "c5.cargo": "从源码装",
  "c5.get": "去下载",

  "close.title": "下次跑长任务的时候试试。",
  "close.docs": "看文档",
  "footer.built": "用 Rust 写的，基于 ConPTY 和 Win32 控制台 API。",
};

const nodes = Array.from(document.querySelectorAll("[data-i18n]"));
const EN = new Map(nodes.map((n) => [n, n.innerHTML]));
// Pictures of a page that has words of its own come in both languages.
const pictures = Array.from(document.querySelectorAll("img[data-src-zh]"));
const EN_SRC = new Map(pictures.map((i) => [i, i.getAttribute("src")]));

function apply(lang) {
  for (const n of nodes) {
    const key = n.dataset.i18n;
    if (lang === "zh" && ZH[key]) n.innerHTML = ZH[key];
    else n.innerHTML = EN.get(n);
  }
  for (const i of pictures) i.setAttribute("src", lang === "zh" ? i.dataset.srcZh : EN_SRC.get(i));
  document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
  document.getElementById("lang").textContent = lang === "zh" ? "EN" : "中文";
  try {
    localStorage.setItem("wmux-lang", lang);
  } catch {
    /* private mode: the toggle still works, it just is not remembered */
  }
}

// Remembered choice wins; otherwise a Chinese browser starts in Chinese.
let lang = "en";
try {
  const saved = localStorage.getItem("wmux-lang");
  if (saved === "zh" || saved === "en") lang = saved;
  else if ((navigator.language || "").toLowerCase().startsWith("zh")) lang = "zh";
} catch {
  /* private mode */
}
apply(lang);

document.getElementById("lang").addEventListener("click", () => {
  lang = lang === "zh" ? "en" : "zh";
  apply(lang);
});
