import ctypes, sys
from ctypes import wintypes
user32 = ctypes.windll.user32
pid_target = int(sys.argv[1])
EnumWindows = user32.EnumWindows
WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
GetWindowRect = user32.GetWindowRect
IsWindowVisible = user32.IsWindowVisible
GetWindowThreadProcessId = user32.GetWindowThreadProcessId
GetClassNameW = user32.GetClassNameW
class RECT(ctypes.Structure):
    _fields_ = [("left", ctypes.c_long), ("top", ctypes.c_long), ("right", ctypes.c_long), ("bottom", ctypes.c_long)]
results = []
@WNDENUMPROC
def cb(hwnd, lparam):
    pid = wintypes.DWORD()
    GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    if pid.value == pid_target:
        r = RECT()
        GetWindowRect(hwnd, ctypes.byref(r))
        buf = ctypes.create_unicode_buffer(256)
        GetClassNameW(hwnd, buf, 256)
        results.append((hwnd, buf.value, bool(IsWindowVisible(hwnd)), (r.right-r.left, r.bottom-r.top, r.left, r.top)))
    return True
EnumWindows(cb, 0)
for hwnd, cls, vis, geo in results:
    print(f"hwnd={hwnd} class={cls} vis={vis} size={geo[0]}x{geo[1]} at {geo[2]},{geo[3]}")
