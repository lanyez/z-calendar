# 任务栏日历（Rust 版）

替换 Windows 任务栏"点击时间弹出日历"的日历应用，Rust 原生实现（GDI+ 自绘渲染，无 GPU 框架），编译为**单个约 900KB 的独立 exe**，无任何运行时依赖，**常驻内存 ~1MB（显示时 ~9MB，私有内存 4MB）**。

![预览](shots/view-1-cal-sep.png)

## 功能

- **接管任务栏时钟**：启动后自动把透明点击层覆盖在任务栏时钟上，点击弹出本日历（原生日历不再弹出）；右键时钟弹出菜单。
- **纯后台运行**：日历弹窗带 `WS_EX_TOOLWINDOW`，任务栏和 Alt-Tab 中不出现程序；常驻入口只有托盘图标。
- **法定节假日自动对接**：数据来自 [chinese-days](https://github.com/yaavi/chinese-days)（`cdn.jsdelivr.net/npm/chinese-days/dist/holidays.ics`），法定节假日"休"（蓝角标）与调休补班"班"（红角标）。启动及每 30 分钟检查、每 6 小时更新一次，缓存到本地；数据源每年发布新年份数据后自动跟进（当前覆盖 2024~2026）。
- **农历 / 节气 / 节日**：内置 1900~2100 农历与二十四节气数据表（由 lunar-javascript 数据一次性生成，见 `rust/gen-lunar.js`），传统节日（春节/除夕红色）、公历节日、纪念日（烈士纪念日等红色）本地计算，离线可用。
- **周数**（左侧 ISO 周数 + 顶部"第 N 周"）、**今日蓝色圆形高亮**、非本月置灰。
- **天气**：ipwho.is 定位 + Open-Meteo 当前温度（30 分钟刷新，失败自动隐藏）。
- **日程**：按选中日期增删日程（本地存储），有日程的日期底部显示蓝点。
- **设置**：开机自启（写注册表 Run）、显示天气、自动更新开关、手动更新。
- **托盘**常驻（显示日历 / 开机自启 / 立即更新 / 退出），单实例运行。

## 使用

- 启动：双击 `启动日历.vbs`（或直接运行 `Z日历.exe`，纯 GUI 程序无控制台）。
- 点击任务栏右下角时间 → 弹出日历；再点一次时钟 / Esc / 点击日历外任意位置 → 关闭。
- 应用已在运行时再次双击 exe → 已运行实例的日历自动弹出（单实例，无重复进程）。
- 任务管理器/托盘中显示名称为「Z日历」。
- 月份标题点击回到今天；‹ › 切换月份；底部：日程 / 今天 / ＋ / 设置 / 退出。

## 构建

```cmd
cd rust
build.cmd          REM 将 E:\tools\mingw64\bin 加入 PATH 后执行 cargo build --release
```

- 工具链：stable-x86_64-pc-windows-gnu（rustup 默认）+ [MinGW-w64](https://winlibs.com)（gcc 16.2，位于 `E:\tools\mingw64`，构建 `ring` 等需要）。
- 产物：`rust/target/release/CalendarFlyout.exe`（约 7MB，静态链接 CRT/pthread，可直接拷贝运行）。
- `cargo test` 内置农历转换/节气/节日单元测试。

## 目录结构

```
Z日历.exe             编译好的可执行文件（交付物，构建产物 CalendarFlyout.exe 复制改名）
启动日历.vbs           无黑窗启动脚本
rust/                 源码（Cargo 项目）
  src/main.rs         装配：单实例事件、ICS/天气后台更新线程、消息循环
  src/flyout.rs       日历弹窗（Win32 窗口 + GDI+ 绘制三页 + IME 输入）
  src/gdi.rs          GDI+ 平面 API 封装与绘制助手
  src/overlay.rs      任务栏时钟接管（Win32 透明覆盖层线程）
  src/tray.rs         托盘图标与菜单
  src/ics.rs          holidays.ics 下载解析（休/班/假期区间）
  src/lunar.rs        农历/节气/节日算法（含单元测试）
  src/lunar_data.rs   生成的 1900~2100 数据表
  gen-lunar.js        数据表生成器（数据源 lunar-javascript）
shots/                界面截图（含设计验证记录）
tools/                开发辅助脚本
```

## 数据与配置

- 配置 / 假期缓存 / 天气缓存 / 日程：`%APPDATA%\CalendarFlyout\`

## 实现要点

- 时钟定位：`Shell_TrayWnd → TrayNotifyWnd → TrayClockWClass`（Win10），每秒校正；全屏应用或任务栏隐藏时自动隐藏覆盖层，explorer 重启后 1 秒内恢复。
- 弹窗常驻渲染：窗口"隐藏"时停放在屏幕外 (32000,32000)（winit 对隐藏窗口不派发重绘，事件循环会停摆，离屏方案绕开此限制）。
- 渲染：GDI+ 双缓冲 + UpdateLayeredWindow 分层窗口（无 GPU 框架，锁屏/远程会话均可运行）。
- 调试：`CAL_DUMP=x.bmp` 将首帧保存为 BMP；`CAL_PAGE=agenda|settings`、`CAL_YM=2026-10` 指定起始页面/月份。
- 点击外部关闭：轮询 `GetAsyncKeyState` + 光标位置（不依赖焦点，`SetForegroundWindow` 被拒绝时也能正常关闭）。
- 配色取自设计图：背景 `#202838`、强调蓝 `#3E87FA`、周末红 `#E54B4B`。
- 调试钩子：环境变量 `CAL_PAGE=agenda|settings`、`CAL_YM=2026-10` 可直接以指定页面/月份启动。
