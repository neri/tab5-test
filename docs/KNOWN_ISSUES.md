# 既知の問題

> 索引: [`../DESIGN.md`](../DESIGN.md)

調査中にDW-GDMAチャンネルの停止方法に関する別の不具合を発見し、修正済みです。チャンネルが
転送中の場合、`CHEN0`（`DW_GDMA+0x18`）の有効ビットをクリアするだけでは確実に停止せず、
その後の再始動が不安定になります。正しくはESP-IDFの`dw_gdma_ll_channel_abort`と同じく
`CHEN1`（`DW_GDMA+0x1C`）へアボート要求を書き込み、完了をポーリングする必要があります
（この停止方式自体は現在のコードでは使用していません）。

SDHOST（SDMMCコントローラー）にも、ESP-IDFの実ドライバが一度も踏んでいないと
思われる実機固有の制約が2つ見つかっています（詳細と切り分け過程は
[`SD_CARD_PLAN.md`](SD_CARD_PLAN.md)のStage 2/3を参照）。

- `SDHOST_BUFFIFO_REG`へのCPU/APB直接読み出しはポップ動作をしない。
  `STATUS.FIFO_COUNT`はカードからの実データ到着どおりに増え続けるのに、
  固定アドレス・FIFO窓内でのインクリメントアドレスのどちらで読んでも同じ
  ワードが返り続ける。ESP-IDFは常に内蔵DMA（IDMAC）を使っており、この
  CPU直接読み出し経路を検証していないため、ドライバの誤りというより
  この経路自体が実機で機能しないと考えられる。ブロック読み書きは
  すべてIDMAC経由（`sdmmc.rs`の`read_block`/`read_blocks`/`write_blocks`）。
- DMA転送が実際に成功していても（`STATUS.FIFO_EMPTY`が転送後に1へ戻る、
  `RINTSTS`のDTOビットも正しく立つ）、`SDHOST_IDSTS_REG`のRI
  （Receive Interrupt）ビットは実機で一度も立たない。`SDHOST_CTRL_REG`の
  `int_enable`を含め試したが変化しなかった。DMA完了判定は`IDSTS`ではなく
  `RINTSTS.DTO`のポーリングで行っている。

## IDMACのバッファは64 byte境界から始まらなければならない

ROMのキャッシュ操作関数（`Cache_WriteBack_Invalidate_Addr`）は、開始アドレスが
64 byteのキャッシュライン境界にない範囲を**拒否します**（長さ側は内部で切り上げ
られるため整列不要）。`psram::writeback_invalidate`はこの可否を戻り値で返します。

拒否された場合、CPUはそのバッファのキャッシュ上のコピーを持ち続けます。DMAは
RAMへ正しく書いているのに、CPUが読むのは転送前の内容です。**ゼロ初期化した
バッファなら512 byte全部がゼロで返り、コマンドもDMAもエラーを出しません。**

`sdio.rs`のCMD53と`wifi/hosted.rs`は最初からこの契約を守っていましたが、SDカード側の
`read_block`／`read_blocks`／`write_blocks`／CMD6の`switch_func`は、呼び出し側の
スタック上の`[u8; N]`（Rust上のアラインメントは1）をそのままIDMACへ渡していました。
スタックのどこに置かれるかで成否が変わるため、同じ関数が呼び出し場所によって
正しく読めたり全ゼロを返したりします。`fs`層のMBR判定を追加したときに、
`blkread sd0 0`は正しく読めるのに`devices`は全ゼロという形で顕在化しました。

現在は`sdmmc.rs`が非整列のバッファを整列済みステージングバッファ経由で転送するので、
呼び出し側はアラインメントを意識しなくてよくなっています。加えてキャッシュ操作が
拒否された場合は転送失敗として扱い、黙って誤ったデータを返さないようにしました。
生の入口である`data_transfer_on`だけはステージングせず、非整列を拒否します
（`sdio.rs`の呼び出し側が既に整列を保証しているため）。

`dma2d.rs`の`Descriptor`もこの理由で`align(64)`にしてあります。`usb/hcd.rs`は
同じROM関数を使いますが、DMAが触るバッファに明示的なアラインメントを宣言しており、
実機受入も済んでいるため今回は変更していません。

## `hadris-fat` 2.1.0の追記がクラスタ1つ分を上書きする（2.2.0で修正済み）

`FileWriter::new_append`が、ファイル長がクラスタサイズの整数倍のときに書き込み位置を
**最終クラスタの先頭**に置いていました。

```rust
// 2.1.0
let offset_in_last = file_size % cluster_size;   // 整数倍なら 0
```

`write()`は`offset_in_cluster >= cluster_size`のときだけ新しいクラスタを確保するので、
この0では確保せず、既に埋まっている最終クラスタへ先頭から書きます。**追記のはずが
直前のクラスタ1つ分を上書きします。**

RAMディスクは8 MiBで`choose_layout`が`sectors_per_cluster = 1`を選ぶため
クラスタサイズが512 byteです。`Vfs::write`をハンドルへ4096 byteずつ呼ぶと、
2回目以降の全てが該当します。`fswritetest /tmp`が検査5（`handle: write 64 KiB`）で
`content differs at byte 3584`——64 KiBの最初の4096 byte書き込みの、最後の512 byte——
として捕まえました。

影響したのは`FileWriter::new_append`を使う経路、すなわち`Vfs::write`の2回目以降の
書き込みと、既存ファイルへの`Vfs::write_stream(Append)`です。1回の
`write_stream`の内側は影響しません——writerを1つ作って`write`を繰り返すだけで、
`new_append`を通らないためです。

2.2.0が整数倍のときに`offset_in_last = cluster_size`を置くよう修正しています
（コメントに`#B2`とあります）。`Cargo.toml`の要求を2.2.0へ上げてあり、これは
好みではなく**下限**です。

## VPDページ0x80の要求に標準INQUIRYを返すUSBメモリがある

実機のUSBメモリで確認しました。EVPDビットを立ててページ`0x80`
（Unit Serial Number）を要求すると、CHECK CONDITIONではなく**標準INQUIRYの応答**が
返ってきます。

これが厄介なのは、**応答からは区別できない**ことです。VPD応答のbyte 1はページ
コードを echo する規定なので`data[1] != page`で検査していましたが、標準INQUIRYの
byte 1はRMB（リムーバブル媒体ビット）で、USBメモリでは`0x80`です。要求した
ページコードと同じ値なので検査を素通りします。

さらに、要求したallocation length（64 byte）まで**デバイスが自分の内部バッファの
残りで埋めて**返します。そこには直前に読んだブロックが入っているため、返ってくる
バイト列が呼び出しのたびに変わります。実際に観測した2つは、後半がそれぞれMBRの
ブートコードと、FAT16ブートセクタの拡張BPBでした。

結果として、シリアル番号ではないものが媒体fingerprintに混ざり、**動いていない
USBメモリの同一性が読むたびに変わる**状態になっていました。`fsverify`が
`MEDIA CHANGED; mount dropped`を出し、書き込み前の媒体照合が全ての変更操作を
`media changed`で拒否します。

現在は**ページ`0x00`（サポートページ一覧）を先に取り、そこに載っているページだけ**を
要求します。一覧が取れない、または一覧として筋が通らない（先頭がページ`0x00`で
昇順になっていない）場合はVPDを使いません。このデバイスでは`inquiry`＋`mbr`＋`boot`
だけが情報源になり、いずれも安定しています。

切り分けには、fingerprintのソース別digest（`differing=`）と、接続中にunit serialが
変わったときのUARTログが効きました。どちらも残してあります——接続が続いている
あいだシリアルが変わるのは、デバイスが答えを作っているか転送層が壊しているかの
どちらかで、黙って指紋へ畳み込んでよいものではありません。

## 2 GiBを超えるFAT16はマウントできない

`hadris-fat`が1クラスタ32 KiBを超えるボリュームを開きません
（`BPB cluster size must not exceed 32 KiB`）。FAT16は最大65,524クラスタなので、
約2 GiBを超えるFAT16は64 KiBクラスタでしか作れず、そこで引っかかります。

実機では3823 MiB・MBRタイプ`0x06`のUSBメモリで踏みました。`usbmbr`と`devices`は
タイプバイトを表示するので`FAT16`と出ますが、マウント判定はブートセクタの中身を見るので
`not a readable filesystem`になります。**媒体は壊れていません。** 2 GiBを超える媒体は
FAT32でフォーマットしてください。

`src/fs/bootsector.rs`側の上限は64 KiBにしてあります。ドライバの制限に合わせて
「ブートセクタではない」と答えると、`mbr.rs`のsuperfloppy判定にも嘘をつくことに
なるためです。ドライバが断った場合はその理由をUARTへ出します。
