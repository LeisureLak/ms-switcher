# 一次性工具：生成 assets/icon.ico（与托盘代码同一设计：深蓝圆 + 白色指针）
# ICO 内含 32bpp BGRA BMP 帧（BITMAPINFOHEADER + XOR + AND 掩码）
import struct, os

BLUE = (0x00, 0x78, 0xD7)   # RGB
WHITE = (0xFF, 0xFF, 0xFF)
SIZES = [16, 24, 32, 48, 64]

def render(size):
    rgba = [[None] * size for _ in range(size)]
    c = (size - 1) / 2.0
    k = size / 32.0
    for y in range(size):
        for x in range(size):
            dx = (x - c) / k
            dy = (y - c) / k
            d = (dx * dx + dy * dy) ** 0.5 * k
            if d <= c * k:
                if -2.0 < dy < 8.0 and -6.0 + dy * 0.45 < dx < -1.0 + dy * 0.45:
                    rgba[y][x] = WHITE
                else:
                    rgba[y][x] = BLUE
    return rgba

def frame(size):
    rgba = render(size)
    # BITMAPINFOHEADER (biHeight = 2x：XOR + AND)
    hdr = struct.pack('<IiiHHIIiiII', 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    xor = bytearray()
    for y in range(size - 1, -1, -1):  # BMP 自下而上
        for x in range(size):
            p = rgba[y][x]
            if p is None:
                xor += b'\x00\x00\x00\x00'
            else:
                r, g, b = p
                xor += bytes((b, g, r, 0xFF))
    row = ((size + 31) // 32) * 4
    and_mask = bytearray()
    for y in range(size - 1, -1, -1):
        bits = 0
        for x in range(size):
            if rgba[y][x] is not None:
                bits |= 1 << (size - 1 - x)
        and_mask += bits.to_bytes(row, 'big')  # MSB-first，逐字节大端即可
    return hdr + bytes(xor) + bytes(and_mask)

def main():
    frames = [(s, frame(s)) for s in SIZES]
    out = struct.pack('<HHH', 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    for s, data in frames:
        w = 0 if s >= 256 else s
        out += struct.pack('<BBBBHHII', w, w, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in frames:
        out += data
    path = os.path.join(os.path.dirname(__file__), '..', 'assets', 'icon.ico')
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'wb') as f:
        f.write(out)
    print(f'wrote {path} ({len(out)} bytes)')

if __name__ == '__main__':
    main()
