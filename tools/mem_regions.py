import ctypes, sys
from ctypes import wintypes as wt
pid = int(sys.argv[1])
k = ctypes.windll.kernel32
h = k.OpenProcess(0x1F0FFF, False, pid)
class MB(ctypes.Structure):
    _fields_ = [('BaseAddress', ctypes.c_void_p), ('AllocationBase', ctypes.c_void_p),
                ('AllocationProtect', wt.DWORD), ('RegionSize', ctypes.c_size_t),
                ('State', wt.DWORD), ('Protect', wt.DWORD), ('Type', wt.DWORD)]
regions = {}
addr = 0
while addr < 0x7FFFFFFEFFFF:
    mbi = MB()
    r = k.VirtualQueryEx(h, ctypes.c_void_p(addr), ctypes.byref(mbi), ctypes.sizeof(mbi))
    if not r: break
    size = mbi.RegionSize
    if mbi.State == 0x1000 and mbi.Type == 0x20000:
        ab = mbi.AllocationBase or 0
        regions[ab] = regions.get(ab, 0) + size
    addr = (mbi.BaseAddress or addr) + size
    if size == 0: break
items = sorted(regions.items(), key=lambda kv: -kv[1])
total = sum(v for _, v in items)
print(f'private committed: {total//1024} KB / {len(items)} blocks')
for ab, v in items[:10]:
    print(f'  {v//1024:6d} KB  base={hex(ab)}')
