//! 界面自检（`--features selftest`，默认不编译、不进发布包）。
//!
//! `CAL_SELFTEST=<dir>` 时：往（隔离的）数据目录铺一份样例数据，走正常启动流程，
//! 然后把每个窗口的各主要状态各渲染一帧——不显示窗口，直接取后台位图——写成
//! `<dir>/*.bmp` 后退出。用于界面重构后的无人值守回归：与已安装实例互不干扰地
//! 并行运行（单实例事件名与数据目录都隔离开）。
//!
//! ```text
//! cargo build --release --features selftest --target-dir <tmp>
//! set CAL_INSTANCE=selftest
//! set CAL_DATA_DIR=<tmp>\data
//! set CAL_FAKE_WX=1
//! set CAL_SELFTEST=<tmp>\shots
//! <tmp>\release\z-calendar.exe
//! ```
//!
//! `CAL_SELFTEST_LIGHT=1` 改用浅色主题。截图可用 tools/bmp2png2.py 转成 PNG 查看。
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::debug;
use crate::events::{AgendaEntry, AgendaMap, RichEvent};
use crate::sidebar::Todo;

pub fn requested() -> bool {
    !debug::value(debug::SELFTEST).is_empty()
}

pub fn dir() -> Option<PathBuf> {
    debug::value_os(debug::SELFTEST).map(PathBuf::from)
}

/// 铺样例数据（已存在的文件不覆盖）：覆盖日程页 / 侧栏 / 月历角标的各种条目形态
pub fn seed_sample_data() {
    let dir = crate::config::data_dir();
    let today = chrono::Local::now().date_naive();
    let day = |n: i64| today + chrono::Duration::days(n);
    let key = |n: i64| crate::ics::key_of_date(day(n));
    let rich = |name: &str, start: String, end: String| RichEvent {
        id: crate::events::gen_id(),
        name: name.into(),
        all_day: false,
        start,
        end,
        remind: None,
        repeat: None,
        recur: None,
        recur_until: None,
        skip_dates: vec![],
        note: None,
        color: None,
    };
    let at = |n: i64, hm: &str| format!("{} {hm}", key(n));
    let all_day = |n: i64| key(n);

    let mut agenda: AgendaMap = std::collections::HashMap::new();
    agenda.insert(key(-1), vec![AgendaEntry::Rich(rich("昨日复盘", at(-1, "16:00"), at(-1, "17:00")))]);
    let mut standup = rich("站会", at(-9, "09:30"), at(-9, "10:00"));
    standup.recur = Some("w".into());
    agenda.insert(key(-9), vec![AgendaEntry::Rich(standup)]);
    let mut review = rich("项目评审", at(0, "14:30"), at(0, "15:30"));
    review.color = Some(2);
    let mut training = rich("培训", all_day(0), all_day(0));
    training.all_day = true;
    agenda.insert(
        key(0),
        vec![
            AgendaEntry::Rich(review),
            AgendaEntry::Rich(training),
            AgendaEntry::Legacy("旧文本日程 16:00 同步".to_string()),
        ],
    );
    agenda.insert(key(1), vec![AgendaEntry::Rich(rich("客户拜访", at(1, "10:00"), at(1, "11:30")))]);
    agenda.insert(key(3), vec![AgendaEntry::Rich(rich("出差", at(3, "08:00"), at(6, "18:00")))]);
    agenda.insert(key(12), vec![AgendaEntry::Rich(rich("月度总结", at(12, "15:00"), at(12, "16:00")))]);

    let todo = |text: &str, date: NaiveDate| Todo {
        id: crate::events::gen_id(),
        text: text.into(),
        done: false,
        date: Some(crate::ics::key_of_date(date)),
        priority: 0,
        has_time: false,
        start: None,
        end: None,
        remind: None,
        repeat: None,
        recur: None,
        recur_until: None,
        skip_dates: vec![],
        done_dates: vec![],
        note: None,
        color: None,
    };
    let mut timed = todo("交周报", today);
    timed.has_time = true;
    timed.priority = 1;
    timed.start = Some(at(0, "14:00"));
    let mut done = todo("已完成的事项", day(-1));
    done.done = true;
    let mut daily = todo("喝水", day(-5));
    daily.recur = Some("d".into());
    let todos = vec![timed, todo("逾期待办", day(-3)), todo("随时可做", today), done, daily];

    let log = vec![
        crate::reminder::RemindLogEntry { t: crate::clock::now_ms() - 3_600_000, title: "日程提醒".into(), body: "09:30 站会".into() },
        crate::reminder::RemindLogEntry { t: crate::clock::now_ms() - 7_200_000, title: "待办提醒".into(), body: "全天 交材料".into() },
    ];

    // 自检实例不在托盘留图标；侧栏各卡片全开以便覆盖
    let cfg = crate::config::Config {
        show_tray: false,
        sidebar_history: true,
        sidebar_motto: true,
        sidebar_reminds: true,
        theme: if debug::flag(debug::SELFTEST_LIGHT) { 2 } else { 1 },
        ..crate::config::Config::default()
    };

    let put = |name: &str, text: String| {
        let p = dir.join(name);
        if !p.exists() {
            let _ = std::fs::write(p, text);
        }
    };
    put("config.json", serde_json::to_string_pretty(&cfg).unwrap_or_default());
    put("agenda.json", serde_json::to_string(&agenda).unwrap_or_default());
    put("todo.json", serde_json::to_string(&todos).unwrap_or_default());
    put("remind_log.json", serde_json::to_string(&log).unwrap_or_default());
}

/// 各窗口渲染一帧并转储，然后退出进程
pub fn run(dir: &Path, st: crate::flyout::SharedState) {
    let _ = std::fs::create_dir_all(dir);
    let t0 = std::time::Instant::now();
    crate::flyout::debug_render(dir);
    crate::sidebar::debug_render(dir);
    crate::ctxmenu::debug_render(dir);
    crate::ctxmenu_date::debug_render(dir);
    crate::recur_menu::debug_render(dir);
    crate::inputbox::debug_render(dir);
    crate::wxcity::debug_render(st, dir);
    crate::toast::debug_render(dir);
    crate::tooltip::debug_render(dir);
    eprintln!("selftest: frames written to {} in {:?}", dir.display(), t0.elapsed());
    std::process::exit(0);
}
