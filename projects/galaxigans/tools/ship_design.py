from PIL import Image, ImageDraw
W = H = 24
HEX = '0123456789ABCDEF'

leftcol = [11,11,10,10, 9, 9, 9, 9,
           8, 7, 5, 3, 1, 0, 1, 4,
           7, 9, 9, 6, 5, 9,10,10]

def silhouette(bank):
    lc = list(leftcol); rc = [W-1-l for l in leftcol]
    if bank < 0:                       # bank LEFT: left wing foreshortens, hull leans left
        for y in range(9,16): lc[y] = min(10, lc[y] + 5)
        for y in range(0,9): lc[y]-=1; rc[y]-=1
        for y in range(16,24): lc[y]-=1; rc[y]-=1
    elif bank > 0:                     # bank RIGHT: mirror
        for y in range(9,16): rc[y] = max(13, rc[y] - 5)
        for y in range(0,9): lc[y]+=1; rc[y]+=1
        for y in range(16,24): lc[y]+=1; rc[y]+=1
    return lc, rc

def build(bank):
    lc, rc = silhouette(bank)
    g = [[0]*W for _ in range(H)]
    for y in range(H):
        L, R = lc[y], rc[y]
        if L > R: continue
        mid = (L+R)/2
        for x in range(L, R+1):
            d = min(x-L, R-x)
            if d == 0: g[y][x] = 1
            elif d == 1: g[y][x] = 3
            elif abs(x-mid) < 1.2: g[y][x] = 5
            else: g[y][x] = 4
    # cockpit canopy near the hull centre (follow the bank a little)
    cxs = 11 + (1 if bank>0 else -1 if bank<0 else 0)
    for y in range(4,10):
        for x in (cxs-1,cxs,cxs+1,cxs+2):
            if 0<=x<W and g[y][x]: g[y][x]=7
    for x in (cxs,cxs+1):
        if 0<=x<W:
            if g[5][x]: g[5][x]=8
            if g[6][x]: g[6][x]=9
            if g[7][x]: g[7][x]=8
    # engine glow at the rear centre
    ex = 11 + (1 if bank>0 else -1 if bank<0 else 0)
    for x in (ex-1,ex,ex+1,ex+2):
        if 0<=x<W and g[21][x]: g[21][x]=10
    for x in (ex,ex+1):
        if 0<=x<W:
            if g[22][x]: g[22][x]=11
            g[23][x]=12
    return g

def torows(g): return [''.join(HEX[v] if v else '.' for v in row) for row in g]

def colr(i):
    if i==0: return None
    if i<=6: return (int(30+i*28),int(45+i*30),int(78+i*26))
    if i<=9: return (40,int(120+(i-6)*45),255)
    return (255,int(215-(i-10)*45),55)

frames = [('C',build(0)),('L',build(-1)),('R',build(1))]
S=12; gap=20
img=Image.new('RGB',(W*S*3+gap*4, H*S+24),(12,12,22)); d=ImageDraw.Draw(img)
out_rows={}
for k,(name,g) in enumerate(frames):
    rows=torows(g); out_rows[name]=rows
    ox=gap+k*(W*S+gap)
    for y,row in enumerate(rows):
        for x,c in enumerate(row):
            v=int(c,16) if c in HEX else 0; cc=colr(v)
            if cc: d.rectangle([ox+x*S,12+y*S,ox+x*S+S-1,12+y*S+S-1],fill=cc)
    d.text((ox,0),'ship'+name,fill=(180,200,255))
img.save('projects/galaxigans/ship_try.png')
print('widths', set(len(r) for n in out_rows for r in out_rows[n]), 'rows', {n:len(out_rows[n]) for n in out_rows})
import json
open('projects/galaxigans/ship_rows.json','w').write(json.dumps(out_rows))
