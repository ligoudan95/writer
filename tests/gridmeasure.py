# 稿纸行线像素测量仪 v3：明暗主题自适应
# 用法：python tests/gridmeasure.py <png> [x0 x1]
import sys
from collections import Counter
from PIL import Image

png = sys.argv[1]
img = Image.open(png).convert("RGB")
W, H = img.size
x0 = int(sys.argv[2]) if len(sys.argv) > 2 else int(W * 0.35)
x1 = int(sys.argv[3]) if len(sys.argv) > 3 else int(W * 0.62)
px = img.load()

lums = []
for y in range(H):
    row = []
    for x in range(x0, x1):
        r, g, b = px[x, y]
        row.append(0.299 * r + 0.587 * g + 0.114 * b)
    lums.append(row)

all_vals = sorted(v for row in lums for v in row)
bg = all_vals[len(all_vals) // 2]  # 中位数 = 纸底/深底
print(f"图 {W}x{H} 列[{x0},{x1}) 背景基准亮度={bg:.0f} ({'深色主题' if bg < 128 else '浅色主题'})")

stats = []
for y in range(H):
    row = lums[y]
    mean = sum(row) / len(row)
    strong = sum(1 for v in row if abs(v - bg) > 55)   # 文字（高对比）
    mild = abs(mean - bg)
    stats.append((strong, mean, mild))

is_text = [s >= 2 for s, m, mi in stats]
is_line_candidate = [(not is_text[y]) and (2.0 <= stats[y][2] <= 16.0) for y in range(H)]

lines = []
for y in range(H):
    if is_line_candidate[y]:
        if lines and y - lines[-1][-1] <= 1:
            lines[-1].append(y)
        else:
            lines.append([y])
line_ys = [sum(g) / len(g) for g in lines if len(g) <= 3]

phases = Counter(round(ly % 34) for ly in line_ys)
if not phases:
    print("未检测到横线相位！")
    sys.exit(1)
phase = phases.most_common(1)[0][0]
grid = sorted(set(round(phase + 34 * k) for k in range(0, H // 34 + 2)))
real_hits = sum(1 for ly in line_ys if min(abs(g - ly) for g in grid) <= 1.5)

# 不透明底色区（代码块底色等）：连续>=14行的均匀非背景亮度
opaque = []
run = None
for y in range(H):
    bgish = (not is_text[y]) and 2.0 <= stats[y][2] <= 16.0
    if bgish:
        run = run or [y, y]
        run[1] = y
    else:
        if run and run[1] - run[0] >= 14:
            opaque.append(tuple(run))
        run = None
if run and run[1] - run[0] >= 14:
    opaque.append(tuple(run))

def region_of(y):
    for a, b in opaque:
        if a <= y <= b:
            return "OPAQUE"
    return "open"

bands = []
for y in range(H):
    if is_text[y]:
        if bands and y - bands[-1][1] <= 3:
            bands[-1][1] = y
        else:
            bands.append([y, y])

print(f"相位(34模)={phase} | 命中真实线 {real_hits}/{len(line_ys)} | 不透明区 {opaque}")
print(f"{'顶':>4} {'底':>4} {'高':>4} {'强px':>5} {'区域':>7} {'下一网格线':>8} {'底→线距':>7}")
for top, bottom in bands:
    ny = next((g for g in grid if g >= bottom + 1), None)
    gap = ny - bottom if ny is not None else "-"
    reg = region_of((top + bottom) // 2)
    strong_max = max(stats[y][0] for y in range(top, bottom + 1))
    print(f"{top:>4} {bottom:>4} {bottom-top+1:>4} {strong_max:>5} {reg:>7} {str(ny):>8} {str(gap):>7}")
