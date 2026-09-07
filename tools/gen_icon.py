# 生成 assets/icon.ico
# 从 assets/icon_source.png（完整版，含速度线）与 icon_source_small.png（简化底座版）
# 读取源图，缩放为多尺寸 Windows ICO。
# ICO 帧为 32bpp BGRA BMP（BITMAPINFOHEADER + XOR + AND 掩码），兼容 winresource。
import os
import struct
from pathlib import Path

from PIL import Image

SIZES = [16, 24, 32, 48, 64, 128, 256]
SMALL_THRESHOLD = 48  # 小于等于此尺寸用简化版源图，避免细节糊成一团
PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE_FULL = PROJECT_ROOT / "assets" / "icon_source.png"
SOURCE_SMALL = PROJECT_ROOT / "assets" / "icon_source_small.png"
OUT = PROJECT_ROOT / "assets" / "icon.ico"


def resize(src: Image.Image, size: int) -> Image.Image:
    if size <= 48:
        return src.resize((size, size), Image.Resampling.NEAREST)
    return src.resize((size, size), Image.Resampling.LANCZOS)


def frame(src: Image.Image, size: int) -> bytes:
    img = resize(src, size).convert("RGBA")
    pixels = img.load()

    # BITMAPINFOHEADER (biHeight = 2x：XOR + AND)
    hdr = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)

    xor = bytearray()
    for y in range(size - 1, -1, -1):  # BMP 自下而上
        for x in range(size):
            r, g, b, a = pixels[x, y]
            xor += bytes((b, g, r, a))

    # AND 掩码：alpha 为 0 的像素置 0
    row = ((size + 31) // 32) * 4
    and_mask = bytearray()
    for y in range(size - 1, -1, -1):
        bits = 0
        for x in range(size):
            if pixels[x, y][3] >= 128:
                bits |= 1 << (size - 1 - x)
        and_mask += bits.to_bytes(row, "big")

    return hdr + bytes(xor) + bytes(and_mask)


def main():
    full = Image.open(SOURCE_FULL).convert("RGBA")
    small = Image.open(SOURCE_SMALL).convert("RGBA")

    def square(src: Image.Image) -> Image.Image:
        w, h = src.size
        if w == h:
            return src
        side = min(w, h)
        left = (w - side) // 2
        top = (h - side) // 2
        return src.crop((left, top, left + side, top + side))

    frames = [(s, frame(square(small) if s <= SMALL_THRESHOLD else square(full), s)) for s in SIZES]
    out = struct.pack("<HHH", 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    for s, data in frames:
        w = 0 if s >= 256 else s
        out += struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in frames:
        out += data

    os.makedirs(OUT.parent, exist_ok=True)
    OUT.write_bytes(out)
    print(f"wrote {OUT} ({len(out)} bytes, {len(SIZES)} frames)")


if __name__ == "__main__":
    main()
