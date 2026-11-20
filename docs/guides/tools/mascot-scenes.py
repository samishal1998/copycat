# Build the GPUI mascot's scenes atlases (9 x 7 cells of 144px, lossy WebP)
# from the copycat-scenarios-final bundle: rows held, fling right / left / up
# / down, knead, sleep. That cat is drawn ~0.77x the core cat, so it is
# scaled up about its bottom-centre anchor to match; held is drawn big already.
# usage: python3 mascot-scenes.py <copycat-scenarios-final> apps/copycat-gpui/assets/mascot
import json, sys
from PIL import Image

src, out = sys.argv[1], sys.argv[2]
m = json.load(open(f'{src}/manifest.json'))
ANCHOR, CELL = (96, 164), 144
for theme in ('light', 'dark'):
    seqs = m['themes'][theme]['sequences']
    sheet = Image.new('RGBA', (9 * CELL, 7 * CELL))
    for r, name in enumerate(m['scene_order']):
        scale = 1.0 if name == 'held' else 1.3
        ox, oy = round(ANCHOR[0] * (1 - scale)), round(ANCHOR[1] * (1 - scale))
        for c, f in enumerate(seqs[name]['frames']):
            big = Image.open(f'{src}/{f["path"]}').convert('RGBA').resize((round(192 * scale),) * 2, Image.LANCZOS)
            cell = big.crop((-ox, -oy, -ox + 192, -oy + 192)).resize((CELL, CELL), Image.LANCZOS)
            sheet.alpha_composite(cell, (c * CELL, r * CELL))
    sheet.save(f'{out}/scenes-{theme}.webp', 'WEBP', quality=85, alpha_quality=100, method=6)
