# CrabHop 🦀

**原生、键盘优先的 Linux 图形文件管理器。**
三列自绘窗口、原生预览栈、无头回放测试架。Rust 单二进制，除系统库外零依赖。

[English](#english) · 中文

---

![CrabHop 演示 1](https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/demo-1.gif)
![CrabHop 演示 2](https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/demo-2.gif)

[完整演示视频（2 分 47 秒）](https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/crabhop-demo.mp4)

## 它是什么

CrabHop 是一个桌面文件管理器：左边是当前目录的父级、中间是当前目录、右边是预览列。
整个窗口由程序自己绘制（egui + wgpu），不依赖 GTK/Qt，所以列宽、行高、图标、配色
都是它自己的，也是一个可以逐像素测试的窗口。

它的设计取向是**键盘优先**：所有操作都有键位，鼠标操作是补充而不是唯一路径。
出厂键位表 94 行，就在仓库根目录的 `keymap-default.toml` 里——那张表是帮助面板和
`which` 提示的唯一事实源，也是你可以整张替换掉的东西。

## 功能

### 窗口与操作

- **三列布局**：父列 / 当前列 / 预览列，列宽可拖、可存
- **键位表完全可覆盖**：写一份 `~/.config/crabhop/keymap.toml`，出厂表整张让位
- **右侧竖条（rail）**：常驻窗口右缘，顶上两个按钮——索引、帮助，点开成侧边栏，再点收起
- **索引侧边栏 `i`**：书签 + 高频访问 + 最近三区，叠在预览列上不挤空间，左缘拖着伸缩；`Del` 删条目（可撤回），`Ctrl+S` 存当前目录
- **帮助侧边栏 `?`**：可搜索、`↑↓` 浏览、回车直接执行该行的动作；和索引同一个槽位，一次只开一个
- **`/` 高亮查找**：列表里命中的字符变色，`n` / `N` 跳下一个 / 上一个
- **标签页**：`t t` 新建、`t q` 关闭，`1`–`9` `0` 切换，`[` `]` 交换位置
- **回收站**：`g t` 开成独立标签页，`Enter` 恢复、`D` 永久删除、`C` 清空
- **右键菜单**（或键盘 `o`）：在终端中打开、打开方式、复制、剪切、粘贴、移动到回收站
- **多选**：`Space` 选入/选出，`j`/`k` 移动，`Ctrl+A` 全选，状态栏显示 `SELECT`
- **撤回**：`Ctrl+Z` 或 `u`，粘贴/删除/重命名/建链接都能回退，栈深 10
- **`Enter` 用默认程序打开**，`l` / `→` 进入目录
- **顶栏按钮簇**：← 上级、→ 进入、最小化、最大化/还原、关闭；底栏一行高频快捷键提示

### 拖拽

- **拖出**：把文件行拖去别的程序（Nautilus、浏览器、微信……）。复制还是移动由接收方决定，
  对方选了移动，原件自动清掉；Esc 取消
- **拖入**：别的程序拖文件进来，落到当前目录——空手是复制，按住 `Shift` 是移动，都能 `u` 撤回
- **窗口内拖**：拖到目录行上松手即移动；悬停目录 1 秒自动进入（弹簧目录）；拖到左侧父列
  悬停 1 秒回到上一级；拖出窗口自动交接成桌面拖拽
- 实现上 winit 0.30 不说 Wayland 拖拽协议，CrabHop 在窗口自己的 `wl_display` 上挂第二条
  事件队列直接讲 `wl_data_device`（`src/dnd.rs`），X11 会话静默降级

### 预览栈

| 类型 | 实现 |
|---|---|
| 图片 | 1:1 Lanczos 降采样（屏幕一像素 = 纹理一像素）+ 三层预加载 + 跨会话磁盘缓存 |
| GIF | 逐帧解码后按帧调度播放 |
| PDF | 静态链接 PDFium，按页缓存 |
| docx | 原生解析段落与图片 |
| xlsx | 表头结构 + 行列数摘要 |
| Markdown | pulldown-cmark 解析 + 自写排版（标题尺寸阶梯、代码块底纹、内嵌本地图片） |
| 代码 | syntect 高亮 + 行号 |
| 压缩包 | 可折叠树（zip / tar / gz / bz2 / xz；7z / rar 显示降级说明） |
| 视频 | 有 ffmpeg 则抽首帧，否则纯 Rust 探针显示编码/分辨率/帧率/时长 |

### 桌面集成

- **`mimeapps.list` 公共账本**：活读 + 合并写，和 Nautilus / Thunar 互认
- **系统剪贴板**：复制文件时同时挂 `text/uri-list` 与 GNOME / KDE 风味，别的文件管理器能粘走
- **UDisks2 挂载面板**（`M`）：zbus 直连，不调外部命令
- **打开方式双区**：推荐区（MIME 匹配）+ 全部应用区，`Ctrl+Enter` 设成默认
- **菜单项自注册**：第一次启动把 `.desktop` 和图标写进 `~/.local/share`（npm 装的用户在安装那一刻就有，见下）

## 安装

### 方式一：npm（最省事）

```sh
npm i -g crabhop
```

装完**立刻**有应用菜单项和图标（postinstall 注册；`sudo` 装进 `/usr/local/share`，
普通装进 `~/.local/share`）。卸载同样干净：

```sh
npm uninstall -g crabhop     # 菜单项、图标一并清掉，不留守尾
```

用了 `--ignore-scripts` 的话，首次运行 `crabhop` 会补注册。目前只提供 linux-x64 二进制。

### 方式二：下载发布包

```sh
wget https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.3/crabhop-1.0.3-linux-x86_64.tar.xz
tar xf crabhop-1.0.3-linux-x86_64.tar.xz
cd crabhop-1.0.3
./install.sh
```

### 方式三：Arch（makepkg）

仓库里有两份 PKGBUILD：

- `packaging/arch/` —— `crabhop`，源码编译（需要 Rust 工具链）
- `packaging/arch-bin/` —— `crabhop-bin`，直接解发布包，秒装

```sh
cd packaging/arch-bin && makepkg -si
```

pacman 记账：装完菜单就有，`pacman -R crabhop-bin` 片甲不留。AUR 包在筹备中。

### 方式四：源码构建

```sh
git clone https://github.com/linxuan-sys/CrabHop
cd CrabHop
scripts/fetch-pdfium.sh      # PDF 预览所需的静态库，一次性
cargo build --release
./target/release/crabhop
```

依赖：Rust 工具链、Vulkan 驱动、Wayland 或 X11。可选 `ffmpeg` / `ffmpegthumbnailer`（视频首帧）。

## 卸载

- npm：`npm uninstall -g crabhop`（自动清理菜单项与图标）
- 发布包：`cd crabhop-1.0.3 && ./uninstall.sh`
- Arch：`pacman -R crabhop-bin`（或 `crabhop`）

配置和数据不会被删——它们是使用者的东西：

- 配置 `~/.config/crabhop/`（`config.toml`、`keymap.toml`、`theme.toml`）
- 状态 `~/.local/share/crabhop/`（书签、频率、历史）
- 缓存 `~/.cache/crabhop/`（预览桶，删了会自动重建）

## 配置

安装脚本会在 `~/.config/crabhop/` 放三份**示例**（已存在则不覆盖）：

- `keymap.toml.example` —— 完整 94 行键位表，抄成 `keymap.toml` 改
- `config.toml.example` —— 带注释的三列比例示例，抄成 `config.toml` 改
- `theme.toml.example` —— 25 个颜色字段的出厂值 + 字体示例，抄成 `theme.toml` 后只留想改的

出厂文件在仓库里，用户文件在 `~/.config/crabhop/`，后者覆盖前者：

| 文件 | 覆盖什么 |
|---|---|
| `config.toml` | 三列比例；缺字段用出厂值 |
| `theme.toml` | 颜色与字体；`[colors]` 段字段名同 `src/tokens.rs`（三列背景可写别名 `left`/`middle`/`right`），值写 `#rrggbb`；`[font] path = "字体文件路径"` 换字体（系统字体文件即可，图标自动回退内置 Maple） |
| `keymap.toml` | 整张键位表；不写则用仓库内的 `keymap-default.toml` |
| `~/.config/mimeapps.list` | 默认打开程序（与桌面其他程序共用同一本账） |

`theme.toml` 坏值、未知字段或不是 TOML，只在 stderr 提一句，然后继续用出厂颜色。**保存即生效**：颜色与字体热重载，无需重启，窗口会弹"主题已重载"。

## 无头测试架

窗口可以在不开窗的情况下回放按键并逐像素检查——这是开发期的主要验收方式：

```sh
CRABHOP_KEYS="key j;key j;shot" CRABHOP_SHOT=/tmp/crabhop cargo run
tools/run-crabhop.sh /some/directory /tmp/crabhop
```

## 许可与致谢

MIT（见 `LICENSE`）。第三方署名见 `NOTICE`：

- 图标表改编自 [yazi](https://github.com/sxyazi/yazi) 的预设（MIT）
- PDF 渲染由 PDFium（BSD-3-Clause）静态提供
- 字体 Maple Mono NF CN
- 图标集本于 nvim-tree（见 `LICENSE-ICONS`）

## 社区欢迎件

配置面刻意做成两层（出厂 + 用户覆盖），下面这些是规格已经写好、等着人接手的方向：

- **主题切换 Mocha / Latte**：`src/tokens.rs` 的令牌第二套 + 切换键
- **字号与行高**：从 `config.toml` 读取，行高 1.5 跟随（字体文件已可从 `theme.toml` 热换）
- **预览缓存上限与预加载开关**：写进 `config.toml`
- **启动默认值**：排序、行模式、隐藏文件、确认策略
- **用户图标覆盖表**：`config.toml` 的 `[icons]` 段
- **剪贴板入方向**（约 3 天）：读外部 `text/uri-list` 粘进窗口（拖入已通，剪贴板粘贴还差这半）
- **X11 拖拽**：XDND 协议，Wayland 侧已完成

---

<a id="english"></a>

# CrabHop (English)

A native, keyboard-first file manager for Linux: three columns drawn by the window itself, a
native preview stack, and a headless harness that replays keys and paints frames without a window.
A single Rust binary; nothing to install beside the system's own libraries.

![CrabHop demo 1](https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/demo-1.gif)
![CrabHop demo 2](https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/demo-2.gif)

[Full demo video (2:47)](https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/crabhop-demo.mp4)

## What it is

Left column is the parent, middle is the current directory, right is the preview column. The whole
window is drawn by the program itself (egui + wgpu) — no GTK, no Qt — so column widths, row
heights, icons and colours are its own, and so is a window that can be tested pixel by pixel.

It is keyboard-first by design: everything has a key, the mouse is an addition rather than the only
road. The factory keymap is 94 rows in `keymap-default.toml` at the repository root — that table is
the single source of truth for the help panel and the `which` hints, and it is the thing you can
replace whole.

## Features

### Window and keys

- **Three columns**: parent / current / preview; widths drag and persist
- **Keymap fully overridable**: write `~/.config/crabhop/keymap.toml` and the factory table steps aside
- **Right-edge rail**: a permanent strip whose two buttons unfold the index and help sidebars
- **Index sidebar `i`**: bookmarks + high frequency + recent, riding over the preview column without
  squeezing it, resize by dragging its left edge; `Del` deletes (undoable), `Ctrl+S` saves the cwd
- **Help sidebar `?`**: searchable, `↑↓` to browse, Enter runs that row's action; shares the sidebar
  slot with the index — one at a time
- **`/` highlight find**: matching characters are coloured in place; `n` / `N` walk the hits
- **Tabs**: `t t` new, `t q` close, `1`–`9` `0` switch, `[` `]` swap
- **Trash**: `g t` opens it as its own tab; `Enter` restores, `D` deletes for good, `C` empties
- **Right-click menu** (or `o`): open in terminal, open with, copy, cut, paste, move to trash
- **Multi-select**: `Space` toggles, `j`/`k` walk, `Ctrl+A` all, the status bar says `SELECT`
- **Undo**: `Ctrl+Z` or `u`; paste, delete, rename and link all come back; the stack is 10 deep
- **`Enter` opens with the default app**; `l` / `→` walks into a directory
- **Title-bar cluster**: back, forward, minimize, maximize/restore, close; a one-line hint strip of
  the most-used keys rides the status bar

### Drag and drop

- **Out**: drag file rows into other programs (Nautilus, browsers, chat apps…). Copy or move is the
  receiver's call; when it moves, the originals are cleaned up. Esc cancels
- **In**: drop files from other programs into the current directory — bare hands copy, `Shift`
  moves; both undo with `u`
- **Inside the window**: drop on a directory row to move; hover a directory for a second to spring
  into it; hover the parent column for a second to go up; dragging out of the window hands over to
  the desktop drag
- winit 0.30 does not speak the Wayland drag protocol, so CrabHop attaches a second event queue to
  the window's own `wl_display` and talks `wl_data_device` directly (`src/dnd.rs`); X11 sessions
  degrade quietly

### Preview

Images are resampled at 1:1 with Lanczos (one screen pixel, one texture pixel), warmed by a
three-layer preloader and cached across sessions. PDF is statically linked PDFium, cached per page.
docx is parsed natively; xlsx shows its header structure with a row/column summary; Markdown is
laid out by hand on top of pulldown-cmark (a heading ladder, code bands, local inline images);
code is highlighted with syntect and numbered; archives open as a collapsible tree (zip/tar/gz/bz2/xz,
with a plain note for 7z/rar); video shows its first frame when ffmpeg is present, and a pure-Rust
header card (codec, resolution, frame rate, duration) when it is not.

### Desktop integration

- **`mimeapps.list` is the shared ledger**: read live, merged on write, the same file Nautilus and
  Thunar use
- **System clipboard**: a file copy also offers `text/uri-list` plus the GNOME/KDE flavours, so other
  file managers can paste what was copied here
- **UDisks2 mount panel** (`M`) through zbus, no external command
- **Open with, two zones**: recommended (MIME matches) and every application, `Ctrl+Enter` to make
  one the default
- **Self-registering menu entry**: the first run writes the `.desktop` file and icons under
  `~/.local/share` (npm installs get them at install time — see below)

## Install

### Via npm (the short road)

```sh
npm i -g crabhop
```

The menu entry and icons appear **immediately** (postinstall; `sudo` installs register under
`/usr/local/share`, ordinary installs under `~/.local/share`). Uninstall is just as clean:

```sh
npm uninstall -g crabhop     # the menu entry and icons go with it
```

With `--ignore-scripts`, the first run of `crabhop` registers them instead. Linux x64 binary only
for now.

### From the release tarball

```sh
wget https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.3/crabhop-1.0.3-linux-x86_64.tar.xz
tar xf crabhop-1.0.3-linux-x86_64.tar.xz
cd crabhop-1.0.3
./install.sh
```

### On Arch (makepkg)

Two PKGBUILDs live in the repository:

- `packaging/arch/` — `crabhop`, built from source (needs a Rust toolchain)
- `packaging/arch-bin/` — `crabhop-bin`, unpacks the release tarball, installs in seconds

```sh
cd packaging/arch-bin && makepkg -si
```

pacman keeps the ledger: the menu entry is there right after install, and `pacman -R crabhop-bin`
leaves nothing behind. AUR packages are in preparation.

### From source

```sh
git clone https://github.com/linxuan-sys/CrabHop
cd CrabHop
scripts/fetch-pdfium.sh      # the static library PDF preview needs, once
cargo build --release
./target/release/crabhop
```

Needs a Rust toolchain, a Vulkan driver, and Wayland or X11. `ffmpeg` / `ffmpegthumbnailer` are
optional (video first frames).

## Uninstall

- npm: `npm uninstall -g crabhop` (menu entry and icons cleaned automatically)
- tarball: `cd crabhop-1.0.3 && ./uninstall.sh`
- Arch: `pacman -R crabhop-bin` (or `crabhop`)

Configuration and state are deliberately left alone: `~/.config/crabhop/` (config, keymap, theme),
`~/.local/share/crabhop/` (bookmarks, frequency, history), `~/.cache/crabhop/` (preview buckets,
rebuilt on demand).

## Configuration

The installer drops three **examples** into `~/.config/crabhop/` (never overwriting anything already
there): `keymap.toml.example`, the full 94-row key table to copy into `keymap.toml`,
`config.toml.example`, a commented column-share file to copy into `config.toml`, and
`theme.toml.example`, all 25 colour fields at their factory values to copy into `theme.toml`.

Factory files live in the repository, the person's in `~/.config/crabhop/`, the latter overriding
the former: `config.toml` (column shares), `theme.toml` (colours and font), `keymap.toml` (the whole
key table), and the desktop's own `mimeapps.list` for default applications.

`theme.toml` takes a `[colors]` table named after the fields in `src/tokens.rs` (the three column
backgrounds also answer to `left` / `middle` / `right`), values `#rrggbb`; write only the fields you
want to change. `[font] path = "…"` swaps the face for any font file on the system — icons fall
back to the built-in Maple. A bad value, an unknown field or a file that is not TOML is reported on
stderr for one line and ignored. **Saving is applying**: colours and font hot-reload without a
restart, and the window toasts "主题已重载".

## The headless harness

The window can replay keys and be inspected pixel by pixel without ever opening one — that is how
this project is accepted:

```sh
CRABHOP_KEYS="key j;key j;shot" CRABHOP_SHOT=/tmp/crabhop cargo run
tools/run-crabhop.sh /some/directory /tmp/crabhop
```

## Licence and credits

MIT (see `LICENSE`). Third-party notices in `NOTICE`: the icon table adapted from
[yazi](https://github.com/sxyazi/yazi)'s preset (MIT), PDF rendering by PDFium (BSD-3-Clause),
the Maple Mono NF CN faces, and the icon set rooted in nvim-tree (`LICENSE-ICONS`).

## Community-welcome items

The configuration surface is deliberately two-layered, and the items below are specifications
already written down, waiting for hands: theme switching (Mocha/Latte), font size and line height
(the font file itself already hot-swaps from `theme.toml`), preview cache caps and a preload switch,
startup defaults (sort, linemode, hidden files, confirm policy), a user icon table, the inbound
half of the clipboard (~3 days — drag-in works, clipboard paste is the missing half), and X11
drag-and-drop (XDND; the Wayland side is done).