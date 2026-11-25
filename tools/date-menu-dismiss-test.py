# 验证：日期右键菜单打开后，左键点击任意位置（菜单外/菜单空白处/菜单项）都能关闭菜单
import ctypes, time, subprocess, re
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

# 打开日历
if not find_class('z-calendar-main'):
    click(cx, cy)
main = find_class('z-calendar-main')
check('日历已打开', bool(main))
L, T = main[0][1][0], main[0][1][1]
dx, dy = L + 196, T + 234  # 日期格

# 场景1：左键点击任务栏空白处（日历外）关闭
click(dx, dy, right=True)
check('右键弹出日期菜单', bool(find_class('z-calendar-datemenu')))
click(crect[0] - 150, cy)
check('左键点任务栏空白处 → 菜单关闭', not find_class('z-calendar-datemenu'))

# 场景2：左键点击日历内另一处（菜单外）关闭
click(dx, dy, right=True)
dm = find_class('z-calendar-datemenu')
check('再次右键弹出菜单', bool(dm))
click(L + 100, T + 40)  # 标题区
check('左键点日历标题区 → 菜单关闭', not find_class('z-calendar-datemenu'))

# 场景3：左键点击菜单空白处（未命中条目）关闭
click(dx, dy, right=True)
dm = find_class('z-calendar-datemenu')
check('第三次右键弹出菜单', bool(dm))
if dm:
    h, (l, t, r, b) = dm[0]
    click(l + 2, t + 40)  # 菜单左侧 6px 空白边
    check('左键点菜单空白边 → 菜单关闭', not find_class('z-calendar-datemenu'))

# 场景4：左键点击菜单项 → 打开输入框且菜单关闭；ESC 取消输入框
click(dx, dy, right=True)
dm = find_class('z-calendar-datemenu')
check('第四次右键弹出菜单', bool(dm))
if dm:
    h, (l, t, r, b) = dm[0]
    click(l + 93, t + 4 + 17)  # 新增日程
    ib = find_class('z-calendar-input')
    check('点菜单项 → 输入框弹出', bool(ib))
    check('点菜单项 → 菜单已关闭', not find_class('z-calendar-datemenu'))
    if ib:
        user32.PostMessageW(ib[0][0], 0x0100, 0x1B, 0)  # ESC
        time.sleep(0.5)
        check('ESC 取消输入框', not find_class('z-calendar-input'))

print(f'--- {ok} passed, {fail} failed')
exit(1 if fail else 0)
