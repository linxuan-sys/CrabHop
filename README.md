# CrabHop 🦀

**原生、键盘优先的 Linux 图形文件管理器。**
三列自绘窗口、原生预览栈、无头回放测试架。Rust 单二进制，除系统库外零依赖。

[English](#english) · 中文

---

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
- **`?` 交互式帮助**：可搜索、`↑↓` 浏览、回车直接执行该行的动作
- **`/` 高亮查找**：列表里命中的字符变色，`n` / `N` 跳下一个 / 上一个
- **索引面板 `i`**：书签 + 高频访问 + 最近三区，`Del` 删条目（可撤回），`Ctrl+S` 存当前目录
- **标签页**：`t t` 新建、`t q` 关闭，`1`–`9` `0` 切换，`[` `]` 交换位置
- **回收站**：`g t` 开成独立标签页，`Enter` 恢复、`D` 永久删除、`C` 清空
- **右键菜单**（或键盘 `o`）：在终端中打开、打开方式、复制、剪切、粘贴
- **多选**：`Space` 选入/选出，`j`/`k` 移动，`Ctrl+A` 全选，状态栏显示 `SELECT`
- **撤回**：`Ctrl+Z` 或 `u`，粘贴/删除/重命名/建链接都能回退，栈深 10
- **`Enter` 用默认程序打开**，`l` / `→` 进入目录

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

## 安装

### 方式一：下载发布包（推荐）

把下面四行**整段复制进终端**，一次跑完：

```sh
wget https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/crabhop-1.0.0-linux-x86_64.tar.xz
tar xf crabhop-1.0.0-linux-x86_64.tar.xz
cd crabhop-1.0.0
./install.sh
```

四行各自的作用：

1. `wget ...` —— 从 Releases 下载发布包（约 25M）到当前目录
2. `tar xf ...` —— 解压，只会多出一个 `crabhop-1.0.0/` 目录
3. `cd crabhop-1.0.0` —— 进入这个目录
4. `./install.sh` —— 安装：把程序、菜单项、三档图标、许可文件装进 `~/.local`（不需要 root），
   刷新桌面缓存，然后**把下载的压缩包移到回收站**——装完不留垃圾

装完按 Super 键搜 `CrabHop` 即可，或终端直接运行 `crabhop`。

### 方式二：源码构建

```sh
git clone https://github.com/linxuan-sys/CrabHop
cd CrabHop
scripts/fetch-pdfium.sh      # PDF 预览所需的静态库，一次性
cargo build --release
./target/release/crabhop
```

依赖：Rust 工具链、Vulkan 驱动、Wayland 或 X11。可选 `ffmpeg` / `ffmpegthumbnailer`（视频首帧）。

## 卸载

```sh
cd crabhop-1.0.0 && ./uninstall.sh     # 撤掉 ~/.local 里的程序、菜单项、图标
```

配置和数据不会被删——它们是使用者的东西：

- 配置 `~/.config/crabhop/`（`config.toml`、`keymap.toml`）
- 状态 `~/.local/share/crabhop/`（书签、频率、历史）
- 缓存 `~/.cache/crabhop/`（预览桶，删了会自动重建）

## 配置

出厂文件在仓库里，用户文件在 `~/.config/crabhop/`，后者覆盖前者：

| 文件 | 覆盖什么 |
|---|---|
| `config.toml` | 三列比例；缺字段用出厂值 |
| `keymap.toml` | 整张键位表；不写则用仓库内的 `keymap-default.toml` |
| `~/.config/mimeapps.list` | 默认打开程序（与桌面其他程序共用同一本账） |

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
- **字体家族与字号**：从 `config.toml` 读取，行高 1.5 跟随
- **预览缓存上限与预加载开关**：写进 `config.toml`
- **启动默认值**：排序、行模式、隐藏文件、确认策略
- **用户图标覆盖表**：`config.toml` 的 `[icons]` 段
- **拖拽双向**（约 1 天 + 3–5 天）：桌面协议是主要工作量
- **剪贴板入方向**（约 3 天）：读外部 `text/uri-list` 粘进窗口

---

<a id="english"></a>

# CrabHop (English)

A native, keyboard-first file manager for Linux: three columns drawn by the window itself, a
native preview stack, and a headless harness that replays keys and paints frames without a window.
A single Rust binary; nothing to install beside the system's own libraries.

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
- **`?` interactive help**: searchable, `↑↓` to browse, Enter runs that row's action
- **`/` highlight find**: matching characters are coloured in place; `n` / `N` walk the hits
- **Index panel `i`**: bookmarks + high frequency + recent, `Del` deletes (undoable), `Ctrl+S` saves the cwd
- **Tabs**: `t t` new, `t q` close, `1`–`9` `0` switch, `[` `]` swap
- **Trash**: `g t` opens it as its own tab; `Enter` restores, `D` deletes for good, `C` empties
- **Right-click menu** (or `o`): open in terminal, open with, copy, cut, paste
- **Multi-select**: `Space` toggles, `j`/`k` walk, `Ctrl+A` all, the status bar says `SELECT`
- **Undo**: `Ctrl+Z` or `u`; paste, delete, rename and link all come back; the stack is 10 deep
- **`Enter` opens with the default app**; `l` / `→` walks into a directory

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

## Install

### From the release tarball (recommended)

Copy all four lines into a terminal and let them run:

```sh
wget https://github.com/linxuan-sys/CrabHop/releases/download/v1.0.0/crabhop-1.0.0-linux-x86_64.tar.xz
tar xf crabhop-1.0.0-linux-x86_64.tar.xz
cd crabhop-1.0.0
./install.sh
```

What each line does: `wget` downloads the release (about 25M) into the current directory; `tar xf`
unpacks it into one `crabhop-1.0.0/` directory; `cd` steps into that directory; `./install.sh` puts
the binary, the menu entry, three icon sizes and the licences under `~/.local` (no root), refreshes
the desktop caches, and moves the downloaded tarball to the trash — nothing is left to clean up.
Search `CrabHop` in your application menu, or run `crabhop`.

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

```sh
cd crabhop-1.0.0 && ./uninstall.sh
```

Configuration and state are deliberately left alone: `~/.config/crabhop/` (config, keymap),
`~/.local/share/crabhop/` (bookmarks, frequency, history), `~/.cache/crabhop/` (preview buckets,
rebuilt on demand).

## Configuration

Factory files live in the repository, the person's in `~/.config/crabhop/`, the latter overriding
the former: `config.toml` (column shares), `keymap.toml` (the whole key table), and the desktop's
own `mimeapps.list` for default applications.

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
already written down, waiting for hands: theme switching (Mocha/Latte), font family and size,
preview cache caps and a preload switch, startup defaults (sort, linemode, hidden files, confirm
policy), a user icon table, drag and drop in both directions (~1 day + 3–5 days), and the inbound
half of the clipboard (~3 days).
