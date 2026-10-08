//! 轻量诊断日志：追加写 %APPDATA%/z-calendar/zcalendar.log，超过 256KB 轮转为 .old。
//! 供天气拉取失败、节假日数据失败、配置重置、通知回退、panic 等关键事件留痕。
use std::io::Write;
use std::sync::Mutex;

static LOG_LOCK: Mutex<()> = Mutex::new(());
const MAX_BYTES: u64 = 256 * 1024;

pub fn info(msg: &str) {
    write("INFO", msg);
}
pub fn warn(msg: &str) {
    write("WARN", msg);
}
pub fn error(msg: &str) {
    write("ERROR", msg);
}

fn write(level: &str, msg: &str) {
    let _g = LOG_LOCK.lock().unwrap();
    let dir = crate::config::data_dir();
    let path = dir.join("zcalendar.log");
    if let Ok(md) = std::fs::metadata(&path) {
        if md.len() > MAX_BYTES {
            let _ = std::fs::rename(&path, dir.join("zcalendar.old.log"));
        }
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} [{}] {}", ts, level, msg);
    }
}
