# tab5test

M5Stack Tab5（ESP32-P4 ECO2）のベアメタルRust実験用リポジトリです。
設計、ハードウェア構成、ECO2固有の制約は[DESIGN.md](DESIGN.md)を参照してください。

## ドキュメントの扱い

**`README.md`は人間がメンテします。指示された場合を除き編集しないでください。**
コードの追加・削除でREADMEの記述が古くなる場合も、勝手に直さずに「READMEの
この記述が実態と合わなくなる」と報告するだけにとどめ、更新するかは人間が
決めます。`.claude/settings.json`の`permissions.ask`でも確認を挟むように
設定してあります。

`DESIGN.md`と`docs/`以下は通常の作業対象です。実装を変えたら対応する記述も
更新してください。

## 言語

- `src/`以下のコードコメント（`//`・`///`・`//!`）はすべて英語
- `DESIGN.md`・`README.md`など人間向けドキュメントは日本語

## ビルド

```sh
cargo build --release
```

`riscv32imafc-unknown-none-elf`をターゲットにした`no_std`バイナリです。
debugビルドはRAMに収まらずリンクに失敗するので、確認は必ず`--release`で
行ってください。実機への書き込みは`cargo run --release`（`espflash`）です。

## コーディング方針

各モジュール末尾の`read`/`write`/`modify`（任意の`usize`アドレスを読み書きする
MMIOプリミティブ）は`unsafe fn`として定義します。これらを呼び出す関数は、既知の
ハードウェア定数アドレスしか渡さないことで安全性を担保するので`unsafe fn`には
せず、関数内で`unsafe { ... }`ブロックにまとめます（呼び出し1つずつを`unsafe`で
囲むのではなく、関数単位でまとめる）。
