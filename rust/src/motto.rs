//! 时间格言：一言 API（hitokoto.cn）拉取
//! 分类：d=名人名言 a=文学 i=互联网 k=科普，请求 https://v1.hitokoto.cn/?c=x
//!
//! 不缓存：每次打开日期侧栏（即每次点击日期）都重新拉取一条。
//! 新的一条返回前继续显示上一条，避免闪回内置格言；拉取中/失败时由调用方
//! 用内置格言兜底。失败后 1 分钟内不再重复请求，防止网络异常时连续点日期刷屏。
use serde::Deserialize;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 分类字母 → 显示名（顺序即设置下拉顺序）
pub const TYPES: &[(char, &str)] = &[('d', "名人名言"), ('a', "文学"), ('i', "互联网"), ('k', "科普")];

/// 拉取失败后的最小重试间隔
const RETRY_MS: u64 = 60 * 1000;

#[derive(Clone, Debug)]
pub struct Motto {
    pub text: String,
    pub from: String,
}

#[derive(Deserialize)]
struct Hitokoto {
    #[serde(default)]
    hitokoto: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    from_who: Option<String>,
}

/// 当前格言槽位
struct Entry {
    /// 分类字母
    ty: String,
    /// 属于第几次“打开侧栏”
    gen: u64,
    motto: Motto,
}

static ENTRY: Mutex<Option<Entry>> = Mutex::new(None);
static INFLIGHT: AtomicBool = AtomicBool::new(false);
/// 每次打开侧栏 +1：据此判断槽位是否属于本轮，每次点击日期都换一条
static GEN: AtomicU64 = AtomicU64::new(0);
static LAST_FAIL_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 侧栏显示时调用：本轮槽位作废，触发重新拉取
pub fn on_sidebar_show() {
    GEN.fetch_add(1, Ordering::Relaxed);
}

/// 当前应显示的格言；尚未取到返回 None（调用方用内置格言兜底）
pub fn current(ty: &str) -> Option<Motto> {
    ensure(ty);
    let g = ENTRY.lock().unwrap();
    let e = g.as_ref()?;
    // 换分类后不显示旧分类的内容；新的一条返回前继续显示上一条
    if e.ty != ty {
        return None;
    }
    Some(e.motto.clone())
}

/// 需要时后台拉取（重复调用安全；阻塞仅发生在后台线程）
fn ensure(ty: &str) {
    let gen = GEN.load(Ordering::Relaxed);
    {
        let g = ENTRY.lock().unwrap();
        // 本轮已经取到同分类的格言则不再请求
        if g.as_ref().map(|e| e.ty == ty && e.gen == gen).unwrap_or(false) {
            return;
        }
    }
    if INFLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    if now_ms() - LAST_FAIL_MS.load(Ordering::Relaxed) < RETRY_MS {
        INFLIGHT.store(false, Ordering::SeqCst);
        return;
    }
    let ty = ty.to_string();
    let spawned = std::thread::Builder::new()
        .name("motto".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            match fetch_quote(&ty) {
                Some(m) => {
                    // 记录完成时的代数：拉取期间用户又点了日期，本轮结果仍然有效
                    *ENTRY.lock().unwrap() = Some(Entry { ty: ty.clone(), gen: GEN.load(Ordering::Relaxed), motto: m });
                    crate::sidebar::post_repaint();
                }
                None => {
                    LAST_FAIL_MS.store(now_ms(), Ordering::Relaxed);
                }
            }
            INFLIGHT.store(false, Ordering::SeqCst);
            crate::trim_working_set();
        })
        .is_ok();
    if !spawned {
        INFLIGHT.store(false, Ordering::SeqCst);
    }
}

/// 启动清理：删除旧版本遗留的格言缓存文件（现已不缓存）
pub fn purge_stale() {
    let _ = std::fs::remove_file(crate::config::data_dir().join("motto.json"));
}

/// 请求一言 API（阻塞，仅后台线程调用）
fn fetch_quote(ty: &str) -> Option<Motto> {
    let url = format!("https://v1.hitokoto.cn/?c={}", ty);
    let mut body = crate::net::agent()
        .get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .ok()?
        .into_reader()
        .take(1 << 20); // 上限 1MB，防异常响应撑爆内存
    let mut buf = Vec::new();
    body.read_to_end(&mut buf).ok()?;
    let v: Hitokoto = serde_json::from_slice(&buf).ok()?;
    let text = v.hitokoto.trim().to_string();
    if text.is_empty() {
        return None;
    }
    // 署名：作者优先，其次出处，均空则归为一言
    let from = v
        .from_who
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let f = v.from.trim().to_string();
            if f.is_empty() { None } else { Some(f) }
        })
        .unwrap_or_else(|| "一言".to_string());
    Some(Motto { text, from })
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_hitokoto() {
        let j = r#"{"id":7451,"uuid":"x","hitokoto":"千淘万漉虽辛苦，吹尽狂沙始到金。","type":"a","from":"浪淘沙","from_who":"刘禹锡"}"#;
        let v: super::Hitokoto = serde_json::from_str(j).unwrap();
        let m = super::Motto { text: v.hitokoto, from: "刘禹锡".into() };
        assert_eq!(m.text, "千淘万漉虽辛苦，吹尽狂沙始到金。");

        // from_who 为 null 时回退到 from
        let j2 = r#"{"hitokoto":"逝者如斯夫，不舍昼夜。","type":"d","from":"《论语》","from_who":null}"#;
        let v2: super::Hitokoto = serde_json::from_str(j2).unwrap();
        assert_eq!(v2.from_who, None);
        assert_eq!(v2.from, "《论语》");
    }

    // 真实网络拉取验证（需联网）
    #[test]
    fn fetch_real_network() {
        for c in ["d", "a", "i", "k"] {
            let m = super::fetch_quote(c);
            println!("c={} -> {:?}", c, m);
            assert!(m.is_some(), "fetch c={} failed", c);
        }
    }

    /// 每次打开侧栏都重新拉取（需联网）；全程不落盘
    #[test]
    fn refreshes_every_show() {
        let cache = crate::config::data_dir().join("motto.json");
        let _ = std::fs::remove_file(&cache);

        // 第一次打开侧栏：等待取到
        super::on_sidebar_show();
        let first = wait_for(|| super::current("d")).expect("首次拉取失败");
        assert_eq!(super::ENTRY.lock().unwrap().as_ref().unwrap().ty, "d");

        // 第二次打开侧栏：槽位立即作废（触发新一次拉取），
        // 但界面上继续显示上一条，不闪回内置格言
        super::on_sidebar_show();
        let gen2 = super::GEN.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(super::current("d").map(|m| m.text), Some(first.text), "新一轮返回前应继续显示上一条");

        // 等新一条落地（代数推进到 gen2）
        wait_for(|| {
            let g = super::ENTRY.lock().unwrap();
            g.as_ref().filter(|e| e.gen == gen2).map(|e| e.motto.clone())
        })
        .expect("重新拉取超时");

        // 换分类：旧分类内容不显示，等新分类取到
        super::on_sidebar_show();
        assert!(super::current("a").is_none(), "换分类后不应沿用旧分类的格言");
        wait_for(|| super::current("a")).expect("换分类后拉取超时");

        assert!(!cache.exists(), "不应写任何缓存文件");
    }

    /// 轮询等待条件成立（最长 15 秒）
    fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
        for _ in 0..150 {
            if let Some(v) = f() {
                return Some(v);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        None
    }
}
