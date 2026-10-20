# QuickCopy

接管 Windows 资源管理器的 Ctrl+C / Ctrl+X → Ctrl+V，用自己的多线程引擎复制、移动，比系统快；保留进度窗口、同名冲突询问和 Ctrl+Z 撤销。不注入 explorer.exe，不需要管理员权限，不装服务。

## 速度

本机 C 盘拷到 E 盘（WD SN5000，Windows Defender 开着，2026-10-19）：

| 场景 | 资源管理器 | robocopy /MT:64 | QuickCopy |
|---|---|---|---|
| 2 万个小文件，663 MB | 约 60 s | 4.36–4.59 s | **3.50–3.80 s** |
| 一个代码项目，8738 个文件 | — | 1.88–2.16 s | **1.60–1.67 s** |
| 4 GB 单个大文件（含落盘） | 1.6–1.8 s | 1.72–1.86 s | **1.29–1.44 s** |

## 什么时候接管

在资源管理器或桌面的文件列表里按 Ctrl+V，并且同时满足：

- 剪贴板里是真实的文件路径（从资源管理器复制或剪切的文件、文件夹）；
- 源文件存在，不在回收站里；
- 目标是磁盘上的真实文件夹（包括桌面）。

不满足就把按键原样交还系统，比如手机（MTP）、压缩包内部、库、网络位置这类虚拟位置。焦点在重命名框、地址栏、搜索框里时不拦截。是否接管和文件大小无关。

用鼠标拖动文件仍由系统处理。

## 功能

- 进度窗口：总进度、当前文件、速度曲线、剩余时间，可暂停、取消。
- 同名冲突：替换、跳过或两个都保留，可勾“对剩下的都这样”。不会悄悄覆盖：被替换的旧文件先改名藏起来，撤销时还原。
- 同盘移动直接改路径；跨盘移动先复制、核对，再删源文件。
- Ctrl+Z 撤销：复制的撤销是把拷出来的文件送回收站，移动的撤销是移回原处。和系统自己的撤销按时间顺序交替进行。

## 构建与安装

需要 Rust（stable，MSVC 工具链）。

```bash
cargo build --release
```

产物在 `target\release`：`quickcopy.exe`（常驻，拦截按键）、`qc-ui-win32.exe`（进度窗口）、`qc.exe`（命令行，测速调试用）。三个文件放在同一目录。

```bash
target/release/quickcopy.exe --install
```

在“启动”文件夹建快捷方式 `QuickCopy.lnk`，开机自动运行。直接双击 `quickcopy.exe` 也能启动，重复启动只会运行一个。

| 命令 | 作用 |
|---|---|
| `quickcopy.exe --install` | 开机启动 |
| `quickcopy.exe --uninstall` | 取消开机启动 |
| `quickcopy.exe --quit` | 退出正在运行的常驻进程 |

日志和撤销记录在 `%LOCALAPPDATA%\QuickCopy`。

## 命令行

```
qc [--threads N] [--on-conflict skip|replace|keep] [--move] [--record] <源>... <目标文件夹>
qc --undo
```

只有加 `--record` 才写撤销记录。

## 测试

```bash
pwsh -File bench/gen.ps1
```

生成测试数据：2 万个 4–64 KB 随机文件和一个 4 GB 文件，默认放在 `.bench\data\bench`，设环境变量 `QC_BENCH_DATA` 可换位置。

| 脚本 | 用途 |
|---|---|
| `python bench/engine_test.py <目录1> <目录2>` | 引擎测试：复制、移动、冲突、撤销 |
| `pwsh -STA -File bench/e2e_all.ps1` | 端到端测试，会真的打开资源管理器窗口按键 |
| `pwsh -File bench/bench.ps1 -Cases small` | 和 robocopy 对比测速，case 有 small / onto / big |

## 许可

MIT
