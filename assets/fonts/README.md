# Scene fonts

The fonts Sorte embeds for the text it draws in the scene (wheel labels, the debug overlay). They ship unmodified inside the executable so labels look the same on every OS and reference images are reproducible. Chinese, Japanese and Korean come from the system fonts, which keeps the binary about 7 MB smaller than with Noto CJK (decided 2026-11-05, `docs/design.md` § Stack tecnologico).

All of them are under the SIL Open Font License 1.1: `OFL.txt` holds the licence and every font's copyright notice, and it must ship wherever the fonts do (the About window lists it).

| File | Font | Version | Upstream | SHA-256 |
| --- | --- | --- | --- | --- |
| `NotoSans[wght].ttf` | Noto Sans, variable weight | 2.015 | [notofonts/latin-greek-cyrillic](https://github.com/notofonts/latin-greek-cyrillic) | `fb53504de62e767a54695416123d68d67f71c9b69722175184ad56e7dd13547b` |
| `NotoSansMono[wght].ttf` | Noto Sans Mono, variable weight | 2.014 | [notofonts/latin-greek-cyrillic](https://github.com/notofonts/latin-greek-cyrillic) | `3c874b97ce11dc54de004c81df02c8f0974033ed1b113377130cbdd71d478f11` |
| `NotoSansArabic[wght].ttf` | Noto Sans Arabic | 2.013 | [notofonts/arabic](https://github.com/notofonts/arabic) | `3fae4d7239d1faa55b234bda8091019a5251ff79df40d5e29367d84bed2a8138` |
| `NotoSansHebrew[wght].ttf` | Noto Sans Hebrew | 3.001 | [notofonts/hebrew](https://github.com/notofonts/hebrew) | `9d81d81ad592644487b03843da73a87b99cd28a2c4da4a096e1db8fbd14fda61` |
| `NotoSansDevanagari[wght].ttf` | Noto Sans Devanagari | 2.006 | [notofonts/devanagari](https://github.com/notofonts/devanagari) | `ffb0d05ef3d8e6d0c3d7c9380099127b003854242c11d0751b6a2e85e1f8f1b5` |
| `NotoSansBengali[wght].ttf` | Noto Sans Bengali | 3.011 | [notofonts/bengali](https://github.com/notofonts/bengali) | `dc9defe97bb041876b6609e9029d9e1e228a686259acde2fe2a9f99d793ee17d` |
| `NotoSansThai[wght].ttf` | Noto Sans Thai | 2.002 | [notofonts/thai](https://github.com/notofonts/thai) | `13f52f79b04f01f2751a81052a5dee2df639fd4e13f4ac30821ab45c8dd5f7e7` |
| `Noto-COLRv1.ttf` | Noto Color Emoji, COLRv1 | 2.057 | [googlefonts/noto-emoji](https://github.com/googlefonts/noto-emoji) | `b8e25ea68db82f9e4d0aee921f4420be2be39887bd5c893a2ad98710531f9d0c` |

The files were taken from Fedora 45's packages `google-noto-*-vf-fonts` 20260601 and `google-noto-color-emoji-fonts` 20260921, which ship the upstream releases unchanged.

To update a font, replace the file with an upstream release, update its row here, and run the reference-image suite: glyph shapes and metrics change the rendered scene.
