# CrabHop

[English](README.md) | 中文

原生、键盘优先的文件管理器：窗口自持状态、自绘列表——三列布局、预览栈（图片、PDF、
办公文档、压缩包、媒体）、桌面原生的"打开方式"清单，以及一套无头 harness：不开窗就能
回放按键、绘制帧。本仓自一棵早期树清洁提取而来；设计笔记住在源码注释里。

## 构建

	cargo build --release

PDF 预览静态链接 PDFium；用 `scripts/fetch-pdfium.sh` 取一次（落在 `libs/pdfium`，
已 gitignore）。

## 测试

	cargo test

## 运行

	cargo run --release

窗口在当前目录打开。出厂键位表是身旁的 `keymap-default.toml`——那张表就是键位唯一的
事实源（帮助面板与 which 提示都读它）。回放与截图走无头，不弹窗：

	CRABHOP_KEYS="key j;key j;shot" CRABHOP_SHOT=/tmp/crabhop cargo run
	tools/run-crabhop.sh /某个目录 /tmp/crabhop   # 同上，任何位置可跑

## 社区欢迎件

配置面刻意做成两层——仓内出厂文件，被 `~/.config/crabhop/config.toml` 与
`~/.config/crabhop/keymap.toml` 覆盖——下面这份清单是本项目交给社区的：规格早已写好的
扩展（早期树的 `docs/design/native-ui.md` 是档案库；§ 号是它自己的节号），每一件都落在
同一套两层结构里。

- **主题切换——Mocha / Latte**：暗色盘是 `src/tokens.rs` 的 `Tokens::MOCHA`；第二批令牌
  加一枚切换键（暗 / 亮 / 跟随系统）就是全部工作量——规格 §9（亮色差异表）与 §7.2（外观页
  三态控件）。
- **字体家族与字号**：`src/tokens.rs` 目前按名装入内置 Maple 两枚字面；家族与 14–20px
  字号（行高按 1.5 跟随）想从 `config.toml` 来——规格 §7.2 的排版行。
- **预览缓存上限与预加载开关**：`src/preview.rs` 按 path+mtime 在窗口缓存目录缓存文本，
  并在窗口空闲时预热邻近图片；两者的上限与开关想进 `config.toml`——规格 §3（预览规则）。
- **启动默认值——排序、行模式、隐藏文件、确认策略**：窗口目前以自己的选择启动；
  `config.toml` 该带上它们，让一个人的第一帧就是他的——规格 §7.1（通用页——隐藏文件与
  确认策略本是它的行；排序与行模式想在旁边加行）与 §1.1（两档密度）。
- **用户图标表**：`src/icons/table.rs` 是出厂表（预设表，画在 §1 的图标盒）；`config.toml`
  里一张 `[icons]` 表就能让使用者加行或覆盖行——规格 §1，以及 `NOTICE`（出厂表的出处）。

另有两件早期树已泊给社区、在此保留以免丢失：

- **拖拽，双向**：接收拖入窗口的 drop（约 1 天）与从窗口发起拖出（约 3–5 天）——桌面协议
  是工作量所在；列表已为它备好。
- **剪贴板的入方向半边**：把外部 `text/uri-list` 读进窗口（约 3 天）——与
  `src/uri_clipboard.rs` 已经提供给文件管理器的出方向互为反面。

## 许可证

MIT——见 `LICENSE`。第三方署名：`NOTICE`（yazi 预设图标表、PDFium）、
`LICENSE-ICONS`（nvim-tree，图标表所本的图标集）、
`assets/fonts/LICENSE-MapleMono.txt`（内置 Maple Mono 字面）。
