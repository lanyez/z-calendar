# 验证：新增日程/待办完整表单弹窗
# 1) 打开弹窗时主日历保持显示
# 2) 待办表单：内容/优先级图标/时间开关/开始结束/提醒/重复
# 3) 日程表单：名称/全天/开始结束/提醒/重复
# 测试数据写入后自动清理
import ctypes, time, subprocess, re, json, os
from ctypes import wintypes
user32 = ctypes.windll.user32
def wstr(s2): return ctypes.create_unicode_buffer(s2)

def find_class(cls_name):
    res = []
    WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    class RECT(ctypes.Structure):
        _fields_ = [('l', ctypes.c_long), ('t', ctypes.c_long), ('r', ctypes.c_long), ('b', ctypes.c_long)]
    def cb(h, lp):
        p = wintypes.DWORD(); user32.GetWindowThreadProcessId(h, ctypes.byref(p))
        if p.value == pid:
            b = ctypes.create_unicode_buffer(256); user32.GetClassNameW(h, b, 256)
            if b.value == cls_name and user32.IsWindowVisible(h):
                r = RECT(); user32.GetWindowRect(h, ctypes.byref(r)); res.append((h, (r.l, r.t, r.r, r.b)))
        return True
    user32.EnumWindows(WNDENUMPROC(cb), 0)
    return res

def clock_rect():
    tray = user32.FindWindowExW(None, None, wstr('Shell_TrayWnd'), None)
    tn = user32.FindWindowExW(tray, None, wstr('TrayNotifyWnd'), None)
    clk = user32.FindWindowExW(tn, None, wstr('TrayClockWClass'), None)
    r = wintypes.RECT()
    user32.GetWindowRect(clk, ctypes.byref(r))
    return (r.left, r.top, r.right, r.bottom)

def click(x, y, right=False):
    user32.SetCursorPos(x, y); time.sleep(0.25)
    down, up = (8, 16) if right else (2, 4)
    user32.mouse_event(down, 0, 0, 0, 0); time.sleep(0.05); user32.mouse_event(up, 0, 0, 0, 0)
    time.sleep(0.55)

def type_text(hwnd, s):
    for ch in s:
        user32.PostMessageW(hwnd, 0x0102, ord(ch), 0)  # WM_CHAR
    time.sleep(0.3)

ok = fail = 0
def check(name, cond):
    global ok, fail
    print(('PASS' if cond else 'FAIL'), name)
    ok += 1 if cond else 0
    fail += 0 if cond else 1

out = subprocess.run(['tasklist', '/FI', 'IMAGENAME eq z-calendar.exe', '/FO', 'CSV'],
                     capture_output=True).stdout.decode('gbk', 'ignore')
pid = int(re.search(r'"(\d+)"', [l for l in out.splitlines() if 'z-calendar' in l][0]).group(1))

crect = clock_rect()
cx, cy = (crect[0] + crect[2]) // 2, (crect[1] + crect[3]) // 2

data_dir = os.path.expandvars(r'%APPDATA%\z-calendar')

def open_dialog(right_click_xy, item_dy):
    """右键日期格打开菜单并点击第 item_dy 行菜单项"""
    click(*right_click_xy, right=True)
    dm = find_class('z-calendar-datemenu')
    assert dm, 'date menu missing'
    h, (l, t, r, b) = dm[0]
    click(l + 93, t + 4 + item_dy * 34 + 17)

# 打开日历
if not find_class('z-calendar-main'):
    click(cx, cy)
check('日历已打开', bool(find_class('z-calendar-main')))
main = find_class('z-calendar-main')
L, T = main[0][1][0], main[0][1][1]
dx, dy = L + 196, T + 234

# ===== 场景A：新增日程 =====
open_dialog((dx, dy), 0)
dlg = find_class('z-calendar-input')
check('日程弹窗打开', bool(dlg))
check('主日历保持显示(需求1)', bool(find_class('z-calendar-main')))
if dlg:
    h, (l, t, r, b) = dlg[0]
    type_text(h, '表单测试日程A')
    click(l + 330, t + 119)   # 全天开关
    time.sleep(0.3)
    click(l + 200, t + 243)   # 提醒行 → 下拉
    time.sleep(0.3)
    click(l + 250, t + 222)   # 选择 5分钟前(锚定钳制后第3项)
    time.sleep(0.3)
    click(l + 200, t + 277)   # 重复行 → 下拉
    time.sleep(0.3)
    click(l + 250, t + 267)   # 选择 每15分钟
    time.sleep(0.3)
    click(l + 200, t + 327)   # 保存
    time.sleep(0.5)
    check('保存后弹窗关闭', not find_class('z-calendar-input'))
    p = os.path.join(data_dir, 'agenda.json')
    data = json.load(open(p, encoding='utf-8')) if os.path.exists(p) else {}
    found = None
    for v in data.values():
        for it in v:
            if isinstance(it, dict) and it.get('name') == '表单测试日程A':
                found = it
    check('日程富条目已写入', found is not None)
    if found:
        check('全天=true', found.get('all_day') is True)
        check('提醒=5分钟前', found.get('remind') == 5)
        check('重复=每15分钟', found.get('repeat') == 15)
        check('全天条目无时间部分', found.get('start', '').find(' ') < 0)

# ===== 场景B：新增待办 =====
open_dialog((dx, dy), 1)
dlg = find_class('z-calendar-input')
check('待办弹窗打开', bool(dlg))
check('主日历仍显示', bool(find_class('z-calendar-main')))
if dlg:
    h, (l, t, r, b) = dlg[0]
    check('时间关时弹窗高度较矮', (b - t) < 400)
    type_text(h, '表单测试待办B')
    click(l + 150, t + 161)   # 优先级 Ⅰ 重要且紧急
    time.sleep(0.3)
    click(l + 330, t + 273)   # 时间开关
    time.sleep(0.5)
    dlg2 = find_class('z-calendar-input')
    h2, (l2, t2, r2, b2) = dlg2[0]
    check('时间开时弹窗变高', (b2 - t2) > 450)
    click(l2 + 200, t2 + 380)  # 提醒行
    time.sleep(0.3)
    click(l2 + 250, t2 + 345)  # 5分钟前
    time.sleep(0.3)
    click(l2 + 200, t2 + 414)  # 重复行
    time.sleep(0.3)
    click(l2 + 250, t2 + 405)  # 每15分钟
    time.sleep(0.3)
    click(l2 + 238, t2 + 464)  # 打开开始时间选择面板 → 完成
    time.sleep(0.3)
    click(l2 + 238, t2 + 464)
    time.sleep(0.3)
    check('选择面板已收起', find_class('z-calendar-input'))
    click(l2 + 200, t2 + 465)  # 保存
    time.sleep(0.5)
    p = os.path.join(data_dir, 'todo.json')
    data = json.load(open(p, encoding='utf-8')) if os.path.exists(p) else []
    found = next((x for x in data if x.get('text') == '表单测试待办B'), None)
    check('待办已写入', found is not None)
    if found:
        check('优先级=Ⅰ(1)', found.get('priority') == 1)
        check('有时间', found.get('has_time') is True)
        check('开始/结束时间存在', bool(found.get('start')) and bool(found.get('end')))
        check('提醒=5', found.get('remind') == 5)
        check('重复=15', found.get('repeat') == 15)

# ===== 场景C：Esc 取消 =====
open_dialog((dx, dy), 0)
dlg = find_class('z-calendar-input')
if dlg:
    h = dlg[0][0]
    user32.PostMessageW(h, 0x0100, 0x1B, 0)  # WM_KEYDOWN ESC
    time.sleep(0.5)
check('Esc 取消关闭弹窗', not find_class('z-calendar-input'))

# ===== 清理测试数据 =====
p = os.path.join(data_dir, 'agenda.json')
if os.path.exists(p):
    data = json.load(open(p, encoding='utf-8'))
    changed = False
    for k in list(data.keys()):
        n0 = len(data[k])
        data[k] = [x for x in data[k] if (isinstance(x, str) and x != '表单测试日程A') and not (isinstance(x, dict) and x.get('name') == '表单测试日程A')]
        if len(data[k]) != n0:
            changed = True
        if not data[k]:
            del data[k]
    if changed:
        json.dump(data, open(p, 'w', encoding='utf-8'), ensure_ascii=False)
p = os.path.join(data_dir, 'todo.json')
if os.path.exists(p):
    data = json.load(open(p, encoding='utf-8'))
    n0 = len(data)
    data = [x for x in data if x.get('text') != '表单测试待办B']
    if len(data) != n0:
        json.dump(data, open(p, 'w', encoding='utf-8'), ensure_ascii=False, indent=2)

print(f'--- {ok} passed, {fail} failed')
exit(1 if fail else 0)
