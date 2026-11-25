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
    user32.SetCursorPos(x, y); time.sleep(0.3)
    down, up = (8, 16) if right else (2, 4)
    user32.mouse_event(down, 0, 0, 0, 0); time.sleep(0.05); user32.mouse_event(up, 0, 0, 0, 0)
    time.sleep(0.6)

def agenda_data():
    p = os.path.expandvars(r'%APPDATA%\z-calendar\agenda.json')
    if os.path.exists(p):
        return json.load(open(p, encoding='utf-8'))
    return {}

def todo_data():
    p = os.path.expandvars(r'%APPDATA%\z-calendar\todo.json')
    if os.path.exists(p):
        return json.load(open(p, encoding='utf-8'))
    return []

out = subprocess.run(['tasklist', '/FI', 'IMAGENAME eq z-calendar.exe', '/FO', 'CSV'],
                     capture_output=True).stdout.decode('gbk', 'ignore')
pid = int(re.search(r'"(\d+)"', [l for l in out.splitlines() if 'z-calendar' in l][0]).group(1))

crect = clock_rect()
cx, cy = (crect[0] + crect[2]) // 2, (crect[1] + crect[3]) // 2

# 唤醒/切换到打开状态
main = find_class('z-calendar-main')
if not main:
    click(cx, cy)
    main = find_class('z-calendar-main')
print('日历可见:', bool(main))
assert main, 'calendar not open'

main_rect = main[0][1]
L, T = main_rect[0], main_rect[1]
dx, dy = L + 196, T + 234  # 第2行第3列日期格

# 右键日期格 → 菜单
click(dx, dy, right=True)
dm = find_class('z-calendar-datemenu')
print('日期右键菜单弹出:', bool(dm), dm[:1] if dm else '')
assert dm, 'date menu missing'

# 点击“新增日程”
h, (l, t, r, b) = dm[0]
click(l + 93, t + 4 + 17)
ib = find_class('z-calendar-input')
print('输入框弹出:', bool(ib))
before = agenda_data()
assert ib, 'inputbox missing'
ih = ib[0][0]
for ch in '测试日程123':
    user32.PostMessageW(ih, 0x0102, ord(ch), 0)  # WM_CHAR
time.sleep(0.2)
user32.PostMessageW(ih, 0x0100, 0x0D, 0)  # Enter
time.sleep(0.6)
after = agenda_data()
key = '2026-9-20'
print('日程已写入:', after.get(key), '| 之前:', before.get(key))

# 再次右键同一日期 → 新增待办
click(dx, dy, right=True)
dm = find_class('z-calendar-datemenu')
print('再次右键菜单(切换重开):', bool(dm))
assert dm, 'date menu missing again'
h, (l, t, r, b) = dm[0]
click(l + 93, t + 4 + 34 + 17)  # 新增待办
ib = find_class('z-calendar-input')
print('待办输入框弹出:', bool(ib))
assert ib, 'todo inputbox missing'
ih = ib[0][0]
for ch in '测试待办ABC':
    user32.PostMessageW(ih, 0x0102, ord(ch), 0)
time.sleep(0.2)
user32.PostMessageW(ih, 0x0100, 0x0D, 0)
time.sleep(0.6)
todos = todo_data()
print('待办已写入:', todos[-1] if todos else '无')
