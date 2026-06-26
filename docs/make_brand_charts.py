#!/usr/bin/env python3
"""LongMemEval-S comparison chart — UltraMem brand system, 3 design variants.

Brand tokens lifted from web/ (tailwind.config.ts + globals.css):
  canvas #09090b · surface #101014 · panel #141418 · hairline #26262e
  ink #ececE6 / muted #9a9a93 / faint #63635d
  lime #ccff00 (UltraMem / the hero) · violet #8b5cf6 (the "other" layer)
Design rule: UltraMem is the ONLY lime/glowing element; the four leaderboard
systems sit in a neutral grayscale ramp, so UltraMem visually owns the chart.
Pure Pillow (no matplotlib). Honesty caveat printed on every variant.
"""
from PIL import Image, ImageDraw, ImageFont, ImageFilter

BG=(9,9,11); SURFACE=(16,16,20); PANEL=(20,20,24); HAIR=(38,38,46)
INK=(236,236,230); MUTE=(154,154,147); FAINT=(99,99,93)
LIME=(204,255,0); LIMEDIM=(166,204,0); VIOLET=(139,92,246)
# neutral ramp for the 4 competitors (bright -> dark), all readable on near-black
RAMP=[(214,214,206),(150,150,142),(104,104,97),(74,74,69)]

CATS=["single-session-user","single-session-assistant","single-session-preference",
      "knowledge-update","temporal-reasoning","multi-session"]
NAMES=["UltraMem","Shram","Supermemory","Zep","Full context"]
VALS={  # per category, indexed like CATS
 "UltraMem":[90,70,45,80,85,65],
 "Shram":[100,100,90,93.6,71.4,72.9],
 "Supermemory":[97.1,96.4,70,88.4,76.7,71.4],
 "Zep":[92.9,80.4,56.7,83.3,62.4,57.9],
 "Full context":[81.4,94.6,20,78.2,45.1,44.3],
}
COLOR={"UltraMem":LIME,"Shram":RAMP[0],"Supermemory":RAMP[1],"Zep":RAMP[2],"Full context":RAMP[3]}

def font(sz, mono=False):
    cands=(["/System/Library/Fonts/SFNSMono.ttf","/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/Monaco.ttf"] if mono else
           ["/System/Library/Fonts/SFNS.ttf","/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/Supplemental/Arial.ttf"])
    for p in cands:
        try: return ImageFont.truetype(p,sz)
        except Exception: pass
    return ImageFont.load_default()

def fmt(v): return f"{v:.0f}%" if float(v).is_integer() else f"{v:.1f}%"

def tracked(d, xy, s, fnt, fill, track=4, anchor_l=True):
    x,y=xy
    for ch in s:
        d.text((x,y),ch,font=fnt,fill=fill,anchor="lm" if anchor_l else "lm")
        x+=d.textlength(ch,font=fnt)+track
    return x

def glow(img, box, color, radius=14, blur=22, alpha=150):
    layer=Image.new("RGBA",img.size,(0,0,0,0))
    ImageDraw.Draw(layer).rounded_rectangle(box,radius=radius,fill=color+(alpha,))
    img.alpha_composite(layer.filter(ImageFilter.GaussianBlur(blur)))

def header(d, img, w, title, sub):
    # eyebrow with lime dot
    d.ellipse([60,58,68,66],fill=LIME)
    tracked(d,(80,62),"RESEARCH · BENCHMARK",font(15,mono=True),MUTE,track=3)
    d.text((60,92),title,font=font(52),fill=INK,anchor="la")
    d.text((62,162),sub,font=font(18,mono=True),fill=FAINT,anchor="la")

def legend(d, x_right, y, names):
    x=x_right
    for nm in reversed(names):
        c=COLOR[nm]; lab=nm
        tw=d.textlength(lab,font=font(19,mono=True))
        x-=tw; d.text((x,y),lab,font=font(19,mono=True),fill=MUTE if nm!="UltraMem" else INK,anchor="lm"); x-=10
        x-=18; d.rounded_rectangle([x,y-9,x+18,y+9],radius=4,fill=c); x-=26

def card(d,box,r=16):
    d.rounded_rectangle(box,radius=r,fill=SURFACE,outline=HAIR,width=2)

CAVEAT=("UltraMem · 120-Q slice (20/cat) · Gemini 2.5 Flash judge   —   "
        "Shram / Supermemory / Zep / Full context: full-set · GPT-4o judge (indicative, not strictly comparable)")

# ---------------------------------------------------------------- Variant 1: vertical grouped bars
def variant1():
    W,H=2000,1080
    img=Image.new("RGBA",(W,H),BG+(255,))
    d=ImageDraw.Draw(img)
    header(d,img,W,"UltraMem vs the field",CAVEAT)
    cx0,cy0,cx1,cy1=60,210,W-60,H-50
    card(d,[cx0,cy0,cx1,cy1])
    tracked(d,(cx0+28,cy0+34),"ACCURACY BY QUESTION TYPE",font(15,mono=True),MUTE,track=3)
    legend(d,cx1-28,cy0+34,NAMES)
    base=cy1-150; top=cy0+90; fullh=base-top
    L=cx0+40; R=cx1-40; pw=R-L; gw=pw/len(CATS)
    bw,gap=42,7; n=len(NAMES); grp=n*bw+(n-1)*gap; pad=(gw-grp)/2
    for gi,cat in enumerate(CATS):
        gx=L+gi*gw
        for si,nm in enumerate(NAMES):
            v=VALS[nm][gi]; c=COLOR[nm]
            bx=gx+pad+si*(bw+gap); bh=max(6,fullh*v/100); ty=base-bh
            if nm=="UltraMem": glow(img,[bx-2,ty-2,bx+bw+2,base],LIME,blur=16,alpha=110)
            d.rounded_rectangle([bx,ty,bx+bw,base],radius=8,fill=c)
            d.rectangle([bx,base-8,bx+bw,base],fill=c)
            d.text((bx+bw/2,ty-8),fmt(v),font=font(14,mono=True),
                   fill=LIME if nm=="UltraMem" else MUTE,anchor="mb")
        # category label (mono, wrapped on hyphens)
        parts=cat.split("-"); line1=parts[0]; line2="-".join(parts[1:])
        d.text((gx+gw/2,base+26),line1,font=font(15,mono=True),fill=INK,anchor="ma")
        if line2: d.text((gx+gw/2,base+48),"-"+line2,font=font(15,mono=True),fill=MUTE,anchor="ma")
    d.line([L,base,R,base],fill=HAIR,width=2)
    img.convert("RGB").save("docs/brand-v1-bars.png"); print("v1")

# ---------------------------------------------------------------- Variant 2: horizontal bars (site-native)
def variant2():
    W=2000; rows=len(CATS); rh=210; H=300+rows*rh
    img=Image.new("RGBA",(W,H),BG+(255,))
    d=ImageDraw.Draw(img)
    header(d,img,W,"Accuracy by question type",CAVEAT)
    cx0,cy0,cx1=60,210,W-60
    card(d,[cx0,cy0,cx1,H-50])
    legend(d,cx1-28,cy0+34,NAMES)
    tracked(d,(cx0+28,cy0+34),"ULTRAMEM VS THE FIELD",font(15,mono=True),MUTE,track=3)
    trackL=cx0+360; trackR=cx1-40; tw=trackR-trackL
    y=cy0+90
    for gi,cat in enumerate(CATS):
        d.text((cx0+28,y+6),cat,font=font(17,mono=True),fill=INK,anchor="la")
        bh=20; bgap=8
        for si,nm in enumerate(NAMES):
            v=VALS[nm][gi]; c=COLOR[nm]; by=y+34+si*(bh+bgap)
            d.rounded_rectangle([trackL,by,trackR,by+bh],radius=6,fill=PANEL)  # track
            ww=max(tw*v/100,40)
            if nm=="UltraMem": glow(img,[trackL,by-1,trackL+ww,by+bh+1],LIME,blur=12,alpha=90)
            d.rounded_rectangle([trackL,by,trackL+ww,by+bh],radius=6,fill=c)
            d.text((trackL+ww-8,by+bh/2),fmt(v),font=font(12,mono=True),
                   fill=BG if nm=="UltraMem" else (BG if si==1 else INK),anchor="rm")
            d.text((cx0+200,by+bh/2),nm.upper() if nm!="Full context" else "FULL CTX",
                   font=font(10,mono=True),fill=LIME if nm=="UltraMem" else FAINT,anchor="lm")
        if gi<rows-1: d.line([cx0+28,y+rh-22,cx1-28,y+rh-22],fill=HAIR,width=1)
        y+=rh
    img.convert("RGB").save("docs/brand-v2-hbars.png"); print("v2")

# ---------------------------------------------------------------- Variant 3: lollipop / dot plot
def variant3():
    W,H=2000,1000
    img=Image.new("RGBA",(W,H),BG+(255,))
    d=ImageDraw.Draw(img)
    header(d,img,W,"Where UltraMem sits in the field",CAVEAT)
    cx0,cy0,cx1,cy1=60,210,W-60,H-50
    card(d,[cx0,cy0,cx1,cy1])
    legend(d,cx1-28,cy0+34,NAMES)
    axL=cx0+360; axR=cx1-60; axw=axR-axL
    top=cy0+90; bot=cy1-70; rows=len(CATS); rstep=(bot-top)/rows
    # gridlines 0..100
    for g in range(0,101,25):
        gx=axL+axw*g/100
        d.line([gx,top-10,gx,bot],fill=(255,255,255,12),width=1)
        d.text((gx,bot+18),f"{g}%",font=font(13,mono=True),fill=FAINT,anchor="ma")
    for gi,cat in enumerate(CATS):
        ry=top+rstep*gi+rstep/2
        d.text((cx0+28,ry),cat,font=font(16,mono=True),fill=INK,anchor="lm")
        xs={nm:axL+axw*VALS[nm][gi]/100 for nm in NAMES}
        lo=min(xs.values()); hi=max(xs.values())
        d.line([lo,ry,hi,ry],fill=HAIR,width=3)
        for nm in NAMES:
            if nm=="UltraMem": continue
            d.ellipse([xs[nm]-7,ry-7,xs[nm]+7,ry+7],fill=COLOR[nm])
        ux=xs["UltraMem"]
        glow(img,[ux-13,ry-13,ux+13,ry+13],LIME,radius=13,blur=16,alpha=160)
        d.ellipse([ux-12,ry-12,ux+12,ry+12],fill=LIME)
        d.text((ux,ry-26),fmt(VALS["UltraMem"][gi]),font=font(14,mono=True),fill=LIME,anchor="mb")
    img.convert("RGB").save("docs/brand-v3-lollipop.png"); print("v3")

variant1(); variant2(); variant3()
print("done")
