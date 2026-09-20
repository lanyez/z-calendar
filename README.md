# Z日历（任务栏日历，Rust 版）

替换 Windows 任务栏"点击时间弹出日历"的日历应用，Rust 原生实现（GDI+ 自绘渲染，无 GPU 框架），编译为**单个约 1.9MB 的独立 exe**，无任何运行时依赖，**常驻内存约 6MB（工作集，私有内存约 12MB）**，可直接拷贝到任意目录运行。

## 功能

- **接管任务栏时钟**：启动后自动把透明点击层覆盖在任务栏时钟上，点击弹出本日历（原生日历不再弹出）；右键时钟弹出菜单。
- **纯后台运行**：日历弹窗带 `WS_EX_TOOLWINDOW`，任务栏和 Alt-Tab 中不出现程序；常驻入口只有托盘图标。
- **法定节假日自动对接**：数据来自 [chinese-days](https://github.com/yaavi/chinese-days)（`cdn.jsdelivr.net/npm/chinese-days/dist/holidays.ics`），法定节假日"休"（蓝角标）与调休补班"班"（红角标）。启动及每 30 分钟检查一次，距上次成功更新超过 12 小时自动重新拉取（约每天一次），缓存到本地；数据源每年发布新年份数据后自动跟进（当前覆盖 2024~2026）。
- **农历 / 节气 / 节日**：内置 1900~2100 农历与二十四节气数据表（由 lunar-javascript 数据一次性生成，见 `rust/gen-lunar.js`），传统节日（春节/除夕红色）、公历节日、纪念日（烈士纪念日等红色）本地计算，离线可用。
- **周数**：左侧 ISO 周数列（可在设置中关闭）、**今日蓝色圆形高亮**、非本月日期置灰（可选择隐藏）。
- **天气**：ipwho.is 定位 + Open-Meteo 当前温度与天气现象（30 分钟刷新，失败 1 分钟后重试，失败自动隐藏；本地缓存 3 小时）。
- **日程**：按选中日期增删日程（本地存储），支持中文 IME 与剪贴板粘贴输入，有日程的日期底部显示蓝点，条目过多时折叠显示。
- **设置窗口**：开机自启、自动更新假期数据、显示系统托盘图标、显示天气预报、使用 12 小时制、显示周数、显示农历/节日信息、显示调休安排、显示非当前月日期、一周开始（周一~周日），更改立即生效无需保存。
- **托盘**常驻（显示日历 / 开机自启 / 立即更新节假日数据 / 退出），单实例运行。

## 使用

- 启动：运行 `rust/target/release/CalendarFlyout.exe`（纯 GUI 程序无控制台），可自行复制改名（如 `Z日历.exe`）到任意位置。
- 点击任务栏右下角时间 → 弹出日历；再点一次时钟 / Esc / 点击日历外任意位置 → 关闭。
- 应用已在运行时再次启动 → 已运行实例的日历自动弹出（单实例，无重复进程）。
- 开机自启：托盘菜单或设置中勾选（写注册表 `HKCU\...\Run`，项名「Z日历」，指向当前 exe 路径）。
- 月份标题点击回到今天；‹ › 切换月份；底部工具条：日程 / 今天 / ＋ / 设置 / 退出。

## 构建

```cmd
cd rust
build.cmd          REM 将 E:\tools\mingw64\bin 加入 PATH 后执行 cargo build --release 与 cargo test
```

- 工具链：stable-x86_64-pc-windows-gnu（rustup 默认）+ [MinGW-w64](https://winlibs.com)（gcc，位于 `E:\tools\mingw64`，构建 `ring` 等需要）。
- 产物：`rust/target/release/CalendarFlyout.exe`（约 1.9MB，静态链接 CRT/pthread，可直接拷贝运行）。
- **图标**：构建时以 `rust/icon.png` 为源自动生成多尺寸 `icon.ico`（16~256px）嵌入 exe，托盘图标与 exe 图标同源；换图标只需替换 `icon.png` 后重新构建。
- `cargo test` 内置农历转换/节气/节日单元测试与天气联网测试。

## 目录结构

```
rust/                 源码（Cargo 项目）
  build.cmd           构建脚本（设置 MinGW PATH 后 cargo build/test）
  build.rs            构建脚本：icon.png → 多尺寸 ico 生成、资源嵌入、托盘 RGBA 导出
  icon.png            应用图标源图（构建时自动生成 rust/icon.ico，勿手改）
  src/main.rs         装配：单实例事件、ICS/天气后台更新线程、消息循环
  src/flyout.rs       日历弹窗（Win32 窗口 + GDI+ 绘制日历/日程/设置三页 + IME 输入）
  src/gdi.rs          GDI+ 平面 API 封装与绘制助手
  src/overlay.rs      任务栏时钟接管（Win32 透明覆盖层线程）
  src/tray.rs         托盘图标与菜单（图标来自构建期生成的 RGBA）
  src/config.rs       配置读写与开机自启（注册表 Run）
  src/ics.rs          holidays.ics 下载解析（休/班/假期区间）
  src/weather.rs      IP 定位 + Open-Meteo 天气获取与缓存
  src/lunar.rs        农历/节气/节日算法（含单元测试）
  src/lunar_data.rs   生成的 1900~2100 数据表
  gen-lunar.js        数据表生成器（数据源 lunar-javascript）
tools/                开发辅助脚本（截图/内存/取色等）
```

## 数据与配置

配置与缓存均在 `%APPDATA%\CalendarFlyout\`：

- `config.json` — 配置项
- `holidays.json` — 法定节假日缓存
- `weather.json` — 天气缓存（3 小时有效）
- `agenda.json` — 日程

## 实现要点

- 时钟定位：`Shell_TrayWnd → TrayNotifyWnd → TrayClockWClass`（Win10），每秒校正；覆盖层全窗 alpha=1/255（肉眼不可见但可接收点击）；全屏应用或任务栏隐藏时自动隐藏覆盖层，explorer 重启后自动恢复。
- 弹窗常驻渲染：窗口"隐藏"时停放在屏幕外 (32000,32000)（winit 对隐藏窗口不派发重绘，事件循环会停摆，离屏方案绕开此限制）。
- 渲染：GDI+ 双缓冲 + UpdateLayeredWindow 分层窗口（无 GPU 框架，锁屏/远程会话均可运行）。
- 点击外部关闭：轮询 `GetAsyncKeyState` + 光标位置（不依赖焦点，`SetForegroundWindow` 被拒绝时也能正常关闭）；设置窗口失焦自动隐藏。
- 配色取自设计图：背景 `#202838`、强调蓝 `#3E87FA`、周末红 `#E54B4B`。
- 调试钩子（环境变量）：`CAL_PAGE=agenda|settings`、`CAL_SETTINGS=1` 指定起始页面，`CAL_YM=2026-10` 指定起始月份，`CAL_DUMP=x.bmp` / `CAL_DUMP2=x.bmp` 保存日历/设置窗首帧，`CAL_DUMP_EXIT=1` 退出时转储状态。
