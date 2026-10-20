# QuickCopy

替换 Windows 资源管理器的复制、剪切、粘贴，要更快，同时保留系统原有的进度显示、同名冲突询问和撤销。
做完后再接一个屏幕边缘暂存框（见「阶段 2」）。

## 阶段 1：替换复制/移动（先做）

### 必须有
- 在资源管理器或桌面按 Ctrl+C / Ctrl+X 后再按 Ctrl+V，由本工具完成操作。焦点在重命名框、地址栏、搜索框里时原样放行。
- 源文件或目标位置是虚拟位置（手机 MTP、压缩包内部、库、回收站、网络命名空间等，即剪贴板里拿不到真实文件系统路径）时，交回系统处理。
- 进度窗口：显示总进度、当前文件、速度、剩余时间，可以暂停和取消。
- 同名冲突：询问替换、跳过还是两个都保留，可以勾“对剩下的都这样”。**绝不悄悄覆盖。**robocopy 默认会用旧文件覆盖新文件，已实测。
- 撤销：自己记操作日志。在资源管理器里按 Ctrl+Z 时，撤销本工具最近一次操作：复制就删掉拷出来的文件，移动就移回原处。系统自带的撤销认不出我们的操作。
- 同盘移动直接改路径（MoveFileEx 或 rename，瞬间完成）；跨盘移动先复制、核对完整，再删源文件。
- 不往 explorer.exe 注入 DLL，不需要管理员权限，不留后台服务。

### 已知做不到、先不做
- 用鼠标在两个窗口之间拖动文件。接管它要写外壳扩展、注入资源管理器，风险大，仍走系统。

### 可选
- 拷贝的文件夹里有 node_modules、.venv、venv、__pycache__、.git 时，问要不要跳过。实测拷 Onto-Contract 项目到 U 盘，跳过后从 45 s 降到 0.4 s。

## 阶段 2：屏幕边缘暂存框（之后做）

把文件拖到屏幕边缘，弹出暂存框；之后打开目标文件夹，把文件从框里拖出来，完成移动或复制。
现成开源项目 **Regncreative/Wedge** 已经实现了暂存部分（MIT 许可，Tauri：Rust + TypeScript）。但它拖出时由资源管理器完成拷贝，用的是慢的系统引擎。
计划：在 Wedge 的基础上改，松手时识别目标文件夹（光标位置 → ShellWindows 找到对应窗口 → 取它的路径），改用阶段 1 的引擎完成操作。

## 技术方向（已定的倾向，开工时再确认）

- 语言用 Rust。BetterCopy 和 Wedge 都是 Rust，阶段 1 和阶段 2 可以共用一个引擎 crate。
- 参考 **articulite/BetterCopy**（MIT 许可）：
  - 它的触发方式：一个只接收消息的窗口注册全局热键，配合 SetWinEventHook 判断前台是不是 CabinetWClass 窗口或桌面，再从剪贴板的 CF_HDROP 读出文件列表。它用的是 Ctrl+Shift+V，我们要接管 Ctrl+V 本身。
  - 它的引擎：大文件用 CopyFileExW 加 NO_BUFFERING，按盘的类型自动调线程数。
- 剪切和复制的区分：读剪贴板的 `Preferred DropEffect`。
- 界面：进度窗口和冲突对话框要像系统原生的。**已定用 Win32**（crates/qc-ui-win32，2026-10-19 用户拍板）。两版实测：Win32 私有内存约 16 MB、复制满载时按键后约 0.5 s 出窗口；Tauri 连 WebView2 共 7 个进程、约 235 MB，要 1.7–3.3 s，复制速度一样，没有优势。Tauri 对照版已于 2026-10-20 删除，不要加回来。
- 图标用 `assets/app.svg`（一张浅灰纸 + 灰蓝闪电，用户 2026-10-20 选的方案 B），`assets/app.ico` 由它渲染出 16–256 九个尺寸；两个 exe 的 build.rs 用 embed-resource 嵌成资源 1，进度窗口的标题栏图标也 `LoadImageW` 取它。

## 已实现的做法与测试（阶段 1）

- 替换不直接覆盖：旧文件先改名成同目录的隐藏文件 `原名.~qc-…`，新文件到位后才算完成，撤销时改回来。撤销日志（`%LOCALAPPDATA%\QuickCopy\ops.json`）只留最近 20 条，挤出去的那条的备份送回收站。
- 撤销复制是把复制出来的东西送回收站，不是彻底删除。U 盘、网络盘没有回收站（系统也是彻底删除），这时自己逐个删，好报进度。
- Ctrl+Z 要和系统撤销按时间顺序交错（用户 2026-10-19 要求）：常驻进程注册外壳变更通知，我们的操作之后有系统文件操作就先把 Ctrl+Z 交给系统，系统的撤完再撤我们的。测法：先用 e2e.ps1 粘贴一次，再 `bench/e2e.ps1 -Key Z -KeepHook -Pre newfolder`，应看到“交还系统：我们的操作之后还有 1 次系统操作”。
- 进度窗口打开时要切到前台（用户要求），用 AttachThreadInput 拿前台权限，不常驻置顶。
- `qc` 命令行只有加 `--record` 才写撤销日志，测速时别加，否则资源管理器里的 Ctrl+Z 会撤到测速的复制上。
- 引擎测试：`python bench/engine_test.py <E 盘目录> <C 盘目录>`（撤销日志写到隔离目录）。
- 端到端测试（真实资源管理器，会开关窗口）：`pwsh -STA -File bench/e2e_all.ps1`，跑完还原撤销日志。
- 测速：`pwsh -File bench/bench.ps1 -Cases small`，每次只传一个 case（`-File` 传不了数组）。

## 实测基准（2026-10-18，本机 C 盘拷到 E 盘，E 盘是 WD SN5000）

| 工具 | 2 万个小文件，663 MB | Onto-Contract 项目，8738 个文件 | 4 GB 大文件（含落盘） |
|---|---|---|---|
| robocopy /MT:64 | 4.5–5.1 s | **1.9 s** | 1.7 s |
| BetterCopy master 源码编译 | **4.1 s** | 2.6 s | 1.4–3.4 s |
| BetterCopy v0.1.0-alpha 发布版 | 21–29 s | 21–88 s | 1.5–2.6 s |
| LovelyCopy | 约 21 s | 18–28 s | 2–4 s |
| FastCopy 5.12 | 15 s | — | 1.8–3.2 s |
| TeraCopy 4.0.3 | 14–43 s | — | 4.0 s |
| 资源管理器 | 约 60 s | — | 1.6–1.8 s |

2026-10-19 Defender 下重测（bench/bench.ps1，C 盘拷到 E 盘）：小文件 robocopy 4.36–4.59 s / qc 3.50–3.80 s；Onto-Contract robocopy 1.88–2.16 s / qc 1.60–1.67 s；4 GB robocopy 1.72–1.86 s / qc 1.29–1.44 s。三项都达标。

验收线：自研引擎在上面三个场景里都不能比 robocopy /MT:64 慢；资源管理器是要甩开的对象。
测试数据用 `pwsh -File bench/gen.ps1` 生成（2 万个 4–64 KB 随机文件加一个 4 GB 文件，默认放 `.bench\data\bench`，`QC_BENCH_DATA` 可改）；测速时每次拷到新目录，核对文件数和总字节数；大文件拷完要 Flush 落盘再停表，否则测到的只是内存缓存。

## 本机环境的坑

- 杀毒软件：卡巴斯基已于 2026-10-19 卸载，现在是 Windows Defender。上面的基准是在卡巴斯基实时扫描下测的，验收前要在 Defender 下把 robocopy /MT:64 基线重测一遍，以重测结果为准。Defender 的实时扫描同样会拖慢小文件复制；要不要给测试目录加排除（需管理员，由用户操作）到时再定。
- Windows 11：经典注册表方式加的右键菜单项会藏在“显示更多选项”里面。
- **开机自启用启动文件夹快捷方式 `QuickCopy.lnk`（`crates/quickcopy/src/autostart.rs`），不要改回 HKCU `Run`**：本机登录时 Explorer 静默跳过新写入的 Run 项（Shell-Core 日志 9707 里没有它，`StartupApproved` 写成 02 也没用，2026-10-20 三次开机都没起来），快捷方式能正常启动（QuickView 同样做法已重启验证）。`--install [--ui win32]` / `--uninstall` 建删快捷方式；常驻进程启动时把旧的 `Run\QuickCopy` 及其 `StartupApproved\Run` 记录删掉，旧版开过自启且快捷方式不存在时补建，不覆盖用户在“设置 > 启动应用”里的开关。查自启看 `quickcopy.log` 开机后有没有 `started`。
- 构建用 E:\Rust 下的 cargo。临时文件放项目内被 .gitignore 忽略的目录，不放 /tmp。
- 在 E 盘或 E:\_claude_bench 这类盘根目录下删文件夹会被保护检查拦下；遇到就把命令给用户，让用户自己删。
