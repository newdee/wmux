// Language toggle for the wmux tour. Every translatable node carries
// data-i18n; the English text lives in the HTML, the Chinese here.
const ZH = {
  "nav.tour": "功能",
  "nav.download": "下载",
  "hero.eyebrow": "开源 · 原生 Windows",
  "hero.title": "Windows 上的 tmux。",
  "hero.lede":
    "把一个终端切成几块，用 h j k l 在它们之间走；关掉窗口，回来的时候东西都还在跑。PowerShell、cmd、WSL 都能在 pane 里正常用，按键一个不丢。",
  "hero.download": "下载 Windows 版",
  "hero.source": "看源码",
  "hero.meta": "MIT 许可 · Windows 10 1809 及以上 · 一个 3 MB 的 exe",
  "hero.caption": "一段完整录制：分屏、移动、全屏、弹出 pane 菜单、切窗口、脱离，再接回来。",
  "stat.exe": "个文件就够",
  "stat.deps": "依赖 Cygwin / WSL",
  "stat.tests": "项自动化测试",
  "stat.license": "许可证",

  "c1.title": "分屏还是那套手感",
  "c1.sub":
    "<kbd>C-b %</kbd> 左右分，<kbd>C-b \"</kbd> 上下分。用 vim 键位或方向键移动，而且可以连着按：移动和调大小在半秒内不用再按前缀。",
  "c1.p1":
    "<b>每块都是真终端。</b> 每个 pane 就是一个 ConPTY，所以 PSReadLine 的组合键、中文输入法、全屏程序，表现和不用 wmux 时一样。",
  "c1.p2":
    "<b>鼠标也管用。</b> 点一下选中 pane，拖边框调大小，拖选一段文字松手就进了 Windows 剪贴板。",
  "c1.p3": "<b>一次敲进所有 pane。</b> <kbd>C-b S</kbd> 打开 synchronize-panes，这个窗口里的每块都收到同样的输入。",

  "c2.title": "铺满屏幕、从列表里挑，或者弹个菜单",
  "c2.sub":
    "<kbd>C-b z</kbd> 把当前 pane 放大到整个窗口，再按一次还原。<kbd>C-b w</kbd> 弹出 session 和窗口的树：<kbd>j</kbd> <kbd>k</kbd> 上下，<kbd>g</kbd> <kbd>G</kbd> 到头到尾，数字直接跳，<kbd>Enter</kbd> 进去。<kbd>C-b &gt;</kbd> 把这个 pane 能干的事做成菜单，什么都不用记。",

  "c2.p1":
    "<b>一个键重排。</b> <kbd>C-b Space</kbd> 轮换五种布局：等宽列、等高行、主窗在左或在上，还有平铺网格。<kbd>C-b E</kbd> 把旁边这一排拉成一样大。",
  "c2.p2": "<b>找得到是哪一块。</b> <kbd>C-b q</kbd> 在每个 pane 上写一个大号数字，按下去就跳过去。",
  "c2.p3":
    "<b>翻回滚能搜。</b> copy mode 里 <kbd>/</kbd> 往新的方向搜、<kbd>?</kbd> 往回翻历史搜，<kbd>n</kbd> <kbd>N</kbd> 找下一个，命中的那行会落到屏幕中间。",
  "c2.p4":
    "<b>在窗口上开个小框。</b> <code>display-popup</code> 在框里跑一个程序，跑完就收走——想看眼 <code>git log</code> 又不想动布局的时候正好。",

  "c3.title": "关掉终端，什么都不会死",
  "c3.sub": "<kbd>C-b d</kbd> 脱离，里面的程序照常跑。换个终端窗口敲 <code>wmux attach</code> 就接回来了。",
  "c3.p1":
    "<b>重启也一样。</b> 每个 session 的形状一变就存一次盘，所以 <code>wmux resume</code> 能把窗口、分屏布局、每块跑的命令和所在目录都带回来。",
  "c3.p2":
    "<b>连打印过的东西一起。</b> <code>save-history</code>（默认 500 行）也进那个文件，所以恢复出来的 pane 屏幕上是原来的输出，不是一个空提示符。",
  "c3.p3":
    "<b>按 session 来。</b> <code>wmux list-saved</code> 看有哪些能恢复，<code>wmux resume work</code> 只恢复其中一个。",

  "c35.title": "你没看着的时候，那个任务跑完了",
  "c35.sub":
    "没在看的窗口会自己说话：<code>monitor-activity</code> 有输出就标 <b>#</b>，响铃标 <b>!</b>，太久没动静标 <b>~</b>。<kbd>C-b M-n</kbd> 直接跳到下一个有话要说的窗口。",
  "c35.caption":
    "窗口 1 在跑部署，窗口 0 照常干活；跑完之后状态栏和 <code>list-windows</code> 同时出现标记；失败的命令把 pane 和退出码留在原地。",
  "c35.p1":
    "<b>死掉的任务会留下现场。</b> 开了 <code>remain-on-exit</code>，程序退出后 pane 还在，写着 <code>[cmd exited with 3]</code>——凌晨三点崩的，早上还看得到。<code>respawn-pane</code> 原地再起一次。",
  "c35.p2":
    "<b>完整日志也能留。</b> <code>pipe-pane \"$input | Add-Content build.log\"</code> 把 pane 打印的一切灌进一个命令；那个命令要是跟不上，wmux 会告诉你，不会闷声丢。",
  "c35.p3":
    "<b>脚本之间可以互相等。</b> <code>wmux wait-for ready</code> 挂在那儿，直到另一个进程 <code>wmux wait-for -S ready</code> 放行；<code>-L</code> / <code>-U</code> 是一把锁。",

  "c4.title": "你的 .tmux.conf，基本能直接用",
  "c4.sub":
    "命令行、: 命令提示符、配置文件里是同一套命令名。tmux 有而 wmux 没有的选项会被接受然后忽略，所以现成配置可以拿来当起点。",
  "c4.p1":
    "<b>能被脚本驱动。</b> <code>wmux send-keys</code>（含 <code>-X</code> 那套 copy mode 命令）、<code>wmux capture-pane -p -e</code>、<code>wmux split-window</code> 可以从外面操作一个 session；在 pane 里面用则不用再写 socket 名。",
  "c4.p2":
    "<b>插件。</b> 一个目录，放一个写着命令的 <code>&lt;名字&gt;.wmux</code>，再加任意语言的脚本，用 <code>set -g @plugin 名字</code> 加载。新窗口、分屏、pane 退出、客户端接入都有钩子。",

  "c5.title": "安装",
  "c5.msi": "安装包",
  "c5.msi.sub": "下载 .msi 双击，装到 Program Files，自动进系统 PATH，卸载在“应用和功能”里。",
  "c5.zip": "免安装",
  "c5.zip.sub": ".zip 里就是同一个 exe，解压到哪都能跑。",
  "c5.cargo": "从源码装",
  "c5.get": "去下载",

  "close.title": "切开它，走开，再回来。",
  "close.docs": "看文档",
  "footer.built": "用 Rust 写的，基于 ConPTY 和 Win32 控制台 API。",
};

const nodes = Array.from(document.querySelectorAll("[data-i18n]"));
const EN = new Map(nodes.map((n) => [n, n.innerHTML]));

function apply(lang) {
  for (const n of nodes) {
    const key = n.dataset.i18n;
    if (lang === "zh" && ZH[key]) n.innerHTML = ZH[key];
    else n.innerHTML = EN.get(n);
  }
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
