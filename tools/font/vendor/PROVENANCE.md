# フォント元データの由来

> 索引: [`../../../DESIGN.md`](../../../DESIGN.md)

このディレクトリは`tab5-font`が使う16 pixel bitmap fontの**元データ**を保持します。
生成手順と収録範囲は[`../README.md`](../README.md)を参照してください。

## unifont_jp-17.0.05.bdf.gz

| 項目 | 値 |
| --- | --- |
| 名称 | GNU Unifont Japanese (Unifont-JP) |
| 版 | 17.0.05 (BDFの`FONT_VERSION`プロパティ) |
| 取得元 | <https://unifoundry.com/pub/unifont/unifont-17.0.05/font-builds/unifont_jp-17.0.05.bdf.gz> |
| 収録範囲 | Unicode Plane 0のみ。`ENCODING`の最大値は65533 (U+FFFD) |
| glyph数 | 57,086 (`CHARS`プロパティ) |
| 字幅 | `DWIDTH`が8または16、`FONTBOUNDINGBOX`は`16 16 0 -2` |
| license | SIL Open Font License 1.1 ([`LICENSE-OFL-1.1.txt`](LICENSE-OFL-1.1.txt)) |

### hash

```text
SHA-256 (unifont_jp-17.0.05.bdf.gz) = d3a4c98e41efcf38b49bd520a049230cc040d44433ab9c2cdcd9f1f481443976
SHA-256 (展開後の unifont_jp-17.0.05.bdf) = 044463a47a5b320a1281dcd15fcb3010d6a4ec19603e4193bf28d12909cd009c
```

生成toolが検証するのは**展開後のBDF**のhashです。gzipの再圧縮やファイル名の変更で
archive側のhashは変わりますが、BDF本文のhashは変わらないためです。archive側のhashは
版固定URLから取得する配布物にも一致します。1,302,466 byteのarchiveはリポジトリへcommitせず、
root `Makefile`がdownloadとarchive hash検証を行います。

### licenseの本文について

Unifontの配布物はOFL 1.1とGNU Font Embedding Exception付きGPLv2+のdual licenseです。
本プロジェクトはOFL 1.1側を採用します。[`LICENSE-OFL-1.1.txt`](LICENSE-OFL-1.1.txt)は、
BDFの`COPYRIGHT`プロパティに記録された17.0.05のcopyright表示と、OFL 1.1の本文
（2007-02-26版、Unifontが配布するlicense fileと同一の本文）を並べたものです。

subset化、16×16 canvasへの正規化、column-majorへの転置、独自binary形式への格納は
OFLでいうModified Versionの作成にあたります。生成物`font/data/`はこのlicenseの下に
あり、reserved font nameを使わないために生成物の名前へ`Unifont`を含めません。
