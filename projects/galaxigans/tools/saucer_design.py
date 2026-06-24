from PIL import Image, ImageDraw
import io
W, H = 32, 16
HEX = '0123456789ABCDEF'
# idx: 1 outline, 2-5 silver hull dark->bright, 6 white rim, 7-9 cyan dome, A-C window lights

def ell(cx, cy, rx, ry, y, x):
    return ((x - cx) / rx) ** 2 + ((y - cy) / ry) ** 2 <= 1.0

def build(frame):
    g = [[0] * W for _ in range(H)]
    # disc body: wide flat ellipse, centre (15.5, 9.5)
    for y in range(H):
        for x in range(W):
            if ell(15.5, 9.5, 15.0, 3.0, y, x):
                g[y][x] = 5 if y <= 8 else (4 if y == 9 else 3)   # lit top -> shadow bottom
    # dome: cyan, top centre
    for y in range(H):
        for x in range(W):
            if y <= 7 and ell(15.5, 6.0, 6.5, 5.0, y, x):
                g[y][x] = 8 if y <= 4 else 7
    for x in (13, 14):                      # dome glint
        if g[2][x]: g[2][x] = 9
        if g[3][x]: g[3][x] = 9
    # rim window-lights along the disc waist (row 9), cycling by frame
    for i, lx in enumerate(range(3, 30, 4)):
        c = 11 if (i + frame) % 2 == 0 else 12
        if g[9][lx]: g[9][lx] = c
        if g[10][lx]: g[10][lx] = 10
    # outline: any hull cell on the silhouette edge
    g2 = [row[:] for row in g]
    for y in range(H):
        for x in range(W):
            if g2[y][x]:
                edge = any(not (0 <= y+dy < H and 0 <= x+dx < W and g2[y+dy][x+dx]) for dy, dx in [(-1,0),(1,0),(0,-1),(0,1)])
                if edge: g[y][x] = 1
    return g

def torows(g): return [''.join(HEX[v] if v else '.' for v in row) for row in g]
def colr(i):
    if i == 0: return None
    if i <= 6: return (int(60+i*28), int(65+i*28), int(75+i*28))
    if i <= 9: return (40, int(140+(i-6)*40), 255)
    if i == 10: return (120, 40, 30)
    return (255, 230, 60) if i == 11 else (255, 120, 30)

frames = [('A', build(0)), ('B', build(1))]
S = 14
img = Image.new('RGB', (W*S, H*S*2 + 20), (12, 12, 22)); d = ImageDraw.Draw(img)
rowsout = {}
for k, (nm, g) in enumerate(frames):
    rows = torows(g); rowsout[nm] = rows
    oy = 10 + k*(H*S+6)
    for y, row in enumerate(rows):
        for x, c in enumerate(row):
            cc = colr(int(c, 16) if c in HEX else 0)
            if cc: d.rectangle([x*S, oy+y*S, x*S+S-1, oy+y*S+S-1], fill=cc)
img.save('projects/galaxigans/saucer_try.png')
print('widths', set(len(r) for n in rowsout for r in rowsout[n]), 'rows', {n: len(rowsout[n]) for n in rowsout})
# write the .was
out = ["; galaxigans_saucerart.was - the bonus SAUCER (UFO), 32x16, 2-frame (rim lights cycle).",
       "; Drawn procedurally + render-verified. Palette slot 7.", "",
       "module Galaxigans", ".balign 16", ".DATA", ".balign 16"]
for nm, lbl in [('A', 'saucerArt'), ('B', 'saucerArt2')]:
    rows = rowsout[nm]
    out.append('%-10s BYTE "%s"' % (lbl, rows[0]))
    for r in rows[1:]:
        out.append('%-10s BYTE "%s"' % ("", r))
out.append("endmodule")
io.open('projects/galaxigans/galaxigans_saucerart.was', 'w', encoding='ascii', newline='\n').write("\n".join(out) + "\n")
print("wrote galaxigans_saucerart.was")
