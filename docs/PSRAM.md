# PSRAM

> 索引: [`../DESIGN.md`](../DESIGN.md)

`src/psram.rs`が次の処理を担当します。

1. LDO2を1.8 Vに設定し、MSPI PHYの電源とクロックを有効化
2. 480 MHz SPLLを6分周し、PSRAMを80 MHzで駆動
3. MSPI3経由でモードレジスタを読み書き
4. コマンド経路の読み書き試験
5. DQS位相とdata/DQS delayの実機調整
6. `0x48000000`へ32 MiB（チップの64 MiB PSRAM MMU窓の半分。ECO2の
   `SOC_MMU_ENTRY_NUM`＝1024エントリ、1エントリ64 KiBに対し512エントリを使用）を
   MMU割り当て
7. キャッシュ経由の読み書き試験

フレームバッファは720×1280×2 byteで1,843,200 byteです。走査はシングルバッファ
なので、この1面だけを確保します。

DQS調整では、この実機で繰り返し選択された`phase=0, data=0, dqs=0`を最初に
100回読み出して検証します。合格時は31点の全探索を省略し、不合格時だけ従来の
フル探索へ戻ります。高速経路でも各候補に対するESP-IDFと同じ検査回数を使用します。

CPUが描画した内容をGDMAから参照できるよう、転送前にROMの
`Cache_WriteBack_Invalidate_Addr`をL1 DCache、L2 Cacheの順に呼び出します。
PSRAM、SD、USBの全呼び出しは`src/psram.rs`の
`iram_cache_writeback_invalidate`へ集約し、ROM処理中のcall/return経路が
IROM命令に依存しないようにします。同期中もLCDのframe ISRは動かすためmachine
interruptは許可したままですが、trap入口、ISR、参照定数、状態はIRAM/DRAM内に
閉じています。その後、既知画素を再読出しし、外部PSRAMへ同期されたことを確認します。

## ヒープ（グローバルアロケータ）

32 MiBの割り当てのうち残り約30.24 MiB（`Psram::heap`が返す、フレームバッファ
直後から割り当て末尾までの範囲）は`src/main.rs`のグローバルアロケータへ渡します。

`src/main.rs`は`extern crate alloc`を宣言し、`linked_list_allocator`crateの
`LockedHeap`（spinロック付き）を`#[global_allocator]`として静的に配置します。
初期化は`psram::init()`成功後、`psram.heap()`が返す`(*mut u8, usize)`で
`ALLOCATOR.lock().init(...)`を呼ぶだけで、`app::run`を呼ぶ前に完了します。
`psram::init()`が失敗した場合はヒープが初期化されないまま`app::run`を呼ばずに
待機ループへ入るので、`alloc`を使うコードは実行されません。

シェルの`alloctest <MiB>`コマンドは、この確保済みヒープから実際に
`Vec<u8>`を`try_reserve_exact`で確保し、インデックス由来のパターンを書き込んで
読み直すことで、PSRAM全域の読み書きを実機検証します（`src/app/shell.rs`）。
`mem`コマンドはヒープ容量も表示します。`MAPPED_BYTES - FRAMEBUFFER_BYTES`を
MiBへ切り捨てた値なので30 MiBと出ます（実際は31,711,232 byte＝約30.24 MiB）。
