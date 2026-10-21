# sysmon

一个极轻量的 Windows 桌面挂件：无边框、置顶的最小悬浮窗，每秒实时显示 CPU 与 GPU 占用百分比。

## 特性

- **零第三方框架**：直接调用原生 Win32 API，唯一依赖是微软官方的 `windows` crate。
- **几乎不占资源**：进程优先级设为低于普通（`BELOW_NORMAL_PRIORITY_CLASS`）；闲时的 `GetMessageW` 会让线程休眠；显示数值没有变化时不重绘窗口。
- **CPU 与任务管理器同口径**：优先读取 `% Processor Utility`（Windows 10+ 任务管理器显示的就是它，CPU 睿频时会高于「繁忙时间占比」）；计数器不可用时回退到 `GetSystemTimes` 差分。
- **GPU 数据与任务管理器同源**：读取性能计数器 `\GPU Engine(*)\Utilization Percentage`，对同一物理引擎上各进程的占用求和、对同一显卡取各引擎最大值，聚合方式与任务管理器一致。
- **多显卡支持**：自动识别显卡数量，显示为 `GPU0` / `GPU1`…；无可用数据时显示 `GPU N/A`。虚拟/间接显示驱动（远程桌面、远控软件、安卓模拟器等）会被自动排除，不会出现恒为 0% 的假显卡。
- **负载分级配色**：数字与标签胶囊按占用率变色——0~25% 绿、25~50% 蓝、50~75% 黄、75~100% 红；无数据时显示中性灰。
- **单实例运行**：再打开程序时新实例会关闭旧实例并接管，不会多开。
- **置顶保持**：窗口置顶显示，且每分钟重申一次置顶，避免被其他窗口压下去。
- **高占用高亮**：占用达到 95% 及以上时，该格子的数字与标签胶囊自动转为红色。
- **双缓冲绘制**：分层窗口（`WS_EX_LAYERED` + `UpdateLayeredWindow`）+ GDI+ 抗锯齿，圆角与文字逐像素平滑，无边缘毛刺。
- **进程名规范**：exe 与任务管理器中显示的应用名统一为 `AAsysmon`（版本信息 `FileDescription` 由 `build.rs` 写入）。

## 运行环境

| 项目 | 要求 |
| --- | --- |
| 操作系统 | Windows 10 1709 及以上 |
| 显卡驱动 | WDDM 2.x 及以上（低于此版本时 GPU 数据不可用，显示 `N/A`，CPU 统计仍正常） |
| 工具链 | Rust（edition 2021），Windows MSVC target |

## 构建与运行

```bash
# 调试构建
cargo build

# 发布构建（推荐，LTO + strip，体积最小）
cargo build --release

# 直接运行
cargo run --release
```

发布构建的可执行文件位于 `target\release\AAsysmon.exe`，程序图标由 `build.rs` 在构建期通过 `winresource` 嵌入（`assets/icon.ico`），双击即可运行。

程序没有控制台窗口，也没有托盘图标：**关闭方式是在悬浮窗上点击右键，选择「退出」**。

## 使用说明

| 操作 | 效果 |
| --- | --- |
| 按住左键拖动 | 移动悬浮窗（无边框窗口的拖动方式） |
| 右键单击 | 弹出菜单，可选择「退出」 |

窗口默认出现在屏幕右上角（`SM_CXSCREEN - 320, 20`），始终置顶、不在任务栏显示、不抢占焦点。

## 项目结构

```
src/
├── main.rs   # Win32 分层窗口、消息循环、布局与状态管理
├── render.rs # GDI+ 抗锯齿渲染（分层表面、圆角矩形、居中文字）
├── cpu.rs    # CPU 占用统计（GetSystemTimes 差分）
└── gpu.rs    # GPU 占用统计（PDH 性能计数器 + 实例聚合 + 虚拟显卡过滤）
build.rs      # 构建期把 assets/icon.ico 与版本信息嵌入 exe（winresource 调用 rc.exe）
assets/
├── icon.ico  # 程序图标（16~256 像素多尺寸）
└── make_icon.py  # 图标生成脚本（python assets/make_icon.py 重新生成）
```

整份代码仅三个源文件。`main.rs` 顶部的常量集中了全部外观参数：刷新间隔、格子宽度、圆角半径、配色等，修改那里即可调整视觉效果，不必改动绘制逻辑。

注意 `src/main.rs` 首行的 `#![windows_subsystem = "windows"]` 使程序以 GUI 子系统运行，`println!` / `dbg!` 不会产生任何可见输出；调试时请改用 `MessageBoxW` 或临时注释掉该属性。

## 已知限制

- 仅支持 Windows 平台。
- GPU 实例的解析依赖 Windows 的 PDH 命名格式（`pid_..._luid_<luid>_phys_<...>_engtype_<...>`）；若微软变更该格式，`src/gpu.rs` 中的实例名切片逻辑需要同步调整。
- 依赖版本在 `Cargo.toml` 中被刻意固定（`windows` 0.58），因为该 crate 各版本间 API 变动较大，升级后可能需要修改调用代码。
