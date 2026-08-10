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
（検証用に`sdzero`を追加して手動で復元した）。**書き込み系コマンドを
使う前は、対象LBAが本当に無害な領域か（`sdreadn`で事前確認する等）を
必ず確認すること。**

### 追加: 4bitバスモード ✅ 完了（実機確認済み）

CMD7(SELECT_CARD)の直後にACMD6(CMD55+CMD6, 引数0b10)でカードを4bit幅へ切り替え、
ホスト側は`SDHOST_CTYPE_REG.CARD_WIDTH4`（card0）を1に設定する
（`sdmmc.rs`の`set_bus_width_4bit`）。ピン設定はStage 0/1で既にD0〜D3全4本を
設定済みだったため、変更不要だった。ACMD6が失敗しても`init()`全体は失敗させず
1bitのまま継続する（`SdCard.bus_width_4bit`で呼び出し側に伝える）。
実機で`sdreadn 0 4`が4bitモードでも1bit時と同じ正しい内容を読めることを確認した。

`sdmmc::dump_block`/`dump_block_at`のUARTダンプは`hexdump -C`風に
4桁オフセット＋ASCII表示を追加し、複数ブロックにまたがる場合もオフセットが
連番になるよう変更した（デバッグ時の可読性向上、SD自体の話ではないが
同じタイミングで実装）。

### 追加: クロック高速化（Default Speed, 20MHz） ✅ 完了（実機確認済み）

活性化（CMD0〜CMD7、4bit切り替え）は400kHzの識別クロックのまま行い、
`init()`の最後で`set_card_clock(8, 0)`（160MHz/8=20MHz、card_div=0で
2段目の分周をバイパス）に切り替える。SD規格のDefault Speed上限は25MHzだが、
ESP-IDF自身の`SDMMC_FREQ_DEFAULT`もこの20MHzという分周値を採用しており、
それに合わせた（160MHzをきれいに割れる値を優先し、上限ぴったりは狙わない）。
`SdCard.clock_khz`で呼び出し側に伝える。

**4bitモードとの非対称性（教訓）**: ACMD6のバス幅切り替えは失敗しても
1bitのまま継続できる（`init()`は`None`を返さない）が、クロック切り替えは
そうできない。`set_card_clock`は切り替えの最初に`CLKENA`でカードクロックを
一旦無効化してから再設定する実装なので、途中の`update_clock_registers`
（擬似コマンド）が失敗すると、クロックが無効化されたまま返ってきてしまい
「前の速度のまま継続」にはならない。そのため`set_card_clock`の失敗は
（Stage 1のクロック初期設定と同じく）`init()`全体を失敗させる致命的エラーとして
扱っている。実機で`sdreadn 0 4`が20MHz・4bitでも以前と同じ正しい内容
（MBR署名、exFAT OEM名）を読めることを確認した。

### 追加: High Speed（50MHz、CMD6 SWITCH_FUNC） ✅ 完了（実機確認済み）

CMD6(SWITCH_FUNC)はこれまでのコマンドと異なり、48bitの単純な応答ではなく
**64byteのステータスブロックをDATライン経由で読み返す**特殊なコマンドである
（`sdmmc.rs`の`switch_func`。`read_block`と同じ単発ディスクリプタDMAの仕組みを
BLKSIZ=64で再利用）。まずチェックモード（引数最上位bit=0）でCMD6を送り、
返ってきたステータスのbit415:400（実装上は`status[12..13]`のbig-endian
16bit値として直接読む）でFunction Group1（Access Mode）のサポートビットを見て
High Speed（値1）対応かを確認する。対応していればスイッチモード
（最上位bit=1、group1=1）で実際に切り替え、応答のbit379:376
（`status[16]`の下位nibble）で選択された機能が本当に1になったかを検証し、
最後にホスト側クロックを`set_card_clock(4, 0)`（160MHz/4=40MHz）へ上げる。

ステータスブロックのバイト位置はESP-IDFの`components/sdmmc/include/sd_protocol_defs.h`
（`SD_SFUNC_SUPPORTED`/`SD_SFUNC_SELECTED`マクロ、`MMC_RSP_BITS`のbit numbering）
と1:1で対応することを確認済み。ただしESP-IDFは32bitワード配列＋
`sdmmc_flip_byte_order`（ワード順反転＋バイトスワップ）という受信バッファの
持ち方をしているのに対し、こちらはDMAが生のワイヤ順でバイト列を書き込む
（`read_block`のMBR確認で実証済みの前提と同じ）ため、同じビット位置を
直接バイトオフセットに変換して読んでいる（word/byteスワップ不要）。

**非対応カードへの配慮**: チェックモードで非対応と分かった場合、または
スイッチモードが失敗・拒否された場合は、クロックには一切触れずDefault Speed
(20MHz)のまま`init()`は成功を返す（`SdCard.high_speed=false`）。逆に
カードがスイッチを受理した後でホスト側クロック変更（`set_card_clock`）が
失敗した場合は、Default Speedへのクロック切り替えと同じ理由（クロックが
無効化されたまま返る）で`init()`全体を致命的エラーとして失敗させる。

実機で複数枚（自宅にあった手持ちのSDカード数枚）を試し、全てHigh Speedに
対応しており、切り替え後も`sdreadn`で正しい内容が読めることを確認した。

### 追加: SD→PSRAM直接DMA転送 ✅ 完了（実機確認済み）

`sdmmc::read_blocks`/`write_blocks`はディスクリプタの転送先アドレスを
渡されたスライスのポインタからそのまま生成するだけで、内蔵SRAMかPSRAMかで
分岐する処理は元々無かった。そのため「動くかどうか」は実装ではなく
IDMAC（SDHOST内蔵DMA）がPSRAMのキャッシュマップ済みウィンドウ
（`0x4800_0000`〜、`psram.rs`のPSRAM_VADDR）へバス到達できるかという
ハードウェア側の問題だった。

検証用に`shell.rs`の`sdreadpsram <lba> <n>`コマンドを追加した。同じブロックを
①スタック（内蔵SRAM）バッファと②PSRAMヒープ上の`Vec`（`extern crate alloc`＋
`linked_list_allocator`、`Psram::heap()`で確保）の両方へ、同一の
`sdmmc::read_blocks`経路でDMA読み込みし、バイト単位で一致するか比較する
（目でダンプを見比べるのではなく自動判定）。実機で一致（`match`）を確認し、
**IDMACはPSRAMへ直接DMA転送できる**ことが分かった。内蔵RAM経由のコピーは
不要。キャッシュのwriteback/invalidateは既存の`cache_writeback_invalidate`
（PSRAMかSRAMかで分岐しない、アドレス指定のROM関数呼び出しのみ）がそのまま
機能した。

この結果により、将来大きなファイルをSDから読んでPSRAM上で扱う（画像表示など）
実装は、`read_blocks`を呼ぶ側がPSRAMヒープ上のバッファを渡すだけで済み、
`sdmmc.rs`側の追加対応は不要と分かった。

**補足（microSDとHigh Speedの関係）**: High Speed（50MHz）を定義したSD
Physical Layer Spec 1.10は2003年策定。microSD規格自体（旧TransFlash）が
SD Associationで標準化されたのは2004〜2005年頃で、High Speedが規格に
入った後に生まれたフォーマットである。そのため「非対応のmicroSD」は
実質存在せず、フルサイズSDカードに存在する2000〜2003年頃の本当に古い
カードとは事情が異なる。とはいえ模造品・容量詐称品のファームウェアが
規格に忠実とは限らないため、CMD6チェック→フォールバックの仕組みは
保険として残す。

### Stage 4a: MBRパーティションテーブルの簡易表示 ✅ 完了（実機確認済み）

ファイルシステム対応（4b以降）は一旦保留し、パーティションテーブルの
情報表示だけを`shell.rs`の`sdmbr`コマンドとして実装した。LBA 0を読み、
446バイト目からの4エントリ（起動フラグ・種類バイト・開始LBA・セクタ数、
各16バイト）と510-511バイト目の`55 AA`署名を見るだけの、GPTもFATも
一切解釈しない最小実装。種類バイトは主要なもの（FAT12/16/32、NTFS/exFAT、
Linux、GPT保護MBRの0xEEなど）だけ短い名前に変換し、その他は"unknown"と
表示する。GPT保護MBR（0xEE）を検出した場合はGPT自体を解析しない旨だけ
表示する。実機で動作確認済み。

### Stage 4（残り）: ファイルシステム — 保留

4b以降（FAT/exFATの実解釈、ファイル読み書き）はユーザーの指示により
一旦後回し。着手する際の方針は変更なし:

- 4b: FAT BPBパース（FAT16/FAT32を想定、SDXCならexFATの可能性も確認）
- 4c: ルートディレクトリ/クラスタチェーン走査によるファイル一覧・読み込み
- 4d: 書き込み（空きクラスタ確保、FAT更新、ディレクトリエントリ更新）は
  読み込みが安定してから着手。壊れたFAT実装は容易にカードのデータを破壊するため、
  最初はテスト用カードで検証する
- ゴール: `shell.rs`に`ls`/`cat`相当のコマンドを追加し、実際にPCで書き込んだ
  ファイルが読める。書き込みは最後にテストカードで検証

## モジュール構成（実際）

当初`src/sdcard.rs`（ブロックデバイス層）・`src/fat.rs`（ファイルシステム層）を
別モジュールにする想定だったが、実際にはStage 2/3のブロックI/Oも
`sdmmc.rs`にそのまま実装した（ホスト初期化とブロックI/Oが同じレジスタ・
同じDMA基盤を共有しており、分ける明確な境界が無かったため）。

- `src/sdmmc.rs`: ホスト初期化・カード活性化（4bit/High Speed含む）・
  DMA経由のブロック読み書き。すべてこのファイルに実装
  （`gpio.rs`/`i2c.rs`と同じ階層で、ペリフェラル固有の独立モジュールとする）
- `src/shell.rs`: 各`sdXXX`コマンド。MBRパース（`sdmbr`）もここに直接実装
  （Stage 4bでFAT対応する際は、`sdmmc.rs`とは別に`src/fat.rs`を切り出す
  想定を維持する）

## 各段階の完了条件（実機確認）

各StageはUARTシェル経由でコマンドを叩いて目視確認できることをもって完了とし、
次のStageへ進む前に必ず実機でログを確認する（`DESIGN.md`のCPUクロック問題の
教訓どおり、シミュレーションではなく実機でのみ踏める罠がSDMMCにも多いため）。
