# Fonts of the Rust window

egui cannot use the system UI font, so the window embeds one: **Noto Sans**.

- Why Noto Sans: it is what Plasma uses by default, so the window looks like the design renders. Its digits are
  tabular by default (every digit is 572/1000 em wide), which egui needs because it has no `tnum` switch. Open licence.
- Licence: SIL Open Font License 1.1 (full text in `OFL.txt` next to the fonts, copyright "The Noto Project
  Authors"). The OFL allows embedding, subsetting and shipping inside a program; the font files are not sold alone and
  keep the licence file next to them. The Noto family has no Reserved Font Name.
- Source: the variable font `NotoSans[wdth,wght].ttf` from `notofonts/latin-greek-cyrillic` (2022 release).
- What was changed (with `fonttools`): cut three static weights (400, 500, 600; width fixed at 100), subset to
  Basic Latin, no-break space, degree sign, en and em dash, right single quote, ellipsis, U+2212 and (400 only) the
  middle dot U+00B7 for "Wave XLR · Firmware 1.3.5" (glyph copied, decomposed, from the system Noto Sans Regular), hinting
  removed, `kern` kept. In this release the minus sign U+2212 is a copy of the hyphen (322 units wide), so the
  window's copy redraws it as a bar as wide as the plus sign (572 units, same height) to get a real minus.
- Size: three files of about 12 KB each (36 KB).
- Weights in use: 400 row labels, hints and the unit, 500 buttons, the status title and scale numbers, 600 readouts and group titles.

To rebuild, run `fonttools varLib.instancer` for each weight, redraw U+2212 from the plus bar, then `pyftsubset` with
the character list above.
