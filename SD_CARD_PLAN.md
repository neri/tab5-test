# SDカードアクセス実装計画

## 方針

`DESIGN.md`の方針（ESP-IDFをリンクせずレジスタ操作で実装、1機能=1モジュール=
実機確認可能な単位でコミット）を踏襲する。SDMMC/SDIOは「ホスト初期化」「カード
活性化」「ブロックI/O」「ファイルシステム」の4層で実機依存の罠が異なるため、
一気に実装せず層ごとに動作確認しながら進める。

参考実装は`esp-idf-reference`の`sdmmc`/`esp_driver_sdmmc`コンポーネント
（`sdmmc_init.c`, `sdmmc_cmd.c`, `sdmmc_sd.c`, `hal/sdmmc_hal.c`）をレジスタ・
シーケンスの照合先として使う（リンクはしない）。

## Stage 0の結果（完了）

実機回路図（`Tab5_Schematics_PDF.pdf`、J2 `TF_CARD_SOCKET`/`SDIO1_*`ネット）で確認済み。

- インターフェース: SDIO1、IOMUX直結（GPIO Matrixを経由しない固定ピン）、4bit対応
- ピン割り当て: GPIO39=D0, GPIO40=D1, GPIO41=D2, GPIO42=D3, GPIO43=CLK, GPIO44=CMD
- カード電源: TFソケットのVDDは`SOC_3.3V`に直結（デカップリングのみ）。電源スイッチ無し、常時給電。電源制御GPIOは不要
- Card Detect（ソケットpin9）: SoC側GPIOへの配線が確認できず未接続。カード有無はコマンドタイムアウトで判定する

## 段階分け

### Stage 0: 配線・電源の確認 ✅ 完了（上記参照）

### Stage 1: SDMMCホスト初期化＋カード活性化 ✅ 完了（実機確認済み）

`src/sdmmc.rs`として実装。ESP-IDF v5.5.3の
`components/soc/esp32p4/register/hw_ver1/soc/sdmmc_reg.h`・`hp_sys_clkrst_reg.h`・
`lp_clkrst_reg.h`とドライバ`esp_driver_sdmmc/src/sdmmc_host.c`のレジスタ操作を
1:1で移植し、ポーリング・非DMAのコマンド送受信のみで活性化する。
`shell.rs`に`sdinfo`コマンドを追加し、RCA・カード種別（SDHC/SDXC or SDSC）・
容量（CSD v2のみ解析）・Manufacturer ID・write-protectをコンソールに、
生CID/CSDをUARTログに出す。

実機の実SDカードでCMD0〜CMD7の活性化が完走し、CIDのOID/PNMフィールドが
"SD"/"SD032"という妥当なASCII文字列としてデコードできることを確認した
（ランダム値ではなく実データを正しく読めている強い根拠）。

**踏んだ罠（教訓）**: `configure_pins()`でCMD/D0-D3にプルアップ（`fun_wpu`）は
有効にしたが、パッドの入力バッファそのものを有効にする`fun_ie`ビットの設定を
忘れていた。ESP-IDFの`gpio_iomux_output`はIOMUX経由のペリフェラル機能選択時に
`mcu_sel`しか書き換えず、出力イネーブルはペリフェラルが自動制御する
（`gpio_hal_iomux_out`のコメント）が、これは入力バッファには当てはまらない。
結果、CLK/CMDの送信（出力）は正常に動作しCMD0は「成功」するのに、カードからの
応答（受信）は常にゼロとしてしか読めず、`SDHOST_RINTSTS_REG`のResponse Error
ビットが立ち続けるという分かりにくい壊れ方をした。切り分けには
`RINTSTS`/`RESP0`/`STATUS`をクリア前に生ログへ残す診断コードが有効だった
（`DESIGN.md`のCPUクロック問題と同種の「実機でしか踏めない罠」）。
今後IOMUX経由で新しいペリフェラルのビディレクショナル信号を追加する際は、
`fun_wpu`だけでなく`fun_ie`も明示的に設定すること。

- クロック/GPIO_MATRIXまたはIO_MUXの設定、ホストペリフェラルのリセット
- 起動シーケンス: CMD0(GO_IDLE) → CMD8(SEND_IF_COND) → ACMD41(SD_SEND_OP_COND,
  タイムアウトポーリング) → CMD2(ALL_SEND_CID) → CMD3(SEND_RELATIVE_ADDR) →
  CMD9(SEND_CSD) → CMD7(SELECT_CARD)
- CSDからカード容量・転送速度、CIDから製造情報をデコードしてUARTへログ出力
- 動作クロックの決定（初期化は400kHz程度、活性化後に高速クロックへ切り替え）
- ゴール: `shell.rs`に`sdinfo`のようなコマンドを追加し、実カードの容量/CID/CSDが
  ログに出る

### Stage 2: 単一ブロック読み込み ✅ 完了（実機確認済み、当初計画からDMA方式に変更）

`shell.rs`に`sdread <lba>`コマンドを追加。CMD17(READ_SINGLE_BLOCK)で512byteを
読み、UARTへ32行の16進ダンプを出す。LBA 0で実行し、末尾に`55 AA`のMBR署名、
"Missing OS"/"Disk I/O Error"の文字列、実ブートコード（`FC 31 C0 8E D0 BC...`）が
正しく読めることを確認した。

**計画からの変更点**: 当初「DMA無しでOK」としていたCPU/APBポーリング方式
（`SDHOST_BUFFIFO_REG`を直接読む）は実機で動作しなかった。固定アドレス・
FIFO窓内でのインクリメントアドレスのどちらで読んでも、`STATUS.FIFO_COUNT`は
着実に増える（カードからは実データが届いている）のに読み出す値が最初の1ワード
から変わらないという壊れ方をした。ESP-IDF自身のドライバもCPU/APB経由の
FIFO読み出しを一度も使っておらず（常に内蔵DMA=IDMAC経由）、実機検証された
経路ではなかったと考えられる。そのためStage 3で予定していたDMA読み出しを
先に実装し、単一ブロックもDMA（IDMACの1発ディスクリプタ）で読む方式に変更した。

**踏んだ罠（教訓、DMA実装時）**:
- IDMAC用のディスクリプタ・転送先バッファはPSRAMと同じくL1/L2キャッシュの
  対象（`SOC_CACHE_INTERNAL_MEM_VIA_L1CACHE=1`）。`psram.rs`と同じROM関数
  `Cache_WriteBack_Invalidate_Addr`で、DMA起動前はwriteback（CPU側の変更を
  RAMへ反映）、DMA完了後はinvalidate（CPUが新しい内容を読めるようにする）が
  必要。転送先バッファはDMA前にも一度writebackしておかないと、無関係な
  キャッシュ追い出しがDMA書き込み後のRAMを古い内容で上書きする恐れがある
  （ESP-IDFの`esp_cache_msync`呼び出し箇所と対応）。
- `SDHOST_CTRL_REG`の`dma_enable`(bit5)・`use_internal_dma`(bit25)は
  `sdmmc_reg.h`の一部ビットにしか名前が付いておらず、`sdmmc_struct.h`の
  ビットフィールド定義でしか位置が分からなかった。
- `SDHOST_CARDTHRCTL_REG`（カード読み取りしきい値）はESP-IDFが一度も
  触っていないレジスタだが、これを`CARDRDTHREN=1`・しきい値=ブロックサイズで
  設定しないと、DAT線からFIFOへのデータ到達（CPUポーリングでは確認できた）は
  起きてもIDMACのバースト転送が始まらなかった。
- 設定を修正した後も`SDHOST_IDSTS_REG`のRI（Receive Interrupt）ビットは
  実機で一度も立たなかった（`SDHOST_CTRL_REG.int_enable`を含め試したが不変）。
  一方`SDHOST_RINTSTS_REG`のDTO（Data Transfer Over）ビットは正しく立ち、
  `STATUS.FIFO_EMPTY`もFIFOが実際に空になったことを示していた＝転送自体は
  成功しているのにIDMAC側のステータスラッチだけが機能しない、という
  ECO2固有と思われる制約。そのため完了判定は`IDSTS`ではなく`RINTSTS.DTO`
  ポーリングに変更した。

以上より、**DMA関連のレジスタ設定はESP-IDFの`sdmmc_host.c`/`sdmmc_ll.h`との
1:1対応を強く優先し、独自に省略・簡略化した箇所（`CARDTHRCTL`など）から
順に疑うとよい**、というのが今回最大の教訓。

### Stage 3: DMA経由の複数ブロック読み書き ✅ 完了（実機確認済み）

Stage 2の単一ディスクリプタDMA基盤（`Descriptor`構造体、`init_dma`、
キャッシュwriteback/invalidate、`CARDTHRCTL`設定）を拡張し、複数ディスクリプタを
チェーン（`second_address_chained`+`next_desc_ptr`、最大8個=64KiB）して
CMD18(READ_MULTIPLE_BLOCK)/CMD25(WRITE_MULTIPLE_BLOCK)を実装した。
STOP_TRANSMISSION(CMD12)は手動送信ではなく`CMD_SEND_AUTO_STOP`ビットで
ハードウェアに自動送信させている。

`shell.rs`に3コマンド追加:
- `sdreadn <lba> <n>`（n<=8、複数ブロック読み込み、破壊的操作なし）
- `sdwritetest <lba>`（1ブロックをテストパターンで上書き→検証→元データへ復元する
  ラウンドトリップテスト。**write系コマンドは唯一の実データ書き込みを伴う操作**）
- `sdzero <lba>`（1ブロックを明示的にゼロで埋める、ラウンドトリップなし）

実機確認: `sdreadn 0 4`でLBA0(MBR)〜LBA3(exFAT起動セクタの一部、"EXFAT   "の
OEM名文字列を含む)まで4ブロックがそれぞれ異なる正しい内容で読めた
（チェーンディスクリプタが正しく機能している強い根拠）。書き込みはMBRと
パーティション先頭の間の未使用領域（LBA1、実機で確認済み）を使い、
`sdwritetest`→`sdzero`で最終的に元のゼロ状態へ復元できることを確認した。

**踏んだ罠（教訓）**: `write_blocks`実行後、データ転送自体（`RINTSTS.DTO`）は
完了してもカードは内部でフラッシュへの書き込み（プログラミング）を継続しており、
`STATUS.DATA_BUSY`（DAT0がLow）が立ったままになる。この状態で次のコマンドを
即座に送るとカードが応答せず`RINTSTS`のRTO（Response Timeout）で失敗する。
Stage 1でCMD7後に使っていた`wait_data_not_busy()`を、`transfer_blocks`の
末尾（読み込み・書き込み両方）でも呼ぶよう修正して解決した。タイムアウトも
CMD7用の短い値（約2.8ms）のままでは書き込みには全く足りないため、SD規格の
上限（ブロックあたり最大250ms）を見て約550msまで拡大した。

この罠を実機で踏んだ際、`sdwritetest`の最初の実行（修正前）がrestoreの
書き込みで失敗し、テスト対象LBAにテストパターンが残った。`sdwritetest`は
「実行開始時点で読んだ内容」を"original"として復元するだけなので、
既に壊れた状態から実行すると同じ壊れた内容を"復元"してしまう
（today検証用に`sdzero`を追加して手動で復元した）。**書き込み系コマンドを
使う前は、対象LBAが本当に無害な領域か（`sdreadn`で事前確認する等）を
必ず確認すること。**

### Stage 4: パーティション/ファイルシステム

- 4a: MBR解析（パーティションタイプ、開始LBA/セクタ数）
- 4b: FAT BPBパース（FAT16/FAT32を想定、SDXCならexFATの可能性も確認）
- 4c: ルートディレクトリ/クラスタチェーン走査によるファイル一覧・読み込み
- 4d: 書き込み（空きクラスタ確保、FAT更新、ディレクトリエントリ更新）は
  読み込みが安定してから着手。壊れたFAT実装は容易にカードのデータを破壊するため、
  最初はテスト用カードで検証する
- ゴール: `shell.rs`に`ls`/`cat`相当のコマンドを追加し、実際にPCで書き込んだ
  ファイルが読める。書き込みは最後にテストカードで検証

## モジュール構成（予定）

- `src/sdmmc.rs`: ホスト初期化・カードコマンド送受信・活性化シーケンス
  （`gpio.rs`/`i2c.rs`と同じ階層で、ペリフェラル固有の独立モジュールとする）
- `src/sdcard.rs`: `sdmmc.rs`の上に載るブロックデバイス層（Stage 2/3）
- `src/fat.rs`: `sdcard.rs`の上に載るファイルシステム層（Stage 4）

## 各段階の完了条件（実機確認）

各StageはUARTシェル経由でコマンドを叩いて目視確認できることをもって完了とし、
次のStageへ進む前に必ず実機でログを確認する（`DESIGN.md`のCPUクロック問題の
教訓どおり、シミュレーションではなく実機でのみ踏める罠がSDMMCにも多いため）。
