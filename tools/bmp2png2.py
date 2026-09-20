import sys, struct, zlib
inp, out = sys.argv[1], sys.argv[2]
with open(inp, 'rb') as f:
    data = f.read()
offset = int.from_bytes(data[10:14], 'little')
w = int.from_bytes(data[18:22], 'little')
h = int.from_bytes(data[22:26], 'little')
rows = []
bg = (16, 21, 28)
for y in range(h):
    row = bytearray()
    src = offset + (h - 1 - y) * w * 4
    for x in range(w):
        b, g, r, a = data[src + x*4], data[src + x*4 + 1], data[src + x*4 + 2], data[src + x*4 + 3]
        a = a / 255.0
        row += bytes([int(r*a + bg[0]*(1-a)), int(g*a + bg[1]*(1-a)), int(b*a + bg[2]*(1-a))])
    rows.append(bytes(row))
raw = b''.join(b'\x00' + bytes(r) for r in rows)
def chunk(t, d):
    c = struct.pack('>I', len(d)) + t + d
    return c + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
png = b'\x89PNG\r\n\x1a\n'
png += chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
png += chunk(b'IDAT', zlib.compress(raw))
png += chunk(b'IEND', b'')
open(out, 'wb').write(png)
print('written', out)
