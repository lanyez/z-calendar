//! Win11 任务栏时钟定位（UI Automation）：
//! Win11 任务栏整体改为 XAML 渲染，时钟没有 TrayClockWClass 这类 Win32 子窗口，
//! 经典的 Shell_TrayWnd → TrayNotifyWnd → TrayClockWClass 窗口链定位不到它。
//! 这里起一个独立后台线程，用 UIA 在 Shell_TrayWnd 的子树里找"UIA Name 是日期
//! 时间文本"的元素（时钟按钮的 Name 就是时钟文本，且位于任务栏最右端），把矩形
//! 写进原子量快照，供 overlay::find_clock 在经典链失败时回退读取。
//!
//! 为什么独立成线程：UIA 是跨进程 COM 调用，shell 忙时可能阻塞数百毫秒；低级鼠标
//! 钩子装在 overlay 线程上，若该线程被 UIA 卡住，钩子回调超时会被系统直接摘除。
//!
//! winapi 0.3 没有 UIA 绑定，这里按 SDK 头文件 UIAutomationClient.h 手写最小 COM
//! 声明：只按虚表槽位调用 ElementFromHandle / CreatePropertyCondition /
//! CreateTrueCondition / FindAll / GetCurrentPropertyValue，槽位序号与头文件一致。
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::time::Duration;

use winapi::ctypes::c_void;
use winapi::shared::guiddef::GUID;
use winapi::shared::windef::{HWND, RECT};
use winapi::shared::wtypes::BSTR;
use winapi::um::combaseapi::{CoCreateInstance, CoInitializeEx, CoUninitialize};
use winapi::um::libloaderapi::{GetModuleHandleW, GetProcAddress};
use winapi::um::oaidl::SAFEARRAY;
use winapi::um::objbase::COINIT_MULTITHREADED;
use winapi::um::oleauto::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayUnaccessData, SysFreeString,
};
use winapi::um::winnt::HRESULT;
use winapi::um::winuser::{FindWindowExW, IsWindowVisible};

// ---------- UIA 常量（UIAutomationClient.h） ----------

/// CLSID_CUIAutomation
const CLSID_CUIAUTOMATION: GUID = GUID {
    Data1: 0xff48dba4,
    Data2: 0x60ef,
    Data3: 0x4201,
    Data4: [0xaa, 0x87, 0x54, 0x10, 0x3e, 0xef, 0x59, 0x4e],
};
/// IID_IUIAutomation
const IID_IUIAUTOMATION: GUID = GUID {
    Data1: 0x30cbe57d,
    Data2: 0xd9d0,
    Data3: 0x452a,
    Data4: [0xab, 0x13, 0x7a, 0xc5, 0xac, 0x48, 0x25, 0xee],
};

// IUIAutomation 虚表槽位（0~2 为 IUnknown）：
// 3 CompareElements 4 CompareRuntimeIds 5 GetRootElement 6 ElementFromHandle
// 7 ElementFromPoint 8 GetFocusedElement 9~12 *BuildCache 13 CreateTreeWalker
// 14~19 walker/condition 属性 20 CreateCacheRequest 21 CreateTrueCondition
// 22 CreateFalseCondition 23 CreatePropertyCondition
const SLOT_ELEMENT_FROM_HANDLE: usize = 6;
const SLOT_CREATE_TRUE_CONDITION: usize = 21;
const SLOT_CREATE_PROPERTY_CONDITION: usize = 23;
// IUIAutomationElement 虚表槽位：3 SetFocus 4 GetRuntimeId 5 FindFirst
// 6 FindAll 7~9 *BuildCache 10 GetCurrentPropertyValue
const SLOT_FIND_ALL: usize = 6;
const SLOT_GET_CURRENT_PROPERTY_VALUE: usize = 10;
// IUIAutomationElementArray 虚表槽位：3 get_Length 4 GetElement
const SLOT_ARRAY_LENGTH: usize = 3;
const SLOT_ARRAY_GET_ELEMENT: usize = 4;

const UIA_BOUNDING_RECTANGLE: i32 = 30001;
const UIA_CONTROL_TYPE: i32 = 30003;
const UIA_NAME: i32 = 30005;
const UIA_BUTTON_CONTROL_TYPE: i32 = 50000;
/// TreeScope_Descendants
const TREE_SCOPE_DESCENDANTS: i32 = 4;

const VT_EMPTY: u16 = 0;
const VT_I4: u16 = 3;
const VT_R8: u16 = 5;
const VT_BSTR: u16 = 8;
const VT_ARRAY: u16 = 0x2000;

/// CoInitializeEx 返回：线程已被初始化为 STA（按文档继续使用 COM，只是不能 Uninit）
const RPC_E_CHANGED_MODE: HRESULT = 0x80010106u32 as i32;

/// CLSCTX_INPROC_SERVER（winapi 未公开导出该常量，值固定为 1）
const CLSCTX_INPROC_SERVER: u32 = 1;

// ---------- 最小 VARIANT（布局同 OLECHAR VARIANT，16 字节） ----------

#[repr(C)]
struct Variant {
    vt: u16,
    _r1: u16,
    _r2: u16,
    _r3: u16,
    val: VariantVal,
}

#[repr(C)]
union VariantVal {
    i4: i32,
    bstr: BSTR,
    array: *mut SAFEARRAY,
}

impl Variant {
    fn i4(v: i32) -> Variant {
        Variant {
            vt: VT_I4,
            _r1: 0,
            _r2: 0,
            _r3: 0,
            val: VariantVal { i4: v },
        }
    }
    fn empty() -> Variant {
        Variant {
            vt: VT_EMPTY,
            _r1: 0,
            _r2: 0,
            _r3: 0,
            val: VariantVal { i4: 0 },
        }
    }
}

unsafe fn variant_clear(v: &mut Variant) {
    match v.vt {
        VT_BSTR => {
            if !v.val.bstr.is_null() {
                SysFreeString(v.val.bstr);
            }
        }
        t if t == VT_ARRAY | VT_R8 => {
            if !v.val.array.is_null() {
                SafeArrayDestroy(v.val.array);
            }
        }
        _ => {}
    }
    v.vt = VT_EMPTY;
}

// ---------- COM 调用辅助 ----------

type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
type ElementFromHandleFn =
    unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT;
type CreateTrueConditionFn =
    unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
type CreatePropertyConditionFn = unsafe extern "system" fn(
    *mut c_void,
    i32,
    Variant,
    *mut *mut c_void,
) -> HRESULT;
type FindAllFn = unsafe extern "system" fn(
    *mut c_void,
    i32,
    *mut c_void,
    *mut *mut c_void,
) -> HRESULT;
type GetCurrentPropValueFn =
    unsafe extern "system" fn(*mut c_void, i32, *mut Variant) -> HRESULT;
type ArrayLenFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT;
type ArrayGetFn =
    unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT;

/// 取 COM 对象虚表第 slot 个槽位的函数指针
unsafe fn slot(obj: *mut c_void, idx: usize) -> *const () {
    let vt = *(obj as *mut *mut *const ());
    *vt.add(idx)
}

unsafe fn com_release(obj: *mut c_void) {
    if !obj.is_null() {
        let f: ReleaseFn = std::mem::transmute(slot(obj, 2));
        f(obj);
    }
}

/// 读元素的 UIA 属性（返回未初始化的 VARIANT，由调用方 variant_clear）
unsafe fn element_property(elem: *mut c_void, id: i32, out: &mut Variant) -> bool {
    let f: GetCurrentPropValueFn =
        std::mem::transmute(slot(elem, SLOT_GET_CURRENT_PROPERTY_VALUE));
    f(elem, id, out) == 0
}

/// 元素的 UIA Name（BSTR → String）
unsafe fn element_name(elem: *mut c_void) -> Option<String> {
    let mut v = Variant::empty();
    if !element_property(elem, UIA_NAME, &mut v) {
        return None;
    }
    let s = if v.vt == VT_BSTR && !v.val.bstr.is_null() {
        let mut len = 0usize;
        while len < 512 && *v.val.bstr.add(len) != 0 {
            len += 1;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(v.val.bstr, len)))
    } else {
        None
    };
    variant_clear(&mut v);
    s
}

/// 元素的 UIA BoundingRectangle（SAFEARRAY of 4 doubles：left/top/width/height）
unsafe fn element_rect(elem: *mut c_void) -> Option<RECT> {
    let mut v = Variant::empty();
    if !element_property(elem, UIA_BOUNDING_RECTANGLE, &mut v) {
        return None;
    }
    let mut rect = None;
    if v.vt == VT_ARRAY | VT_R8 && !v.val.array.is_null() {
        let psa = v.val.array;
        let mut data: *mut c_void = null_mut();
        if SafeArrayAccessData(psa, &mut data) == 0 && !data.is_null() {
            let nums = std::slice::from_raw_parts(data as *const f64, 4);
            let (l, t, w, h) = (nums[0], nums[1], nums[2], nums[3]);
            // 离屏/虚拟元素会报负宽高或 -32000 之类坐标，一律视为无效
            if w > 0.0 && h > 0.0 {
                rect = Some(RECT {
                    left: l as i32,
                    top: t as i32,
                    right: (l + w) as i32,
                    bottom: (t + h) as i32,
                });
            }
            SafeArrayUnaccessData(psa);
        }
    }
    variant_clear(&mut v);
    rect
}

// ---------- 时钟识别 ----------

/// 判定 UIA Name 是否像时钟文本：包含 "H:MM" 形式的时间记号（半角/全角冒号）且
/// 整体较短。任务栏上其他按钮（应用标题等）极少带这种记号；即便撞上（如视频
/// 标题带时长），"取最右"规则也能排除——时钟永远位于任务栏最右端。
fn looks_like_clock_name(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() || chars.len() > 64 {
        return false;
    }
    let is_digit = |c: char| c.is_ascii_digit();
    let mut i = 0;
    while i < chars.len() {
        if is_digit(chars[i]) {
            let mut j = i;
            while j < chars.len() && is_digit(chars[j]) && j - i < 2 {
                j += 1;
            }
            // 1~2 位小时 + 冒号 + 2 位分钟
            if j < chars.len()
                && (chars[j] == ':' || chars[j] == '：')
                && j + 2 < chars.len()
                && is_digit(chars[j + 1])
                && is_digit(chars[j + 2])
            {
                return true;
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    false
}

// ---------- 定位线程 ----------

/// UIA 定位结果快照（供 overlay 线程读取；写者只有本模块的定位线程）
static UIA_VALID: AtomicBool = AtomicBool::new(false);
static UIA_L: AtomicI32 = AtomicI32::new(0);
static UIA_T: AtomicI32 = AtomicI32::new(0);
static UIA_R: AtomicI32 = AtomicI32::new(0);
static UIA_B: AtomicI32 = AtomicI32::new(0);
/// 4 坐标写入的 seqlock 版本号：奇=写入中，偶=稳定。读者撞上写一半时重读，
/// 保证拼出的矩形 4 边来自同一轮定位（4 个独立原子量的非原子组会撕出"左新右旧"）
static UIA_SEQ: AtomicUsize = AtomicUsize::new(0);

/// overlay 线程每秒读取：UIA 最近一次定位到的时钟矩形（屏幕坐标；进程未声明
/// DPI 感知，与 GetWindowRect 返回的坐标系同为虚拟化坐标，二者可直接混用）
pub fn snapshot_rect() -> Option<RECT> {
    if !UIA_VALID.load(Ordering::Relaxed) {
        return None;
    }
    loop {
        let v1 = UIA_SEQ.load(Ordering::Acquire);
        if v1 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        let r = RECT {
            left: UIA_L.load(Ordering::Relaxed),
            top: UIA_T.load(Ordering::Relaxed),
            right: UIA_R.load(Ordering::Relaxed),
            bottom: UIA_B.load(Ordering::Relaxed),
        };
        if UIA_SEQ.load(Ordering::Acquire) == v1 {
            return Some(r);
        }
    }
}

/// 是否 Windows 11 或更新（build ≥ 22000）：任务栏时钟为 XAML 渲染，需要 UIA 定位。
/// 用 RtlGetVersion 而非 GetVersionEx，后者在无清单声明时会被兼容性垫片改写。
pub fn is_win11_or_newer() -> bool {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        csd: [u16; 128],
    }
    unsafe {
        let ntdll = GetModuleHandleW(crate::wide("ntdll.dll").as_ptr());
        if ntdll.is_null() {
            return false;
        }
        let proc = GetProcAddress(ntdll, b"RtlGetVersion\0".as_ptr() as *const i8);
        if proc.is_null() {
            return false;
        }
        type RtlGetVersionFn = unsafe extern "system" fn(*mut OsVersionInfoW) -> i32;
        let f: RtlGetVersionFn = std::mem::transmute(proc);
        let mut vi: OsVersionInfoW = std::mem::zeroed();
        vi.size = std::mem::size_of::<OsVersionInfoW>() as u32;
        if f(&mut vi) != 0 {
            return false;
        }
        vi.build >= 22000
    }
}

/// 启动 UIA 时钟定位线程（仅 Win11+ 调用；Win10 走经典窗口链，无需本线程）
pub fn spawn() {
    std::thread::Builder::new()
        .name("uia-clock".into())
        .stack_size(512 * 1024)
        .spawn(|| unsafe {
            let hr = CoInitializeEx(null_mut(), COINIT_MULTITHREADED);
            if hr < 0 && hr != RPC_E_CHANGED_MODE {
                // Win11 上走到这里意味着 UIA 定位线程整个不工作（时钟只能靠经典
                // 窗口链，大概率找不到）：必须留痕，否则无从排查
                crate::log::warn(&format!("UIA: CoInitializeEx 失败 hr={:#010X}，时钟定位线程退出", hr));
                return;
            }
            let need_uninit = hr == 0 || hr == 1; // S_OK / S_FALSE
            let clsid = CLSID_CUIAUTOMATION;
            let iid = IID_IUIAUTOMATION;
            let mut uia: *mut c_void = null_mut();
            let ok = CoCreateInstance(
                &clsid,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &iid,
                &mut uia,
            ) == 0
                && !uia.is_null();
            if !ok {
                crate::log::warn("UIA: CoCreateInstance(CUIAutomation) 失败，时钟定位线程退出");
                if need_uninit {
                    CoUninitialize();
                }
                return;
            }
            let mut st = ScanState { buttons_only: true, button_misses: 0, fulltree_runs: 0, fails: 5 };
            // 空闲内存防涨：UIA 跨进程扫描每轮都会把 COM/UIA 代码页拉回工作集
            // （数 MB 级），固定 1 秒一轮会让“隐藏态低内存”目标失效。时钟矩形
            // 稳定后按 1→2→4→8→10 秒退频；矩形变化或丢失（任务栏隐藏/资源
            // 管理器重启）立即回到 1 秒快扫。
            let mut interval: u64 = 1;
            let mut stable_runs: u32 = 0;
            let mut last: Option<[i32; 4]> = None;
            // 主面板隐藏时低频修剪：每轮扫描确实会把 COM/UIA 代码页拉回工作集，但
            // 每轮都修剪与换页互相打架反而制造 churn（且与主面板/提醒线程的修剪
            // 冲突），收敛到 5 分钟一次；隐藏瞬间的修剪由主面板空闲路径负责。
            let mut last_trim = std::time::Instant::now();
            loop {
                scan_tick(uia, &mut st);
                let cur = if UIA_VALID.load(Ordering::Relaxed) {
                    Some([
                        UIA_L.load(Ordering::Relaxed),
                        UIA_T.load(Ordering::Relaxed),
                        UIA_R.load(Ordering::Relaxed),
                        UIA_B.load(Ordering::Relaxed),
                    ])
                } else {
                    None
                };
                if cur.is_some() && cur == last {
                    stable_runs += 1;
                    if stable_runs >= 3 && interval < 10 {
                        interval = (interval * 2).min(10);
                        stable_runs = 0;
                    }
                } else if cur != last {
                    interval = 1;
                    stable_runs = 0;
                }
                last = cur;
                // 主面板隐藏时低频修剪（5 分钟节流）
                let fh = crate::flyout::hwnd() as HWND;
                if (fh.is_null() || IsWindowVisible(fh) == 0)
                    && last_trim.elapsed() >= Duration::from_secs(300)
                {
                    crate::trim_working_set();
                    last_trim = std::time::Instant::now();
                }
                std::thread::sleep(Duration::from_secs(interval));
            }
        })
        .ok();
}

struct ScanState {
    /// true=只在 ControlType=Button 的元素里找（快）；连续 3 轮找不到则降级为
    /// 全子树扫描（个别版本时钟可能不暴露为 Button，只能靠 Name 匹配兜底）
    buttons_only: bool,
    button_misses: u32,
    /// 降级后连续全子树命中轮数：到阈值后安排一次"回试轮"重走按钮快路径
    /// （降级不该是永久的——Button 形态偶发缺失恢复后即回到廉价扫描）
    fulltree_runs: u32,
    /// 连续失败轮数（≥5 清空快照，避免 explorer 重启后残留旧矩形）
    fails: u32,
}

/// 连续多少轮全子树命中后回试一次按钮模式（按 1~10 秒轮询约 1~3 分钟一次）
const FULLTREE_PROBE_AT: u32 = 30;

/// CAL_UIA_LOG 只在启动时读一次（每轮读环境变量无谓开销）
static UIA_LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

unsafe fn scan_tick(uia: *mut c_void, st: &mut ScanState) {
    let logging = *UIA_LOG.get_or_init(|| std::env::var("CAL_UIA_LOG").map(|v| v == "1").unwrap_or(false));
    let mut log: Vec<String> = Vec::new();
    let found = locate(uia, st, &mut log);
    match found {
        Some(r) => {
            st.fails = 0;
            // seqlock 写侧：先置奇（写入中）→ 写 4 坐标 → 再置偶（稳定）
            UIA_SEQ.fetch_add(1, Ordering::SeqCst);
            UIA_L.store(r.left, Ordering::Relaxed);
            UIA_T.store(r.top, Ordering::Relaxed);
            UIA_R.store(r.right, Ordering::Relaxed);
            UIA_B.store(r.bottom, Ordering::Relaxed);
            UIA_SEQ.fetch_add(1, Ordering::SeqCst);
            UIA_VALID.store(true, Ordering::Relaxed);
            if logging {
                log.insert(0, format!("mode=buttons:{} OK ({},{})-({},{})", st.buttons_only, r.left, r.top, r.right, r.bottom));
            }
        }
        None => {
            st.fails += 1;
            if st.fails >= 5 {
                UIA_VALID.store(false, Ordering::Relaxed);
            }
            if logging {
                log.insert(0, format!("mode=buttons:{} FAIL n={}", st.buttons_only, st.fails));
            }
        }
    }
    if logging {
        log_dump(&log);
    }
}

/// 一轮定位：经典条件找不到时降级全子树；候选取"最右"者（时钟在任务栏最右端）
unsafe fn locate(
    uia: *mut c_void,
    st: &mut ScanState,
    log: &mut Vec<String>,
) -> Option<RECT> {
    let tray = FindWindowExW(
        null_mut(),
        null_mut(),
        crate::wide("Shell_TrayWnd").as_ptr(),
        std::ptr::null(),
    );
    if tray.is_null() || IsWindowVisible(tray) == 0 {
        return None;
    }
    let taskbar = element_from_handle(uia, tray)?;
    let mut best: Option<RECT> = None;
    // 回试轮：降级状态下按钮快路径周期性重试。回试轮按钮未命中时同轮仍走
    // 全子树兜底，不产生空轮/丢矩形。
    let mut probe = false;
    if !st.buttons_only && st.fulltree_runs >= FULLTREE_PROBE_AT {
        probe = true;
        st.fulltree_runs = 0;
    }
    if st.buttons_only || probe {
        let cond = property_condition(uia, UIA_CONTROL_TYPE, UIA_BUTTON_CONTROL_TYPE);
        best = scan(taskbar, cond, log);
        com_release(cond);
        if best.is_some() {
            st.button_misses = 0;
            if probe {
                st.buttons_only = true; // 回试成功：恢复按钮快路径
            }
        } else {
            st.button_misses += 1;
            if st.button_misses >= 3 {
                st.buttons_only = false;
            }
        }
    }
    if best.is_none() && (!st.buttons_only || probe) {
        let cond = true_condition(uia);
        best = scan(taskbar, cond, log);
        com_release(cond);
        if best.is_some() {
            st.fulltree_runs += 1;
        }
    }
    com_release(taskbar);
    best
}

unsafe fn element_from_handle(
    uia: *mut c_void,
    hwnd: HWND,
) -> Option<*mut c_void> {
    let mut e: *mut c_void = null_mut();
    let f: ElementFromHandleFn = std::mem::transmute(slot(uia, SLOT_ELEMENT_FROM_HANDLE));
    if f(uia, hwnd, &mut e) == 0 && !e.is_null() {
        Some(e)
    } else {
        None
    }
}

unsafe fn property_condition(
    uia: *mut c_void,
    id: i32,
    value: i32,
) -> *mut c_void {
    let mut c: *mut c_void = null_mut();
    let f: CreatePropertyConditionFn =
        std::mem::transmute(slot(uia, SLOT_CREATE_PROPERTY_CONDITION));
    f(uia, id, Variant::i4(value), &mut c);
    c
}

unsafe fn true_condition(uia: *mut c_void) -> *mut c_void {
    let mut c: *mut c_void = null_mut();
    let f: CreateTrueConditionFn = std::mem::transmute(slot(uia, SLOT_CREATE_TRUE_CONDITION));
    f(uia, &mut c);
    c
}

/// 遍历 taskbar 子树中满足 cond 的元素，找 Name 像时钟文本的，取最右者矩形
unsafe fn scan(
    taskbar: *mut c_void,
    cond: *mut c_void,
    log: &mut Vec<String>,
) -> Option<RECT> {
    if cond.is_null() {
        return None;
    }
    let mut arr: *mut c_void = null_mut();
    let find_all: FindAllFn = std::mem::transmute(slot(taskbar, SLOT_FIND_ALL));
    if find_all(taskbar, TREE_SCOPE_DESCENDANTS, cond, &mut arr) != 0 || arr.is_null() {
        return None;
    }
    let len_fn: ArrayLenFn = std::mem::transmute(slot(arr, SLOT_ARRAY_LENGTH));
    let get_fn: ArrayGetFn = std::mem::transmute(slot(arr, SLOT_ARRAY_GET_ELEMENT));
    let mut n: i32 = 0;
    if len_fn(arr, &mut n) != 0 {
        n = 0;
    }
    let mut best: Option<RECT> = None;
    let mut logged = 0;
    let mut i = 0;
    while i < n {
        let mut elem: *mut c_void = null_mut();
        if get_fn(arr, i, &mut elem) == 0 && !elem.is_null() {
            if let Some(name) = element_name(elem) {
                if looks_like_clock_name(&name) {
                    if let Some(r) = element_rect(elem) {
                        // 候选通常只有一两个；调试时全量收集，非调试轮次直接丢弃
                        if logged < 20 {
                            log.push(format!("  cand {} ({},{})-({},{})", name, r.left, r.top, r.right, r.bottom));
                            logged += 1;
                        }
                        let better = match best {
                            None => true,
                            Some(b) => {
                                r.right > b.right
                                    || (r.right == b.right
                                        && r.right - r.left > b.right - b.left)
                            }
                        };
                        if better {
                            best = Some(r);
                        }
                    }
                }
            }
            com_release(elem);
        }
        i += 1;
    }
    com_release(arr);
    best
}

/// 调试：CAL_UIA_LOG=1 时把每轮扫描结果写到 %APPDATA%\z-calendar\uia.log
fn log_dump(lines: &[String]) {
    use std::io::Write;
    let path = crate::config::data_dir().join("uia.log");
    if let Ok(mut f) = std::fs::File::create(path) {
        for l in lines {
            let _ = writeln!(f, "{}", l);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winapi::um::winuser::GetWindowRect;

    #[test]
    fn clock_name_patterns() {
        // 中文/英文/带 AM-PM/只有时间/全角冒号都应命中
        assert!(looks_like_clock_name("2026/10/4 10:04"));
        assert!(looks_like_clock_name("10:04 AM 10/4/2026"));
        assert!(looks_like_clock_name("10/4/2026 10:04 AM"));
        assert!(looks_like_clock_name("上午 10:04"));
        assert!(looks_like_clock_name("10：04"));
        assert!(looks_like_clock_name("10:04"));
        assert!(looks_like_clock_name("0:05"));
        assert!(looks_like_clock_name("10:04:05 2026/10/4"));
        // 非时钟文本不命中
        assert!(!looks_like_clock_name(""));
        assert!(!looks_like_clock_name("开始"));
        assert!(!looks_like_clock_name("设置"));
        assert!(!looks_like_clock_name("Microsoft Edge"));
        assert!(!looks_like_clock_name("任务视图"));
    }

    /// 活机测试：本机（Win10/Win11 均可）UIA 扫描应能在任务栏上找到时钟区域。
    /// 需要桌面会话（CI 无桌面时跳过）。
    #[test]
    fn uia_scan_locates_clock() {
        unsafe {
            let tray = FindWindowExW(
                null_mut(),
                null_mut(),
                crate::wide("Shell_TrayWnd").as_ptr(),
                std::ptr::null(),
            );
            if tray.is_null() {
                eprintln!("no taskbar, skip");
                return;
            }
            let hr = CoInitializeEx(null_mut(), COINIT_MULTITHREADED);
            assert!(hr >= 0, "CoInitializeEx failed: {hr}");
            let clsid = CLSID_CUIAUTOMATION;
            let iid = IID_IUIAUTOMATION;
            let mut uia: *mut c_void = null_mut();
            let ihr = CoCreateInstance(&clsid, null_mut(), CLSCTX_INPROC_SERVER, &iid, &mut uia);
            if ihr != 0 || uia.is_null() {
                CoUninitialize();
                panic!("CoCreateInstance(CUIAutomation) failed: hr=0x{:08x}", ihr);
            }
            let mut st = ScanState { buttons_only: true, button_misses: 0, fulltree_runs: 0, fails: 0 };
            let mut log: Vec<String> = Vec::new();
            let r = locate(uia, &mut st, &mut log);
            com_release(uia);
            CoUninitialize();
            match r {
                Some(rect) => {
                    // 找到的矩形应与任务栏窗口相交
                    let mut tr: RECT = std::mem::zeroed();
                    assert_ne!(GetWindowRect(tray, &mut tr), 0);
                    assert!(
                        rect.left < tr.right && rect.right > tr.left,
                        "clock rect ({},{})-({},{}) not over taskbar ({},{})-({},{})",
                        rect.left, rect.top, rect.right, rect.bottom,
                        tr.left, tr.top, tr.right, tr.bottom
                    );
                    assert!(rect.right > rect.left && rect.bottom > rect.top);
                    eprintln!(
                        "clock rect ({},{})-({},{}) {}x{} in taskbar ({},{})-({},{})",
                        rect.left, rect.top, rect.right, rect.bottom,
                        rect.right - rect.left, rect.bottom - rect.top,
                        tr.left, tr.top, tr.right, tr.bottom
                    );
                }
                None => panic!(
                    "UIA scan found no clock text on taskbar; log: {:?}",
                    log
                ),
            }
        }
    }
}
