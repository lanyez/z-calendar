//! 进程内统一的"现在"取值（此前 6 个模块各写了一份 now_ms）。
//! 墙上时间：系统时钟校准会前后跳，只用于时间戳、缓存有效期、提醒时点这类用途；
//! 需要单调计时的地方（悬停计时、超时）用 std::time::Instant

/// Unix epoch 以来的毫秒数
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 同上，无符号版本（用于只做"当前 − 时间戳"比较的缓存逻辑）
pub fn now_ms_u64() -> u64 {
    now_ms().max(0) as u64
}
