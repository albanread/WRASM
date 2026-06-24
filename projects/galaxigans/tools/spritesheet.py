import re
from PIL import Image, ImageDraw

def load_sprites(path):
    sprites = []
    cur = None
    for line in open(path, encoding='utf-8', errors='replace'):
        m = re.match(r'^(\w+)\s+BYTE\s+"([0-9A-Fa-f.]*)"\s*$', line)
        m2 = re.match(r'^\s+BYTE\s+"([0-9A-Fa-f.]*)"\s*$', line)
        if m:
            cur = {'name': m.group(1), 'rows': [m.group(2)]}
            sprites.append(cur)
        elif m2 and cur is not None:
            cur['rows'].append(m2.group(1))
    return [s for s in sprites if len(s['rows']) >= 2 and len(s['rows'][0]) >= 2
            and all(c in '0123456789ABCDEFabcdef.' for c in s['rows'][0])]

def idx(c):
    if c in '0123456789':
        return int(c)
    if c.upper() in 'ABCDEF':
        return 10 + 'ABCDEF'.index(c.upper())
    return 0

def gray(i):
    if i == 0:
        return None
    v = int(28 + i * 15)
    return (v, v, min(255, v + 8))

def render(path, out):
    sp = load_sprites(path)
    if not sp:
        print(path, '-> no sprites')
        return
    S, pad, cols = 8, 28, 4
    cw = max(len(s['rows'][0]) for s in sp) * S + pad
    ch = max(len(s['rows']) for s in sp) * S + pad + 16
    rows = (len(sp) + cols - 1) // cols
    img = Image.new('RGB', (cw * cols, ch * rows), (12, 12, 22))
    d = ImageDraw.Draw(img)
    for k, s in enumerate(sp):
        ox = (k % cols) * cw + pad // 2
        oy = (k // cols) * ch + 16
        for y, row in enumerate(s['rows']):
            for x, c in enumerate(row):
                col = gray(idx(c))
                if col:
                    d.rectangle([ox + x * S, oy + y * S, ox + x * S + S - 1, oy + y * S + S - 1], fill=col)
        d.text((ox, oy - 13), '%s %dx%d' % (s['name'], len(s['rows'][0]), len(s['rows'])), fill=(170, 200, 255))
    img.save(out)
    print(path, '->', out, len(sp), 'sprites')

for f, o in [('ship', 'ship'), ('aliens2', 'aliens'), ('saucers', 'saucers'), ('bombs', 'bombs')]:
    render('projects/galaxigans/galaxigans_%s.was' % f, 'projects/galaxigans/sheet_%s.png' % o)
