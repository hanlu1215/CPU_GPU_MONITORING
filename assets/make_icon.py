"""生成 assets/icon.ico（AAsysmon 程序图标）。

用法: python assets/make_icon.py

要点（旧版 bug）:
- BMP 条目（16/20/24/32）的 AND 掩码是 1bpp，**每行必须按 4 字节对齐**。
  16px 行是 2 字节、20/24px 行是 3 字节，不足 4 的倍数必须补零，
  否则 Windows 解析错位，小图标显示花屏（大图标走 PNG 条目所以不受影响）。
- PNG 条目（>=48）像素是 RGBA；BMP 条目像素是 BGRA 且自下而上，别混。
"""

import struct
import zlib

# 与窗口一致：深色底板 + 蓝色柱 + 绿色标签条
BG = (26, 30, 42)      # #1A1E2A
BLUE = (77, 163, 255)  # #4DA3FF
GREEN = (70, 230, 140)  # #46E68C

SS = 8  # 超采样倍数


def rasterize(size):
    """返回 RGBA 字节串（自上而下）。"""
    n = size * SS
    px = bytearray(n * n * 4)

    def rect(x0, y0, x1, y1, color, radius=0.0):
        r, g, b = color
        x0, y0, x1, y1, radius = (v * SS for v in (x0, y0, x1, y1, radius))
        for y in range(max(0, int(y0)), min(n, int(y1) + 1)):
            for x in range(max(0, int(x0)), min(n, int(x1) + 1)):
                # 圆角：中心点在圆角区内时要求落在圆内
                cx = min(max(x + 0.5, x0 + radius), x1 - radius)
                cy = min(max(y + 0.5, y0 + radius), y1 - radius)
                if radius > 0 and (x + 0.5 - cx) ** 2 + (y + 0.5 - cy) ** 2 > radius * radius:
                    continue
                i = (y * n + x) * 4
                px[i:i + 4] = bytes((r, g, b, 255))

    m = size / 16.0  # 以 16px 为基准的设计单位
    rect(0, 0, size, size, BG, radius=3.2 * m)          # 圆角底板
    bw, gap = 3.0 * m, 1.4 * m                           # 柱宽/间距
    x = 2.2 * m
    base = 12.6 * m
    for h in (4.6, 7.2, 9.8):                            # 三根递增的柱
        rect(x, base - h * m, x + bw, base, BLUE, radius=0.7 * m)
        x += bw + gap
    rect(2.2 * m, 13.4 * m, size - 2.2 * m, 14.6 * m, GREEN, radius=0.6 * m)  # 标签条

    # 超采样降回 size×size
    out = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(SS):
                for sx in range(SS):
                    i = ((y * SS + sy) * n + x * SS + sx) * 4
                    for c in range(4):
                        acc[c] += px[i + c]
            o = (y * size + x) * 4
            inv = SS * SS
            out[o:o + 4] = bytes(v // inv for v in acc)
    return bytes(out)


def png_entry(size, rgba):
    raw = b"".join(b"\x00" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


def bmp_entry(size, rgba):
    # XOR：BGRA、自下而上
    xor = bytearray()
    for y in range(size - 1, -1, -1):
        row = rgba[y * size * 4:(y + 1) * size * 4]
        for i in range(0, len(row), 4):
            xor += bytes((row[i + 2], row[i + 1], row[i], row[i + 3]))
    # AND：1bpp，1=透明；每行按 4 字节对齐（关键修复点）
    row_bytes = ((size + 31) // 32) * 4
    mask = bytearray()
    for y in range(size - 1, -1, -1):
        bits = 0
        nbits = 0
        line = bytearray()
        for x in range(size):
            alpha = rgba[(y * size + x) * 4 + 3]
            bits = (bits << 1) | (1 if alpha < 128 else 0)
            nbits += 1
            if nbits == 8:
                line.append(bits)
                bits = 0
                nbits = 0
        if nbits:
            line.append(bits << (8 - nbits))
        line += b"\x00" * (row_bytes - len(line))
        mask += line
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0,
                         len(xor) + len(mask), 0, 0, 0, 0)
    return header + bytes(xor) + bytes(mask)


def main():
    sizes = [16, 20, 24, 32, 48, 64, 256]
    blobs = []
    for s in sizes:
        rgba = rasterize(s)
        blobs.append(png_entry(s, rgba) if s >= 48 else bmp_entry(s, rgba))

    header = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries = b""
    for s, blob in zip(sizes, blobs):
        entries += struct.pack("<BBBBHHII", s if s < 256 else 0, s if s < 256 else 0,
                               0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    with open("assets/icon.ico", "wb") as f:
        f.write(header + entries + b"".join(blobs))

    # 自检：BMP 条目声明尺寸必须等于 40 + XOR + 对齐后的 AND
    for s, blob in zip(sizes, blobs):
        if s < 48:
            expect = 40 + s * s * 4 + ((s + 31) // 32) * 4 * s
            assert len(blob) == expect, f"{s}px 条目尺寸不符: {len(blob)} != {expect}"
    print("icon.ico regenerated:", ", ".join(map(str, sizes)))


if __name__ == "__main__":
    main()
