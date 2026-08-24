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
