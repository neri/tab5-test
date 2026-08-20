# PSRAM

> 索引: [`../DESIGN.md`](../DESIGN.md)

`src/psram.rs`が次の処理を担当します。

1. LDO2を1.8 Vに設定し、MSPI PHYの電源とクロックを有効化
2. MPLLを400 MHzへ調整し、2分周した200 MHzでPSRAM初期化を試行
3. MSPI3経由でモードレジスタを読み書き
4. コマンド経路の読み書き試験
5. DQS位相とdata/DQS delayの実機調整
6. 複数アドレスをwalking bitと反転パターンでdirect command経由から検査
7. `0x48000000`へ32 MiB（チップの64 MiB PSRAM MMU窓の半分。ECO2の
   `SOC_MMU_ENTRY_NUM`＝1024エントリ、1エントリ64 KiBに対し512エントリを使用）を
   MMU割り当て
8. キャッシュ経由でも同じ境界を検査し、各書込みをPSRAMへwritebackしてから再読出し
9. 200 MHzのいずれかの段階が失敗した場合は、MSPI reset後にSPLL 480 MHz÷6の80 MHz
   profileをmode registerから再初期化

フレームバッファは720×1280×2 byteで1,843,200 byteです。走査はシングルバッファ
なので、この1面だけを確保します。

DQS調整では、80 MHz profileだけ、この実機で繰り返し選択された
`phase=0, data=0, dqs=0`を最初に100回読み出して検証します。合格時は31点の全探索を
省略し、不合格時だけ従来のフル探索へ戻ります。200 MHzでは80 MHzの既知点を流用せず、
毎bootで4 phaseと31 delay点を全探索します。各delay候補に対する検査回数はESP-IDFと
同じ100回です。

周波数によって変わるclock source、動作時／調整時divider、read/write/register dummy、
mode registerのread/write latency code、既知のDQS候補は`PsramTiming`へ集約します。
`PSRAM_200_MHZ`はESP-IDF v5.5.3のAP Memory Hex-PSRAM設定と同じMPLL 400 MHz÷2、
fixed read latency 14 cycle、write latency 7 cycle、read/write/register-read dummy
26/12/12 bit、調整時divider 20です。`PSRAM_80_MHZ`は従来と同じSPLL 480 MHz÷6、
read latency 10 cycle、write latency 5 cycle、dummy 18/8/8 bit、調整時divider 24です。

初期化成功時は、選択profileと実際に採用した調整点をUARTへ出します。

```text
PSRAM: profile MHz=0x000000C8
PSRAM: read latency cycles=0x0000000E
PSRAM: write latency cycles=0x00000007
PSRAM: DQS phase=0x...
PSRAM: DQS data delay=0x...
PSRAM: DQS delay=0x...
```

全探索した場合は、採用点に加えて`DQS window start`と`DQS window length`も表示します。
採用点はこの連続合格windowの中央です。window長1は不合格にして80 MHzへfallbackするため、
単一点だけの偶然の読出し成功を200 MHz profileとして採用しません。

200 MHz側が失敗したときは`PSRAM: 200 MHz failed stage=0x...`と
`PSRAM: falling back to 80 MHz`を出した後、成功した80 MHz profileだけをreadyログに
表示します。stageは1=clock、2〜5=mode register、6=初回command path、7=DQS調整、
8=direct memory test、9=MMU、10=cache mapping testです。fallbackも失敗した場合は
`ready`を出さず、アプリとPSRAM heapを開始しません。

`pf`コマンドはLP scratch registerへ1回限りのmarkerを書いてCPUを再起動します。次の起動は
200 MHzのDQS全探索まで実行してから診断用stage 7失敗を注入し、実際の80 MHz fallbackを
通ります。markerはPSRAM初期化前に消去されるので、fallback中の再起動や次回bootを
恒久的な80 MHzループへ閉じ込めません。

実機では200 MHzの全探索が`start=5, length=24`の連続DQS windowを返し、`alloctest 30`が
30 MiB全域を不一致なしで完走しました。`pf`によるstage 7の強制失敗後も、同一boot内で
80 MHz、read/write latency 10/5 cycle、DQS 0/0/0へ戻り、`ready`まで到達しています。
続く`rt`では200 MHz profileで20回のCPU-only rebootを完走し、各bootのpost-XIP probe、
heap初期化、display scanout開始まで20/20で到達しました。
走査継続中の`membench`も完走し、cached PSRAMの逐次write/readは61/87 MB/s、64-byte
line write/readは983/537 nsでした。80 MHz baselineの20/38 MB/s、3019/1350 nsに対し、
周波数引上げに対応する改善が確認できています。
完全な電源OFFを挟むコールドブートも10/10で200 MHz profile、post-XIP probe、heap、表示開始に
成功しました。CPU-only reboot 20/20と合わせ、Stage 2のboot耐久条件を満たしています。

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
