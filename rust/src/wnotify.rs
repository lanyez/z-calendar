//! Windows 通知中心集成：提醒走系统 Toast（Win10/11 通知中心可见、不再错过）。
//!
//! 组成：
//! - 进程 AUMID + 开始菜单快捷方式（带 PKEY_AppUserModel_ID 属性）——
//!   非打包 Win32 应用显示 Toast 的身份前提（快捷方式缺失/过期时自动重建）；
//! - WinRT ToastNotificationManager（windows crate，静态链接）发送通知；
//! - 通知按钮（完成/稍后）以 `zcal:` 前缀参数回启应用，经单实例 IPC 转发给
//!   常驻实例处理（dispatch）。
//! 任一环节失败（老系统/权限）返回 false，提醒自动回退到内置提醒卡片。

use std::sync::atomic::{AtomicU8, Ordering};

/// AUMID（应用用户模型 ID）：与快捷方式属性保持一致
const AUMID: &str = "ZCalendar.Taskbar";
/// 开始菜单快捷方式文件名（相对 %APPDATA%\Microsoft\Windows\Start Menu\Programs）
const LNK_NAME: &str = "Z日历.lnk";

static READY: AtomicU8 = AtomicU8::new(0); // 0=未初始化 1=就绪 2=不可用
/// 调试：最近一次失败步骤（0=成功 1=AUMID 2=exe路径 3=CoCreate 4=SetPath/Desc
/// 5=QI PersistFile 6=Load 7=QI PropertyStore 8=AUMID属性 9=Save 10=目标比对失败）
static LAST_FAIL: AtomicU8 = AtomicU8::new(0);
static LAST_HR: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// 调试：最近一次失败步骤码
pub fn last_fail() -> u8 {
    LAST_FAIL.load(Ordering::Relaxed)
}

/// 调试：最近一次失败的 HRESULT（步骤 8/9）
pub fn last_hr() -> i64 {
    LAST_HR.load(Ordering::Relaxed)
}

fn shortcuts_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(std::path::PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs"))
}

fn lnk_path() -> Option<std::path::PathBuf> {
    shortcuts_dir().map(|d| d.join(LNK_NAME))
}

/// 调试：开始菜单快捷方式是否已存在
pub fn lnk_exists() -> bool {
    lnk_path().map(|p| p.exists()).unwrap_or(false)
}

/// 通知激活参数的进程间转发文件（第二实例写入，常驻实例消费）
pub fn ipc_path() -> std::path::PathBuf {
    crate::config::data_dir().join("toast_ipc.txt")
}

/// 提醒线程等无 COM 上下文的线程调用一次
pub fn com_init() {
    use winapi::um::combaseapi::CoInitializeEx;
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    DONE.with(|d| {
        if !d.get() {
            unsafe {
                // S_FALSE（已初始化）与换模式失败都容忍：后续调用尽力而为
                let _ = CoInitializeEx(std::ptr::null_mut(), winapi::um::objbase::COINIT_MULTITHREADED as u32);
            }
            d.set(true);
        }
    });
}

/// 确保通知身份就绪（AUMID + 快捷方式）。幂等；失败置不可用（本次进程内不再尝试）
pub fn ensure_ready() -> bool {
    match READY.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let ok = (|| -> bool {
        com_init();
        unsafe {
            // 进程 AUMID（每次进程启动设置一次）
            let aumid: Vec<u16> = AUMID.encode_utf16().chain(std::iter::once(0)).collect();
            if set_current_process_aumid(&aumid) != 0 {
                LAST_FAIL.store(1, Ordering::Relaxed);
                return false;
            }
            // 快捷方式：存在且指向当前 exe 即可；否则创建/刷新
            let Some(path) = lnk_path() else { return false };
            let exe = match std::env::current_exe() {
                Ok(e) => e,
                Err(_) => {
                    LAST_FAIL.store(2, Ordering::Relaxed);
                    return false;
                }
            };
            if path.exists() && shortcut_target_is(&path, &exe) {
                return true;
            }
            if !create_shortcut(&path, &exe) {
                if LAST_FAIL.load(Ordering::Relaxed) == 0 {
                    LAST_FAIL.store(9, Ordering::Relaxed);
                }
                return false;
            }
            true
        }
    })();
    READY.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
    ok
}

#[link(name = "shell32")]
extern "system" {
    fn SetCurrentProcessExplicitAppUserModelID(appid: *const u16) -> i32;
}

unsafe fn set_current_process_aumid(aumid: &[u16]) -> i32 {
    unsafe { SetCurrentProcessExplicitAppUserModelID(aumid.as_ptr()) }
}

// ---------------- GUID / COM 声明（最小集） ----------------

type Guid = winapi::shared::guiddef::GUID;

const fn guid(data1: u32, data2: u16, data3: u16, b: [u8; 8]) -> Guid {
    winapi::shared::guiddef::GUID { Data1: data1, Data2: data2, Data3: data3, Data4: b }
}

const CLSID_SHELL_LINK: Guid = guid(0x00021401, 0x0000, 0x0000, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
const IID_ISHELL_LINK_W: Guid = guid(0x000214F9, 0x0000, 0x0000, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
const IID_IPERSIST_FILE: Guid = guid(0x0000010B, 0x0000, 0x0000, [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
const IID_IPROPERTY_STORE: Guid = guid(0x886D8EEB, 0x8CF2, 0x4446, [0x8D, 0x02, 0xCD, 0xBA, 0x1D, 0xBD, 0xCF, 0x99]);
/// PKEY_AppUserModel_ID
const PKEY_AUMID: PropertyKey = PropertyKey {
    fmtid: guid(0x9F4C2855, 0x9F79, 0x4B39, [0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3]),
    pid: 5,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct PropertyKey {
    fmtid: Guid,
    pid: u32,
}

/// PROPVARIANT 最小布局（x64：16 字节，VT_LPWSTR 时 union 处为 PWCHAR 指针）
#[repr(C)]
struct PropVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    ptr: usize,
}

const VT_LPWSTR: u16 = 31;

type QiFn = unsafe extern "system" fn(usize, *const Guid, *mut usize) -> i32;
type RefFn = unsafe extern "system" fn(usize) -> u32;
type SetTextFn = unsafe extern "system" fn(usize, *const u16) -> i32;
type GetPathFn = unsafe extern "system" fn(usize, *mut u16, i32, usize, u32) -> i32;
type LoadFn = unsafe extern "system" fn(usize, *const u16, u32) -> i32;
type SaveFn = unsafe extern "system" fn(usize, *const u16, i32) -> i32;
type GetCountFn = unsafe extern "system" fn(usize, *mut u32) -> i32;
type GetAtFn = unsafe extern "system" fn(usize, u32, *mut PropertyKey) -> i32;
type GetValueFn = unsafe extern "system" fn(usize, *const PropertyKey, *mut PropVariant) -> i32;
type SetValueFn = unsafe extern "system" fn(usize, *const PropertyKey, *const PropVariant) -> i32;
type CommitFn = unsafe extern "system" fn(usize) -> i32;

/// IShellLinkW vtable（只用 GetPath/SetDescription/SetPath，其余槽位占位保持偏移）
#[repr(C)]
struct ShellLinkVtbl {
    qi: QiFn,
    add_ref: RefFn,
    release: RefFn,
    get_path: GetPathFn,
    get_id_list: usize,
    set_id_list: usize,
    get_description: GetTextFn2,
    set_description: SetTextFn,
    get_working_directory: usize,
    set_working_directory: usize,
    get_arguments: usize,
    set_arguments: usize,
    get_hotkey: usize,
    set_hotkey: usize,
    get_show_cmd: usize,
    set_show_cmd: usize,
    get_icon_location: usize,
    set_icon_location: usize,
    set_relative_path: usize,
    resolve: usize,
    set_path: SetTextFn,
}

type GetTextFn2 = unsafe extern "system" fn(usize, *mut u16, i32) -> i32;

/// IPersistFile vtable（继承 IPersist：IUnknown + GetClassID；只用 Load/Save）
#[repr(C)]
struct PersistFileVtbl {
    qi: QiFn,
    add_ref: RefFn,
    release: RefFn,
    get_class_id: GetClassIdFn,
    is_dirty: usize,
    load: LoadFn,
    save: SaveFn,
    save_completed: usize,
    get_cur_file: usize,
}

type GetClassIdFn = unsafe extern "system" fn(usize, *mut Guid) -> i32;

/// IPropertyStore vtable（只用 SetValue/Commit）
#[repr(C)]
struct PropertyStoreVtbl {
    qi: QiFn,
    add_ref: RefFn,
    release: RefFn,
    get_count: GetCountFn,
    get_at: GetAtFn,
    get_value: GetValueFn,
    set_value: SetValueFn,
    commit: CommitFn,
}

unsafe fn vtbl_of(obj: usize) -> usize {
    unsafe { *(obj as *const usize) }
}

/// 读取快捷方式目标路径（比对是否指向当前 exe，避免每次启动重写文件）
unsafe fn shortcut_target_is(path: &std::path::Path, exe: &std::path::Path) -> bool {
    unsafe {
        let mut obj: usize = 0;
        let hr = CoCreateInstance(&CLSID_SHELL_LINK, 0, CLSCTX_INPROC_SERVER, &IID_ISHELL_LINK_W, &mut obj);
        if hr != 0 || obj == 0 {
            LAST_FAIL.store(3, Ordering::Relaxed);
            return false;
        }
        let mut ok = false;
        let vt = vtbl_of(obj) as *const ShellLinkVtbl;
        // Load
        let pf = qi_persist_file(obj);
        if pf != 0 {
            let pvt = vtbl_of(pf) as *const PersistFileVtbl;
            let wpath: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            if ((*pvt).load)(pf, wpath.as_ptr(), 0 /*STGM_READ*/) == 0 {
                let mut buf = [0u16; 520];
                if ((*vt).get_path)(obj, buf.as_mut_ptr(), 520, 0, 0) == 0 {
                    let len = buf.iter().position(|c| *c == 0).unwrap_or(0);
                    let target = String::from_utf16_lossy(&buf[..len]);
                    let cur = exe.to_string_lossy().to_string();
                    ok = target.replace('/', "\\").eq_ignore_ascii_case(&cur.replace('/', "\\"));
                }
            }
            ((* (vtbl_of(pf) as *const PersistFileVtbl)).release)(pf);
        }
        ((*vt).release)(obj);
        ok
    }
}

const CLSCTX_INPROC_SERVER: u32 = 1;

#[link(name = "ole32")]
extern "system" {
    fn CoCreateInstance(clsid: *const Guid, outer: usize, ctx: u32, iid: *const Guid, out: *mut usize) -> i32;
}

/// QI 到 IPersistFile
unsafe fn qi_persist_file(obj: usize) -> usize {
    unsafe {
        let vt = vtbl_of(obj) as *const ShellLinkVtbl;
        let mut out: usize = 0;
        if ((*vt).qi)(obj, &IID_IPERSIST_FILE, &mut out) == 0 {
            out
        } else {
            0
        }
    }
}

/// 创建/刷新带 AUMID 属性的开始菜单快捷方式
unsafe fn create_shortcut(path: &std::path::Path, exe: &std::path::Path) -> bool {
    unsafe {
        let mut obj: usize = 0;
        let hr = CoCreateInstance(&CLSID_SHELL_LINK, 0, CLSCTX_INPROC_SERVER, &IID_ISHELL_LINK_W, &mut obj);
        if hr != 0 || obj == 0 {
            return false;
        }
        let mut ok = false;
        let vt = vtbl_of(obj) as *const ShellLinkVtbl;
        let wexe: Vec<u16> = exe.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let desc: Vec<u16> = "Z日历（任务栏日历）".encode_utf16().chain(std::iter::once(0)).collect();
        if ((*vt).set_path)(obj, wexe.as_ptr()) != 0 || ((*vt).set_description)(obj, desc.as_ptr()) != 0 {
            LAST_FAIL.store(4, Ordering::Relaxed);
        } else {
            let pf = qi_persist_file(obj);
            if pf == 0 {
                LAST_FAIL.store(5, Ordering::Relaxed);
            } else {
                let pvt = vtbl_of(pf) as *const PersistFileVtbl;
                let mut ps_local: usize = 0;
                if ((*vt).qi)(obj, &IID_IPROPERTY_STORE, &mut ps_local) != 0 || ps_local == 0 {
                    LAST_FAIL.store(7, Ordering::Relaxed);
                } else {
                    let psvt = vtbl_of(ps_local) as *const PropertyStoreVtbl;
                    // CoTaskMemAlloc 分配的宽字符串（进程内一次性写入，几十字节不释放）
                    let aumid: Vec<u16> = AUMID.encode_utf16().chain(std::iter::once(0)).collect();
                    let buf = CoTaskMemAlloc((aumid.len() * 2) as usize) as *mut u16;
                    if !buf.is_null() {
                        std::ptr::copy_nonoverlapping(aumid.as_ptr(), buf, aumid.len());
                        let pv = PropVariant { vt: VT_LPWSTR, r1: 0, r2: 0, r3: 0, ptr: buf as usize };
                        let hr_set = ((*psvt).set_value)(ps_local, &PKEY_AUMID, &pv);
                        let hr_commit = if hr_set == 0 { ((*psvt).commit)(ps_local) } else { -1 };
                        if hr_set != 0 || hr_commit != 0 {
                            LAST_FAIL.store(8, Ordering::Relaxed);
                            LAST_HR.store(hr_set as i64 * 100000 + hr_commit as i64, Ordering::Relaxed);
                        } else {
                            let wpath: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                            if let Some(dir) = path.parent() {
                                let _ = std::fs::create_dir_all(dir);
                            }
                            let hr_save = ((*pvt).save)(pf, wpath.as_ptr(), -1i32);
                            ok = hr_save == 0;
                            if !ok {
                                LAST_FAIL.store(9, Ordering::Relaxed);
                                LAST_HR.store(hr_save as i64, Ordering::Relaxed);
                            }
                        }
                    }
                    ((*psvt).release)(ps_local);
                }
                ((*pvt).release)(pf);
            }
        }
        ((*vt).release)(obj);
        ok
    }
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemAlloc(size: usize) -> *mut winapi::ctypes::c_void;
}

// ---------------- Toast 发送（WinRT） ----------------

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn uri_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 稍后提醒的参数载荷（无空格，便于命令行传递）
fn snooze_args(title: &str, body: &str, act: &crate::toast::Act) -> String {
    let act_json = serde_json::to_string(act).unwrap_or_default();
    format!(
        "zcal:snooze:{}|{}|{}",
        uri_encode(title),
        uri_encode(body),
        uri_encode(&act_json)
    )
}

/// 发送一条提醒到系统通知中心。返回 false 时调用方回退到内置提醒卡片。
/// act 为待办完成动作时带「完成」按钮；始终带「稍后10分钟」。
pub fn show_reminder(title: &str, body: &str, act: &crate::toast::Act, allow_sound: bool) -> bool {
    if !ensure_ready() {
        return false;
    }
    // 按钮组
    let mut buttons: Vec<(String, String)> = Vec::new();
    if let crate::toast::Act::TodoDone { id, date, .. } = act {
        buttons.push(("完成".into(), format!("zcal:todo-done:{}|{}", id, date)));
    }
    buttons.push(("稍后10分钟".into(), snooze_args(title, body, act)));
    let mut xml = String::from(
        "<toast activationType=\"foreground\" launch=\"zcal:default\"><visual><binding template=\"ToastGeneric\">",
    );
    xml.push_str(&format!("<text>{}</text>", xml_escape(title)));
    if !body.is_empty() {
        xml.push_str(&format!("<text>{}</text>", xml_escape(body)));
    }
    xml.push_str("</binding></visual>");
    if !allow_sound {
        xml.push_str("<audio silent=\"true\"/>");
    }
    xml.push_str("<actions>");
    for (label, args) in &buttons {
        xml.push_str(&format!(
            "<action content=\"{}\" arguments=\"{}\" activationType=\"foreground\"/>",
            xml_escape(label),
            xml_escape(args)
        ));
    }
    xml.push_str("</actions></toast>");

    let res = (|| -> windows::core::Result<()> {
        use windows::core::HSTRING;
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
        com_init();
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        // 非打包应用：按 AUMID 建 Notifier 发送（需开始菜单快捷方式身份，
        // 与官方 ToastNotificationManagerCompat 同款路径）
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
        notifier.Show(&toast)?;
        Ok(())
    })();
    res.is_ok()
}

/// 通知激活参数分发（按钮/正文点击回启应用后调用）：
/// zcal:default → 弹出日历；zcal:todo-done:id|date → 待办标记完成；
/// zcal:snooze:title|body|act → 登记 10 分钟后补提。
pub fn dispatch(args: &str) {
    for a in args.lines() {
        let a = a.trim();
        if let Some(rest) = a.strip_prefix("zcal:todo-done:") {
            let mut it = rest.splitn(2, '|');
            let id = it.next().unwrap_or("");
            let date = it.next().unwrap_or("");
            crate::sidebar::complete_todo_by_id(id, date);
            crate::sidebar::sidebar_repaint();
        } else if let Some(rest) = a.strip_prefix("zcal:snooze:") {
            let mut it = rest.splitn(3, '|');
            let title = uri_decode(it.next().unwrap_or(""));
            let body = uri_decode(it.next().unwrap_or(""));
            let act_json = uri_decode(it.next().unwrap_or(""));
            let act: crate::toast::Act = serde_json::from_str(&act_json).unwrap_or(crate::toast::Act::None);
            let t = crate::toast::now_ms_pub() + crate::toast::SNOOZE_MS_PUB;
            crate::toast::snooze_add_entry(crate::toast::SnoozeEntry { t, title, body, act });
        } else if a.contains("zcal:default") {
            crate::flyout::request_show();
        }
    }
}
