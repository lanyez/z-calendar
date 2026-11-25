# Z日历（任务栏日历，Rust 版）

替换 Windows 任务栏"点击时间弹出日历"的日历应用，Rust 原生实现（GDI+ 自绘，无 GPU 框架），编译为**单个约 2.5MB 的独立 exe**，无运行时依赖，常驻内存低（隐藏态工作集约 1MB，显示日历时约 6MB），可直接拷贝到任意目录运行。

## 功能

- **接管任务栏时钟**（Windows 10 / 11）：点击任务栏时间弹出本日历（点击被吞掉，系统日历不再弹出）；右键时钟菜单：软件设置 / 开机自启 / 日期与时间 / 退出；托盘常驻、单实例运行、全屏应用自动放行。
- **法定节假日**：自动拉取更新 [chinese-days](https://github.com/yaavi/chinese-days) 数据，法定节假日/调休显示"休/班"角标。
- **农历 / 节气 / 节日**：内置 1900~2100 农历与二十四节气数据表，传统节日、公历节日、纪念日本地计算，离线可用。
- **月历**：ISO 周数、今日高亮、非本月日期可选显示、年月快速跳转面板、键盘与滚轮操作、多显示器 DPI 跟随、深色/浅色/跟随系统主题、界面字号与字体可调。
- **天气**（数据来自中国气象局 weather.cma.cn）：右上角显示当前温度与天气现象；悬停弹出近 7 天面板——实况温度/体感/湿度/气压/风向风力、逐日天气与风力、气象预警横幅与角标、节气、数据更新时间；支持定位城市 + 最多 4 个自定义城市 tab 切换（热门城市直达、中文搜索添加、悬停删除）。
- **日程**：按日期增删改；跨天日程在区间内每天显示；支持备注与自定义颜色；日程页为 30 天分组视图（‹ 更早 / 回到今天 / 稍后 › 平移、点分组头跳回月历）；底部快捷输入支持 "14:30 评审""明天9点开会""周五 18:30 团建""10月8日 交报告" 等时间与日期词解析。
- **快捷入口**：点击日期弹出详情侧栏、双击日期格直达"新增待办"、右键日期菜单"新增日程 / 新增待办"、右键重复条目可选"仅这一天 / 整个系列"。
- **待办**：优先级（Ⅰ~Ⅳ）、时间与提醒、勾选完成、逾期待办一键顺延、清除已完成。
- **提醒与重复**：默认走系统通知中心（Toast），失败回退右下角内置卡片（提示音可关）；「稍后10分钟」、错过的提醒当日汇总、提醒历史。重复支持分钟级与每天/每周/工作日/每月/每年/农历每年，可设重复至与"仅这一天"例外。
- **编辑与撤销**：弹窗与快捷输入支持中文 IME、剪贴板、全键盘光标与选区、Ctrl+Z/Y 撤销重做、日期时间直接键入；删除（含批量）10 秒内可撤销恢复。
- **日期侧边栏**：日期信息、黄历（建除十二神/宜忌/冲煞）、最近事件、今日日程、历史上的今天、时间格言、待办清单、最近提醒 8 张卡片，显示与顺序可配置。
- **设置窗口**：软件设置（自启/主题/字号/字体/通知方式等）、日历设置（各项显示开关）、侧栏管理（卡片开关与排序）三个页签，更改立即生效。
- **数据自愈**：配置、日程、待办、提醒登记原子写盘 + 两代备份（.bak / .bak1），损坏自动恢复并提示；诊断日志 `zcalendar.log`。

## 启动

- 运行 `rust/target/release/z-calendar.exe`（纯 GUI 无控制台），可复制改名（如 `Z日历.exe`）到任意位置；开机自启在托盘菜单或设置中开启。
- 点击任务栏右下角时间弹出日历；再点时钟 / Esc / 点击日历外任意位置关闭。悬停右上角天气弹出近一周面板。
- 应用已运行时再次启动 → 已运行实例的日历自动弹出（单实例，无重复进程）。
- 键盘：方向键移动选中日期（↑↓ ±7 天）、PgUp/PgDn 翻月、Home 回今天、回车开关日期侧栏、滚轮直接翻月。

从源码构建（工具链 stable-x86_64-pc-windows-gnu + [MinGW-w64](https://winlibs.com)；gcc 用于构建 `ring` 等依赖，windres 用于编译 exe 的图标与版本信息）：

```cmd
cd rust
build.cmd          REM 本地门禁：cargo build --release → clippy → test → tag 与版本号一致性检查
```

- 工具链：stable-x86_64-pc-windows-gnu（rustup 默认）+ [MinGW-w64](https://github.com/niXman/mingw-builds-binaries/releases)（mingw-builds **msvcrt** 版 gcc 16.2，解压到 `E:\tools\mingw64`；须用 msvcrt 运行时变体与 Rust 的 windows-gnu 目标配套，winlibs 的 UCRT 版会混用运行时）。
- 产物：`rust/target/release/z-calendar.exe`（约 2.5MB，静态链接 CRT，可直接拷贝运行；v1.0.4 起含 WinRT 通知中心依赖 `windows` crate）；exe 属性里的文件版本取自 `Cargo.toml` 的 `version`。
- 资源编译：构建时以 `rust/icon.png` 为源自动生成多尺寸 `icon.ico`（16~256px）嵌入 exe（托盘图标同源）。**release 构建要求 `windres` 可用**（MinGW-w64 自带，须在 PATH 里），否则直接报错——避免发版构建悄悄链接旧的预编译资源、exe 里的版本信息停在过期版本上。只有调试/测试构建会回退链接 `prebuilt-resource.o`；该文件在 windres 可用时由构建自动同步，更换图标或版本号后无需手工维护。确要在没有 windres 的环境做 release 构建，设 `ZC_ALLOW_PREBUILT_RES=1`。
- 测试：`cargo test` 只跑离线单元测试（农历/节气/黄历、重复展开、提醒判定、配置读写与备份恢复、ICS 解析、文本编辑内核）；5 个联网测试默认 `#[ignore]`，需要时显式运行 `cargo test -- --ignored`。
- CI：仓库托管在 Gitee，`.github/workflows/ci.yml` 不会被 GitHub Actions 执行（镜像到 GitHub 后才生效）；实际生效的门禁是 `build.cmd`。

## 目录结构

```
rust/                 源码（Cargo 项目）
  build.cmd           本地门禁脚本（设置 MinGW PATH 后：release 构建 → clippy → 测试 → tag/版本号一致性检查）
  build.rs            构建脚本：icon.png → 多尺寸 ico、资源嵌入、托盘 RGBA 导出
  icon.png            应用图标源图（构建时自动生成 rust/icon.ico，勿手改）
  prebuilt-resource.o 预编译资源对象（无资源编译器环境的图标/版本信息回退；构建自动同步）
  gen-lunar.js        农历数据表生成器（数据源 lunar-javascript）
  src/
    main.rs           装配：单实例唤醒、ICS/天气后台线程、消息循环
    flyout.rs         主日历弹窗（窗口/GDI+ 绘制/交互/设置窗/天气面板）
    wxcity.rs         天气城市选择弹窗（热门城市 + 搜索添加）
    inputbox.rs       新建/编辑日程·待办表单弹窗
    sidebar.rs        日期侧边栏（8 张卡片渲染与交互）
    gdi.rs            GDI/GDI+ 绘制封装；Dib = 分层窗口共用的后台位图（分配/释放/绘制/提交）
    menuwin.rs        通用弹出菜单小窗（下列三个菜单共用）
    ctxmenu.rs        任务栏时钟右键菜单
    ctxmenu_date.rs   日期右键菜单（新增日程 / 新增待办）
    recur_menu.rs     重复条目删除范围选择菜单（仅这一天/整个系列）
    theme.rs          深色/浅色/跟随系统主题
    config.rs         配置读写与开机自启（注册表 Run）；数据文件读写的通用设施
    events.rs         日程数据模型与重复展开
    textedit.rs       通用文本编辑内核（光标/选区/撤销）
    clipboard.rs      剪贴板读写（CF_UNICODETEXT）
    ime.rs            IME 组合串读取（imm32）
    tooltip.rs        截断文本悬停提示
    overlay.rs        任务栏时钟点击接管（低级鼠标钩子）
    uia_clock.rs      Win11 任务栏时钟 UIA 定位线程
    tray.rs           托盘图标与菜单
    weather.rs        天气：IP 定位、中国气象局数据、城市搜索与多城市缓存
    ics.rs            法定节假日 holidays.ics 下载解析
    lunar.rs          农历/节气/节日算法
    lunar_data.rs     生成的 1900~2100 数据表
    almanac.rs        黄历计算（干支/星座/建除十二神/宜忌/冲煞）
    history.rs        "历史上的今天"数据集
    motto.rs          时间格言（一言 API）
    reminder.rs       提醒扫描引擎（后台线程）
    toast.rs          内置提醒卡片
    wnotify.rs        系统通知中心（WinRT Toast + 激活分发）
    log.rs            诊断日志
    net.rs            IPv4 优先 HTTP Agent（全应用共用）
    clock.rs          统一的"当前时间"取值
    debug.rs          调试开关（CAL_* 环境变量）集中登记 + 首帧转储
    selftest.rs       界面自检（cargo feature `selftest`，默认不编译）
```

## 开发与调试

- 环境变量开关集中登记在 `src/debug.rs`（文件头有完整列表）：首帧转储 `CAL_DUMP`/`CAL_DUMP2`/`CAL_DUMP3`/`CAL_DUMP4`、强制缩放 `CAL_SF`、文本渲染提示 `CAL_TEXT_HINT`、示例天气 `CAL_FAKE_WX` 等。发布包同样可用。
- 与已安装的实例并行运行开发构建：`CAL_INSTANCE=dev` 给单实例事件名加后缀，`CAL_DATA_DIR=<临时目录>` 改用独立的数据目录。
- 界面自检——无人值守的截图回归（默认不编译进 exe）：

  ```cmd
  cargo build --release --features selftest --target-dir %TEMP%\zc-target
  set CAL_INSTANCE=selftest
  set CAL_DATA_DIR=%TEMP%\zc-selftest\data
  set CAL_FAKE_WX=1
  set CAL_SELFTEST=%TEMP%\zc-selftest\shots
  %TEMP%\zc-target\release\z-calendar.exe
  ```

  往隔离的数据目录铺一份样例数据后，把每个窗口的各主要状态（主面板各页与各弹出层、设置窗三个页签、天气面板、侧栏、三个菜单、编辑弹窗各状态、城市弹窗、提醒卡片、悬停提示）各渲染一帧写到 `shots\*.bmp` 后退出——全程不显示窗口、不碰真实数据。`CAL_SELFTEST_LIGHT=1` 改用浅色主题。界面重构后跑一遍即可回归。

## 数据与配置

配置与数据均在 `%APPDATA%\z-calendar\`：

- `config.json` — 配置项（主题/字号/字体/各显示开关/侧栏卡片顺序/天气自定义城市 `wx_cities` 等）
- `agenda.json` — 日程（名称/全天/起止/提醒/重复/重复至/例外日期，兼容旧纯文本条目）
- `todo.json` — 待办（文本/完成/日期 + 优先级/时间/提醒/重复/完成记录）
- `remind_done.json` / `snooze.json` / `remind_log.json` — 已提醒登记 / 稍后提醒登记 / 提醒历史
- 以上用户数据写入前自动滚动保留两代备份（`.bak` / `.bak1`）；读取损坏时自动从备份恢复并提示，坏文件留档为 `.corrupt`
- `weather.json` / `stations.json` / `holidays.json` — 天气 / 站号表 / 法定节假日缓存（可重新拉取：原子写盘、不留备份，损坏即视为无缓存）
- `history\1..12.json` — "历史上的今天"按月缓存（同上）
- `zcalendar.log` — 诊断日志（启动、数据拉取失败、通知回退、panic 等；超 256KB 轮转为 `.old.log`）
