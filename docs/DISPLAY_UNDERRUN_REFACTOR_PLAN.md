# 表示アンダーラン対策リファクタリング計画

> 索引: [`../DESIGN.md`](../DESIGN.md)
> この文書は作業計画と実機での判断記録です。現在の実装仕様は現状文書と
> コードを優先してください。

## 状態: Stage 0〜4完了（全受入条件合格）

## 実装状況（2026-08-20）

- `Framebuffer`のproduction経路から、強制CPU raw fillとcache同期なしPPA raw fillを分離
- `ppafill ... cpu`と`ppafill sweep`のCPU列を強制CPU経路へ変更し、768画素以上がPPAへ
  戻っていた測定バグを修正
- `displaybench`を追加し、`idle`、`sync`、`cpu`、`ppa-raw`、`ppa-safe`、`production`を
  固定回数で測定可能にした
- frame境界から0/3/8/12 msの開始位相と、8/16/32/64/128 byteのDMA2D burstを診断時だけ
  指定可能にした。診断後は元のburstへ戻す
- 各操作後にsticky underrun bitを回収し、平均時間、完了数、経過frame、underrunした
  操作数を表示する
- 各描画後は次のframe境界まで待ってからunderrun bitを回収し、処理完了後の同一frame
  後半で発生したアンダーランも直前の操作へ帰属させる
- `cargo fmt --check`、`cargo check`、release build、ELF配置検査、ESP image検査に合格
- Stage 1 release配置はIRAM 6,840 byte、DRAM rodata 1,076 byte、DROM 130,776 byte、IROM
  186,078 byte、残りstack 181,760 byte。ESP imageは344,224 byte、XIP 2本＋RAM 2本
- 修正版`ppafill sweep`を実機確認。全画面CPU 93.548 ms、PPA 13.267 msとなり、旧CPU列が
  PPAへ迂回していた問題が解消した
- `displaybench`の全caseが`completed=100`。idleとcache同期はunderrun 0/100、CPUと
  PPA raw/safe/productionは100/100
- PPA safeの開始位相0/3/8/12 msはすべて100/100。burst 8/16/32/64/128 byteもすべて
  100/100で、短いburstほど16.350 msから60.222 msまで遅くなった
- cache同期なしPPA rawが100/100なので、残る主因をPPAのPSRAM書き込みと走査読み出しの
  競合と判定。Stage 1へ進む
- 標準13 caseを一括実行する短縮コマンド`db [count]`を追加。既定は100回でICM 15/15も
  コマンド内で設定する
- Stage 1として周波数依存値を`PsramTiming`へ集約し、既存値だけを持つ
  `PSRAM_80_MHZ`を追加。clock source、動作／調整divider、dummy、latency code、DQS既知点を
  profile経由に変更した。周波数とレジスタ値は変更していない
- 起動時にprofile MHz、read/write latency、選択DQS phase/data/dqs delayをUART表示する
- Stage 1版も`cargo fmt --check`、`cargo check`、release build、ELF配置検査、ESP image検査に
  合格。flash-critical closure 134 relocationはすべてRAM/ROM内
- Stage 1実機でprofile 80 MHz、read/write latency 10/5 cycle、DQS 0/0/0を確認。
  `db 100`と再起動後の`db 20`はいずれもStage 0と同じ傾向で、idle/syncは0件、
  CPU/PPA系は全操作でunderrunした。profile化による回帰なしとしてStage 1を完了
- ESP-IDF v5.5.3の`esp_psram_impl_ap_hex.c`とESP32-P4 clock low-levelを照合し、
  `PSRAM_200_MHZ`をMPLL 400 MHz÷2、read/write latency 14/7 cycle、dummy 26/12/12 bit、
  MR0/MR4 code 4/1で追加
- 200 MHzは既知DQS点を使わず毎boot全探索する。選定後にPSRAM先頭、64 KiB境界、
  framebuffer境界、heap中間、32 MiB末尾をdirect commandとcache mappingの両方で
  walking/invert pattern検査する
- 200 MHzのclock、mode register、command、DQS、direct test、MMU、mapped testのいずれかが
  失敗した場合、失敗stageを出して同じboot内でMSPI reset後に80 MHzを再初期化する
- Stage 2版もformat/check/release build、ELF配置検査、ESP image検査に合格。最新配置は
  IRAM 9,104 byte、DRAM rodata 1,364 byte、残りstack 179,200 byte、flash-critical closure
  182 relocationはすべてRAM/ROM内。app imageは349,408 byte
- Stage 2実機で200 MHz profileをfallbackなしで選択。read/write latency 14/7 cycle、
  DQS phase/data/dqs=`0/1/0`、direct/cache両検査を通過してheapとframebufferを開始した
- 200 MHzの`db 20`で、production b128は5.989 ms・0/20、PPA rawは5.571 ms・0/20、
  CPUは30.109 ms・0/20。phase 0/3/8/12 msとburst 32/64/128 byteはすべて0/20。
  診断専用burst 8/16 byteだけ20/20でunderrunし、32 byteに明確な境界がある
- `db`が正常な全画面試験色にBLACK/BLUEを使っていたため、濃青の正常frameとBridgeの
  水色underrunを混同しやすかった。試験色をBLACK/REDへ変更し、production b128だけを
  短く反復する`dp [count]`を追加
- 200 MHzの全探索時に連続合格DQS windowのstart/lengthも起動ログへ追加。採用点が
  window中央で、単一点合格ではないことを実機ログだけで確認可能にした
- 200 MHzで`dp`既定100回を実行し、production b128は平均5.989 ms、underrun 0/100。
  `alloctest 30`も30 MiBの書込み・再読出しを不一致なしで完走した
- 同じ実機のDQS全探索はstart 5、length 24点の連続windowとなり、31候補中24点が合格。
  採用点はwindow中央にあり、単一点だけの偶然合格ではないことを確認した
- `pf`で有効な200 MHz調整後にstage 7失敗を注入し、同一bootで80 MHz profile
  （latency 10/5、DQS 0/0/0）へ再初期化してheapとframebufferを開始できた
- `rt [count]`を追加。LP scratch registerに総数と残数を保持し、PSRAM初期化、post-XIP
  probe、heap、display scanoutまで到達したbootだけを数えて自動再起動する。既定20回で、
  最終bootだけ`REBOOT TEST PASS`を表示する。途中bootが80 MHzへfallbackした場合は
  成功数に含めずFAIL終了する
- `mix`追加後もformat/check/release build、ELF/ESP image検査に合格。最新配置はIRAM
  9,092 byte、DRAM rodata 1,364 byte、IROM 209,598 byte、残りstack 179,200 byte。
  app imageは370,272 byte、XIP segmentは2本のまま
- 実機で`rt`既定20回を完走し、全bootが200 MHz profile、post-XIP probe、heap、display
  scanout開始まで到達。最終結果`REBOOT TEST PASS: 20/20`を確認した
- 200 MHzの`membench`を走査継続中に完走。cached PSRAMは逐次write u32/u16が61/60 MB/s、
  read u32が87 MB/s、64-byte line write/readが983/537 ns、4 KiB scatter readが676 ns。
  80 MHz比で逐次write約3倍、read約2.3倍、line write/read約3.1/2.5倍へ改善した
- 完全な電源OFFを挟むコールドブート10回を実機で行い、全回で200 MHz profile、PSRAM
  ready、post-PSRAM DROM/IROM probe、正常な画面起動を確認。10/10合格としてStage 2を完了
- 200 MHzで`db`既定100回を完走。idle、sync、CPU、PPA raw/safe、production、全開始位相、
  burst 32/64/128 byteはすべてunderrun 0/100。診断専用burst 8/16 byteだけ100/100となり、
  初回20回と同じ32 byte境界を再現した。production b128は5.989 ms、0/100
- 30分idle試験を短い`di`コマンドへまとめた。既定103,200 frameを1 frameごとにsticky
  underrun回収し、ICM 15/15、完了frame数、発生数を最後に表示する
- 実機の`di`既定30分は103,200/103,200 frameを完走し、平均17.468 ms、underrun 0件。
  起動直後だけでなく長時間の走査単独でもFIFOに余裕があることを確認した
- `ui`を追加。実際のconsole cell/DMA2D経路で100回scrollをframe単位に計数した後、
  coordinate、paint、touch、axis、desktopを1コマンドで順に開く。各画面とconsole復帰の
  sticky underrunおよびDMA errorを最終行へ集計する
- 実機の`ui`はconsole scroll 100/100、underrun 0件。coordinate、paint、multi-touch、
  axis、desktopの描画・操作・console復帰にも見た目の問題はなく、visual underrun 0件、
  DMA error 0件で完走した
- 最後の複合負荷を`mix [minutes]`へまとめた。既定120分、走査継続中に毎秒production
  全画面fill、SD/USB MSC各4 KiBのread/比較を行い、毎foreground loopでPSRAM heapの
  4 KiB stripeを書込み・writeback-invalidate・再読出しする。外部mediaはLBA 0から
  読むだけで、一切writeしない
- `mix 1`実機試験は表示2,138 frame、heap検査2,110回、DPI underrun 0、display DMA error 0で
  表示・PSRAM・SDは正常だったが、USB MSCの4 KiB readが37回後にBulk IN timeoutとなった。
  portはconnected/enabled/powered、SOFも継続していたため、表示帯域問題とは分離したUSB BOTの
  既存復旧不足と判定した
- USB Bulk timeoutを360 MHz時約444 msの固定値からCPU周波数追従の約5秒へ変更。BOT command失敗時の
  Mass Storage Reset、IN／OUT halt解除、DATA0同期と、read-only READ(10)の1回再送を追加した。
  USB単独の短縮確認は`ut`既定100回とし、その合格後に`mix 1`、最後に`mix`既定120分へ進む
- 第1版`ut`は最初のtimeoutをBOT Recoveryして継続したが、2回目のRecoveryでEP0 IN statusが
  timeoutし92/100、retry 1、failure 1、mismatch 0となった。control側の固定timeoutが360 MHzで
  約44 msまで短縮されていたため、controlもCPU周波数追従の約1秒へ変更した。Mass Storage Reset
  失敗時は後続のhalt解除を打ち切り、未知状態へcontrol requestを重ねない
- 第2版`ut`はCBW OUTのQTD status 1からRecoveryした後、再送READ(10)のBulk INとRecovery controlも
  status 1となり91/100、command retry 1、failure 1、mismatch 0だった。ESP-IDF 5.5.3でstatus 1は
  CRC／timeout／stuff／false EOP／excessive NAKをまとめたpacket errorであり、BOT破綻ではない。
  第3版ではtoggleを進めない同一DATA PIDのpacket再送を50 ms間隔・最大20回追加し、使い切った
  場合だけBOT Resetへ進む。`ut`／`mix`はpacket retryとcommand retryを別々に表示する
- USB Recovery第3版もformat/check/release build、ELF/ESP image検査に合格。IRAM 9,092 byte、
  DRAM rodata 1,364 byte、IROM 214,642 byte、stack 179,200 byte、flash-critical relocation 182件は
  すべてRAM/ROM内。app imageは375,328 byte、XIP 2本＋RAM 2本を維持した
- 第3版`ut`は約91 commandで5秒のBulk IN timeoutを3回発生。BOT Recovery後のREAD(10)を2回
  再送できたが91/100、failure 1、mismatch 0、packet retry 0、command retry 2でFAILし、その後
  deviceがroot portから切断された。4 KiBをHigh-Speed MPS 512 byteごとのQTDへ分割する実装では
  約1,000回channelを再起動していた
- USB Recovery第4版は4 KiB Bulk INを1 QTDへ集約し、短いCSW／INQUIRY／capacity応答を
  MPSサイズの内蔵SRAM staging経由へ変更。複数packet QTDのstatus 1は全体再送せずBOT Resetへ
  進む。format/check/release build、ELF/ESP image検査に合格し、IRAM 9,092 byte、DRAM rodata
  1,364 byte、IROM 213,456 byte、stack 179,200 byte、app image 374,128 byte、XIP 2本＋RAM 2本を
  維持した
- 第4版`ut`も91/100でBulk IN timeoutとなり、QTD集約前と発生位置が変わらなかった。最初の
  BOT Recovery後のREAD(10)もtimeoutし、2回目のRecoveryは両endpoint halt解除のIN statusで
  status 1となった。failure 1、mismatch 0、packet retry 0、command retry 1。channel再起動数を
  主因とした仮説は棄却し、次は`usbfs on`との比較でHigh-Speed固有性を判定する
- Full-Speed切替依頼後の次の`ut`は36/100で複数packet Bulk IN QTDがstatus 1となったが、旧出力では
  強制modeとMPSを確認できずFull-Speed動作を断定できない。第5版は`ut`冒頭へmode/MPSを追加し、
  status 1時はQTD残量から完全受信済みMPS packetと次DATA PIDを復元して未受信suffixだけを再投入する
- USB Recovery第5版もformat/check/release build、ELF/ESP image検査に合格。IRAM 9,092 byte、
  DRAM rodata 1,364 byte、IROM 214,738 byte、stack 179,200 byte、app image 375,424 byteで、
  XIP 2本＋RAM 2本とflash-critical relocation 182件のRAM/ROM閉包を維持した
- mode/MPS表示を`mix`へ誤挿入しており`ut`に出ない実装ミスを修正。第5版相当の`ut`は91/100、
  5秒timeout 3回、failure 1、mismatch 0、packet retry 0、command retry 2で、status 1用partial
  再投入はtimeout経路を改善しなかった
- 第6版はBulk QTDを約1秒で区切り、timeout時もQTD残量から完全MPS prefix、次DATA PID、未受信
  suffixを復元して同じBOT phase内で最大4回再投入する。合計約5秒後だけBOT Resetへ昇格し、
  mode/MPS表示は`ut`／`mix`共通helperから出す
- 第6版もformat/check/release build、ELF/ESP image検査に合格。IRAM 9,092 byte、DRAM rodata
  1,364 byte、IROM 215,112 byte、stack 179,200 byte、app image 375,792 byte。実機で成果物を
  識別できるよう`ut`直後に`USB TEST: recovery v6`をUARTへ直接出す
- 第6版`ut`をHigh-Speed／Bulk IN MPS 512 byteで実行し、100/100、failure 0、mismatch 0、
  QTD retry 2、command retry 0でPASS。長時間haltしないQTDを同一BOT phase内で局所再投入すれば
  回復でき、従来91/100で再現したUSB failureを解消した。次は短い`mix 1`で複合負荷を再確認する
- 第6版`mix 1`は3,441 frame、SD/USB I/O 57回、PSRAM heap検査3,138回を完走。USBはQTD
  retry 4回を使い切った後、BOT RecoveryとREAD(10) command retry 1回で回復した。storage
  mismatch 0、DPI underrun 0、display DMA error 0でPASSし、残る受入条件は`mix`既定120分だけ
- 第6版の120分`mix`は約44秒、2,545 frame、I/O 37回、heap 2,110回でUSBだけがFAIL。QTD retry
  4回後のBOT Resetは両halt解除のEP0 IN statusで失敗したが、DPI underrunとdisplay DMA errorは0
- 第7版はBOT Recovery失敗時にroot portを最大3回再列挙し、新しいMSC sessionから同じread-only
  4 KiBを再取得する。開始時referenceと完全一致した場合だけ継続し、再列挙数も結果へ表示する。
  実機識別markerは`MIX TEST: recovery v7`
- 第7版もformat/check/release build、ELF/ESP image検査に合格。IRAM 9,092 byte、DRAM rodata
  1,364 byte、IROM 216,444 byte、stack 179,200 byte、app image 377,120 byteでXIP 2本＋RAM 2本を維持
- 第7版`mix 2`はsoakを開始しなかった。起動時のHigh-Speed Hub配下の列挙でMSCが未登録だったため、
  `mix: no USB Mass Storage`で先に終了し、その後のbackground rescanでもHub port 2/3のdevice
  descriptor取得が失敗した。第7版の再列挙はsoak中のBOT failureだけを対象にしており、setup時の
  未登録を回復できない抜けだった
- 第8版は`mix` setupでもMSCのready確認に失敗した場合にroot portを最大3回同期的に再列挙する。
  さらに基準4 KiBの最初のREAD(10)が失敗した場合も再列挙して再試行し、両方が成功するまでsoakを
  開始しない。実機識別markerは`MIX TEST: recovery v8`
- 第8版もformat/check/release build、ELF/ESP image検査に合格。IRAM 9,092 byte、DRAM rodata
  1,364 byte、IROM 218,960 byte、stack 179,200 byte、app image 379,632 byteでXIP 2本＋RAM 2本を維持
- 第8版実機ではHub port 2/3のdescriptor列挙失敗後、slotが空のため60 frame周期の増分scanが同じ
  portをreset・列挙し続け、エラーログでconsole操作不能になった。第9版は失敗または未対応の物理接続を
  保留maskへ記録し、背景処理ではconnection/change bitだけをquietに監視する。抜き差しまたは明示的な
  full rescanまで同じdeviceの列挙を再試行しない。markerは起動時`USB ENUM: bounded retry v9`、
  `mix`開始時`MIX TEST: recovery v9`
- 第9版もformat/check/release build、ELF/ESP image検査に合格。IRAM 9,092 byte、DRAM rodata
  1,364 byte、IROM 219,052 byte、stack 179,200 byte、app image 379,728 byteでXIP 2本＋RAM 2本を維持
- 第9版`mix 2`は有限回でsetup failureを返し、連続background logは解消した。3回のfull rescanすべてで
  port 1のLS keyboardはSplit列挙できた一方、port 2のHS deviceはinitial descriptor IN data、port 3の
  FS deviceはSplit transactionで失敗し、MSCを取得できなかった。root resetを跨いで同じ結果のため、
  device／Hub TTにVBUS維持中の状態が残る可能性を次に切り分ける
- 第10版は3回のroot rescanを使い切った場合にUSB-A VBUSだけを1秒offにし、live registryを破棄して
  Hubと全downstream deviceを電源投入状態から一度だけ再列挙する。soak中の回復でも基準4 KiBと一致した
  場合だけ継続し、結果へ`power_cycles`を表示する。同じ試験の自動power-cycleは最大1回とし、2回目が
  必要な不安定状態はFAILにする。`mix` markerは`MIX TEST: recovery v10`
- 第10版もformat/check/release build、ELF/ESP image検査に合格。IRAM 9,092 byte、DRAM rodata
  1,364 byte、IROM 221,020 byte、stack 179,200 byte、app image 381,696 byteでXIP 2本＋RAM 2本を維持
- 第10版`mix 2`はPASS。setupのroot rescan 3回ではport 2/3を取得できなかったが、VBUS power-cycle
  1回後はLS keyboard、HS MSC（Bulk IN MPS 512）、FS mouseの3台を正常列挙した。soak中にBulk IN
  QTD packet retryを使い切りBOT Resetも失敗したが、次のroot rescanでMSCを再列挙し、基準4 KiBと
  完全一致して継続した。最終値は6,881 frame、I/O 118回、heap 6,647回、packet retry 22、
  command retry 0、rescan 5、power-cycle 1、DPI underrun 0、display DMA error 0。短時間複合条件を
  合格とし、残る受入条件は第10版の`mix`既定120分だけ
- 第10版`mix`既定120分は412,801 frame、I/O 7,228回、heap検査411,508回を完走。USBはpacket
  retry 132、command retry 0、rescan 4、VBUS power-cycle 1でread-only dataを維持し、DPI underrun
  0、display DMA error 0でPASSした。表示、PSRAM heap、SD、USB同時負荷の最終受入条件を合格
- Stage 4では診断raw APIをcrate内の`diagnostic_*`名に固定し、productionの128-byte burstと
  phase 0を維持した。`fill`／`fill_rect`のPPA経路は転送前後にcache同期し、CPU fallbackと後続CPU
  描画を同じ呼出し規約で扱うため、呼出し側の最終`flush`は残した。cleanな全画面同期は134 us、
  underrun 0/100であり、経路別の戻り値を追加するより安全なuniform contractを優先した
- 200 MHz／DPI 80 MHzだけで全完了条件を満たしたためDPI 70 MHzとStage 5〜8のtile backendは
  不要と判断した。PSRAM profile／fallbackは起動ログ、描画・帯域・USB回復は現状文書へ同期済み

### Stage 0実機結果

実測表は[`DISPLAY_BANDWIDTH.md`](DISPLAY_BANDWIDTH.md)に記録した。今後のStageで同じ
matrixを比較するときは`db`だけを実行する。個別caseの再測定が必要な場合だけ
`displaybench <mode> [count] [phase_ms] [burst]`を使う。

### Stage 1実機結果

80 MHz profileの起動ログは期待値どおり`MHz=0x50`、read/write latency `0xA/0x5`、
DQS `0/0/0`だった。`db 100`の代表値はidle 17.458 ms・0/100、sync 134 µs・0/100、
CPU 91.817 ms・100/100、PPA raw 16.013 ms・100/100、production 16.485 ms・100/100。
再起動後の`db 20`も各値と発生率が一致した。

## 結論

水色フラッシュの直接原因は、DSI BridgeのFIFOアンダーランである。ここでいう
「DMA停止」はチャネルのdisableやDMA errorではなく、DW-GDMAのPSRAM読み出しが
走査期限までに完了せず、Bridge FIFOが空になることを指す。アンダーラン後、Bridgeが
残りの画面を専用の水色出力へ切り替えるため、現象とレジスタ状態が一致する。

一方、現状文書の「PSRAMは飽和していない」という結論は、CPUの20〜40 MB/sという
レイテンシ律速の測定だけを足したもので、PPAを同時に動かす現在の経路には適用できない。
現行条件を整理すると次のとおり。

| 項目 | 現行値 |
| --- | ---: |
| フレームバッファ | 720×1280×2 = 1,843,200 byte |
| フレーム平均の走査読み出し | 約105.5 MB/s |
| 有効ライン中の平均 | 約125.6 MB/s |
| 有効画素中の瞬間要求 | 80 MHz×2 byte = **160 MB/s** |
| PPA全画面fillの実効値 | 1,843,200 byte÷13.267 ms = **約139 MB/s** |
| 80 MHz x16 DDR PSRAMの理論値 | **320 MB/s** |

有効画素中にPPA fillが重なると、単純合計でも約299 MB/sとなり、理論値の約93%を
占める。実際には固定レイテンシ、refresh、read/writeの切り替え、AXI調停があるため、
FIFOが吸収できる数十µsを超える待ちが発生しても不思議ではない。したがって正確な
表現は、**平均帯域が常時不足しているのではなく、80 MHz構成では大面積更新中の
瞬間帯域と最悪応答時間に余裕がない**、となる。

他のアプリで目立たないこととも矛盾しない。M5Stackの公開設定にはHex PSRAMを
200 MHzで使う例があり、EspressifのTab5 BSPはDPI 70 MHz、小さなLVGL draw buffer、
DMA2Dによるdirty rectangle転送を標準にしている。同じパネルでも、80 MHz PSRAMと
DPI 80 MHzを使い、PSRAM上の走査面を直接全画面更新する本実装とは負荷が異なる。

## 目的

1. 測定経路を実処理から分離し、PPA転送、CPU描画、cache同期、開始フレーム位相の
   どれがアンダーランを起こすか再現可能な数字で判断できるようにする。
2. PSRAMを200 MHzで安全に動かし、表示走査に対する帯域と応答時間の余裕を増やす。
3. 200 MHz化だけで足りない場合は、CPUが走査中のPSRAMフレームバッファを直接
   描画・cache同期しない構成へ移行する。
4. 200 MHzの学習失敗時も80 MHzへ戻り、表示以外を含めて起動可能な状態を維持する。

## 完了条件

最終判定は「見た目では発生しなかった」ではなくBridgeのsticky underrun bitを
各操作の間で回収して行う。少なくとも次をすべて満たすこと。

- 起動後30分のアイドル走査でunderrun 0件
- 全画面更新100回でunderrun 0/100
- `clear`、コンソール末尾スクロール、paint、touch、axis、winの画面遷移を各100回
  相当実行してunderrun 0件
- 表示、USB、SD、PSRAM heapを同時に使う2時間の複合試験でunderrun 0件、DMA error
  0件、trap 0件
- コールドブート10回、`reboot` 20回に成功
- `alloctest`で確保可能なPSRAM全域を検査し、読み書き不一致0件
- RGB565の色、回転、クリッピング、スクロール結果が現状と一致
- releaseリンク時に内部RAMの`.stack >= 128 KiB`を維持

100回試験で1件でも発生した場合は合格にしない。Bridgeの表示は1 bitなので、長い
ループの最後に一度だけ読むのではなく、現在の`stress`と同じく各操作後に消費する。

## 範囲外

- ESP-IDFまたはRTOSの導入
- 解像度やRGB565フォーマットの変更
- 表示のティアリングをなくすためだけのダブルフレームバッファ化
- パネル交換や基板変更
- 37〜39 Hzで目視点滅したVFP拡大方式の再採用

ダブルバッファは表示切り替えを原子的にできるが、描画時のPSRAMトラフィックを
減らさない。全画面コピーを追加すれば帯域には逆効果なので、本問題の対策としては
採用しない。

VFPを増やさずDPIピクセルクロックを70 MHzへ下げ、水平タイミングを組み直す方法は
まだ試していないため範囲内とする。VFP拡大の失敗だけを根拠に除外しない。

## 現状実装の整理

### 直接原因まで確認できていること

- `src/lcd.rs`はBridge `INT_RAW` bit 0を毎フレーム回収している。このbitはFIFOが
  空になったことを示し、水色フラッシュと対応する
- 表示DW-GDMAにはerrorがなく、ISRは同じフレームバッファを次フレームへ再設定できる
- Bridgeの2048 byte burst、768 word refill threshold、表示DMAのoutstanding数は
  ESP-IDF v5.5.3と同じ
- ICMで表示DMAをpriority/ARQOS 15へ上げると発生率は下がるが、0件にはならない
- CPU全画面fillは約94 ms、PPA全画面fillは約12〜13 msだが、PPAでも`stress 20`は
  8/20でアンダーランする

### 診断上の問題

1. `ppafill sweep`のCPU側は公開`Framebuffer::fill_rect`を呼ぶ。このAPIは768画素以上を
   自動的にPPAへ送るため、大きな矩形の「CPU」列はCPU測定になっていない。
2. `Framebuffer::ppa_fill_rect`は転送前後に`flush_rect`を行うが、`stress`は
   `fill()`の後でもう一度全画面`flush()`する。現在の12〜13 msにはPPA転送、前後の
   cache同期、重複した同期が混在している。
3. production APIの「経路選択」と、診断APIの「特定経路を強制する」が分離されて
   いない。このままでは最適化後に同じ条件を比較できない。

最初にこの3点を直さない限り、200 MHz化やburst変更の効果を正しく判定できない。

## 設計方針

1. **一度に一変数だけ変える。** 測定、PSRAMクロック、描画構造を同じStageで変更しない。
2. **80 MHzを必ず残す。** 200 MHzのDQS学習またはメモリ検査が失敗したら、同じbootで
   コントローラーとデバイスを再初期化し80 MHzへ戻す。
3. **走査を最優先にする。** 表示DW-GDMAのICM priority/ARQOS 15は維持し、PPA/DMA2Dは
   それより低いままにする。
4. **productionと診断のAPIを分ける。** productionは最適経路を選び、診断はCPU、PPA、
   cache同期、フレーム位相を明示的に指定する。
5. **大規模な描画API変更は判定後に行う。** 200 MHzで完了条件を満たすなら、SRAMタイル
   移行は実装せず将来案として残す。
6. **各Stageを独立コミット・独立実機確認する。** 起動不能や表示崩れが出たときに直前の
   変更だけを調べられる単位にする。

## 目標構成

200 MHzだけで完了条件を満たさない場合の最終構成は次のとおり。

```text
アプリの描画要求
  -> 内部SRAMの単一RGB565タイルへclip付き描画
  -> dirty rectangleをDMA2DでPSRAMへ転送
  -> PSRAM上のscanout buffer（CPUは画素へ触れない）
  -> 表示DW-GDMA（ICM最優先）
  -> DSI Bridge FIFO
```

`Framebuffer`が現在兼ねている責務を次へ分ける。

| 責務 | 移行後の所有者 |
| --- | --- |
| PSRAM走査面のアドレスと寿命 | `ScanoutBuffer` |
| RGB565 primitiveとclip | `RasterTarget`／`Painter` |
| PPA/DMA2D起動と完了待ち | `DisplayTransfer` |
| dirty範囲、フレーム位相、転送量制限 | `DisplayUpdater` |
| 画面内容の決定 | consoleと各app |

走査面の画素をCPUから参照しない契約にできれば、DMA転送の前後に現在行っている
destination全域のcache writeback/invalidateは不要になる。debug readbackだけは専用APIで
明示的にinvalidateしてから行う。

内部RAMは現在約186 KiBのstack余裕があり、リンク下限は128 KiBである。最初の候補は
720×32×2 = 46,080 byteの**単一native-lineタイル**とする。CW回転後の論理座標では
幅32の縦帯に相当する。追加後も計算上約140 KiBを残せるが、実際のELFで下限を確認する。
二重タイルは約92 KiBを使って128 KiB下限を割るため採用しない。32行が入らない場合は
24行または16行へ下げ、stack下限を優先する。

## Stage 0: 診断経路を正す

productionの表示結果を変えず、測定だけを信頼できる状態にする。

- `src/framebuffer.rs`で次の操作を内部的に分離する
  - CPUだけで矩形を書き、cache同期しないraw fill
  - PPAだけで矩形を書き、必要な同期回数を呼び出し側が把握できるraw fill
  - cache writeback/invalidateだけ
  - 面積しきい値で経路を選ぶproduction fill
- `ppafill sweep`のCPU列はraw CPU経路を直接呼び、productionの自動PPA選択を通さない
- `stress`のPPA経路で重複している全画面`flush()`を除いたケースを別に測る
- 診断コマンドを、少なくとも次のケースを固定回数で比較できる形にする
  - 走査のみ
  - cache同期のみ
  - CPU raw fill + 1回の同期
  - PPA raw fill + 必要最小限の同期
  - 現行production fill
- 各操作後にunderrun bitを回収し、経過µs、underrunした操作数、経過frame数を表示する
- `wait_for_frame`直後を0として0/3/8/12 ms遅延させ、開始位相ごとの差を測れるようにする
- DMA2D burstを8/16/32/64/128 byteから診断時だけ選べるようにする

**完了条件**:

- 全画面CPU raw fillが過去の約94 msと同程度になり、PPAへ流れていない
- PPA転送とcache同期の時間が別々に記録できる
- 同じ設定を100回測って、時間とunderrun数が再現する
- productionの画面、色、スクロールに変化がない

Stage 0の結果は`DISPLAY_BANDWIDTH.md`へ反映し、現在の「PSRAMは飽和していない」と
大矩形のCPU測定表を訂正する。

## Stage 1: PSRAMタイミング設定をプロファイル化する

動作周波数はまだ80 MHzのままにし、200 MHzを安全に追加できる構造へ整理する。

- `src/psram.rs`のクロック、mode register、dummy、DQS学習設定を`PsramTiming`へ集約する
- `80 MHz`と`200 MHz`をenumまたは固定profileで表し、周波数から独立したmagic numberを
  残さない
- 80 MHz profileは現在のSPLL 480 MHz÷6、fixed read latency 10 cycle、write latency
  5 cycleと完全に同じ値にする
- profile名、実クロック、read/write latency、選択されたDQS phase/data/dqs delayをUARTへ
  1回だけ表示する
- MSPI設定とDQS学習の全コードを`.iram.text.critical.psram`、参照定数をDRAM閉包内に維持し、
  既存のELF relocation検査へ対象を追加する

**完了条件**: 80 MHz profileで生成したレジスタ値、起動ログ、`membench`、`stress 100`、
コールドブートと`reboot`が変更前と同等である。

## Stage 2: 200 MHz PSRAMを実機bring-upする

M5Stackの公開構成と同じ200 MHzを第一候補にする。単なるdivider変更にはしない。

- ESP-IDF v5.5.3のESP32-P4 PSRAM初期化と対象PSRAMのmode register定義を照合し、MPLLの
  電源投入、周波数設定、安定待ち、clock source切り替え、MSPI dividerを実装する
- 200 MHz用のfixed read latency（候補14 cycle）、write latency、command dummyをprofileへ
  設定する。値は実装時に公式ドライバとmode register readbackで確定する
- 既知の80 MHz調整値を流用せず、200 MHzでDQS phase/data delay/DQS delayを全探索する
- 候補点は短い1パターンだけでなく、複数アドレス、walking bit、反転パターンを繰り返して
  合否判定する
- 学習後に先頭、フレームバッファ境界、ヒープ中間、32 MiB末尾をcache/direct両経路で
  検査する
- 200 MHzの学習または検査が失敗したらMSPIをresetし、mode registerを含めて80 MHz profile
  から再初期化する。中途半端な200 MHz設定のまま続行しない
- fallbackした事実と失敗段階を、FLASH停止中にも安全な短いエラーコードで残す

**完了条件**:

- 200 MHzでコールドブート10回、`reboot` 20回に成功
- 各bootでDQS学習点が有効範囲の中央付近にあり、1点だけの偶然合格ではない
- `alloctest`、`membench`、全画面read/write/verifyを完走
- 意図的に不正な学習候補を与えた診断buildで80 MHz fallbackが成立
- FLASHのcold DROM/IROM probeがPSRAM再設定後も成功

## Stage 3: 200 MHzで表示の合否を判定する

Stage 0と同じバイナリ経路・回数を使い、周波数だけを変えて比較する。

| 測定 | 80 MHz | 200 MHz |
| --- | ---: | ---: |
| idle 30分のunderrun | 記録 | 記録 |
| CPU raw fill 100回 | 時間・件数 | 時間・件数 |
| PPA raw fill 100回 | 時間・件数 | 時間・件数 |
| cache同期のみ100回 | 時間・件数 | 時間・件数 |
| 位相0/3/8/12 ms | 各件数 | 各件数 |
| burst 8〜128 byte | 各時間・件数 | 各時間・件数 |

理論帯域はx16 DDRとして320 MB/sから800 MB/sへ2.5倍になる。PPAとの単純合計
約299 MB/sは理論値の約37%まで下がるため、最も効果が大きいと予想する。ただし
合否は理論計算ではなく「完了条件」の実測で決める。

200 MHzだけでunderrunが残る場合は、大規模リファクタへ進む前にDPI 70 MHzを独立に
試す。現在の80 MHz用水平レジスタをそのまま流用せず、DSI Hostのlane byte clock単位と
Bridgeのpixel単位の両方を再計算する。可能なら垂直refreshを現在の約57.3 Hz付近に保つ
水平porch候補を先に試し、パネルの点滅、同期外れ、色化けがあれば不採用にする。
70 MHzでは有効画素中の走査要求が160 MB/sから140 MB/sへ下がる。PSRAM、DMA2D burst、
描画処理を同時に変えず、DPI timingだけの差として100回試験する。

**判断A**:

- 完了条件をすべて満たす: 200 MHzをproduction既定にし、Stage 4で軽量なAPI整理と
  文書化だけ行う。Stage 5以降は未着手の代替案として残す
- 200 MHz／DPI 80 MHzで1件でもunderrunする: 200 MHz／DPI 70 MHzを試し、表示品質と
  完了条件の両方を満たせばStage 4へ進む。満たさなければDPI 80 MHzへ戻してStage 5へ進む
- 200 MHzが安定しない: 80 MHz fallbackでDPI 70 MHzを独立評価し、それでも完了条件を
  満たさなければStage 5以降を必須とする

## Stage 4: 低リスク構成を確定する

**完了。** 200 MHz／DPI 80 MHzで文書冒頭の全条件を満たした。productionは128-byte burst、
開始位相0を維持し、DPI timingや大規模描画構造は変更していない。`fill`／`fill_rect`後の最終
`flush`はCPU fallbackと同一規約を保つため意図的に残した。Stage 5以降は未着手の代替案として残す。

判断Aで200 MHzだけで合格した場合の完了Stage。

- Stage 0のraw診断APIをproduction APIから見分けられる名前と可視性に固定する
- `Framebuffer::fill`、`fill_rect`、`ppa_fill_rect`のcache同期責務を文書化し、呼び出し側の
  重複`flush`をなくす
- Stage 0で有意差が確認できた場合だけDMA2D burstと開始位相をproductionへ反映する
- Stage 3でDPI 70 MHzを採用した場合は水平・垂直の実測値とパネル確認結果を残す
- PSRAM起動ログへ実際に選択されたprofileとfallback回数を残す
- `PSRAM.md`、`DISPLAY.md`、`DISPLAY_BANDWIDTH.md`、`GRAPHICS.md`を実装へ同期する

**完了条件**: 文書冒頭の全完了条件を満たす。ここで満たせれば大規模な描画構造の
変更を行わず、本計画を完了にする。

## Stage 5: 走査面と描画primitiveを分離する（判断Aで不合格の場合）

このStageは責務分割だけを行い、最初は既存PSRAMへ同じ描画を行って差分をなくす。

- `Framebuffer`からPSRAM所有と走査アドレスを`ScanoutBuffer`へ分離する
- pixel、line、rectangle、circle、glyphを、base/width/height/stride/rotation/clipを受け取る
  `RasterTarget`上の処理へ移す
- PPA/DMA2D操作を`DisplayTransfer`へ集約し、転送の前後処理と完了待ちを一箇所にする
- appからPSRAM pointerやcache同期APIを直接呼べない可視性にする
- 既存のCW座標変換とRGB565出力をgolden patternで比較する

**完了条件**: まだPSRAM直接描画の互換backendを使った状態で、全アプリの画面と
performanceが変更前と一致する。機能差があるままタイル化へ進まない。

## Stage 6: 単一SRAMタイルと転送budgetを導入する

- 内部SRAMにnative 720×32 RGB565の単一タイルを静的確保する
- `RasterTarget`へclipを設定し、論理Xの32画素帯ごとに既存sceneを描けるようにする
- dirty rectangleと交差する部分だけをDMA2Dでscanout bufferへコピーする
- タイルは1枚だけとし、DMA完了を待ってから再利用する
- scanout buffer初期化後はCPU画素アクセスを禁止し、destination側cache同期を撤去する
- `wait_for_frame`直後のblanking開始から転送し、Stage 0の実測から安全なbyte/frame上限を
  決める。全画面が1 frameに収まらなければ複数frameへ分割する
- DMA2D burstは最大速度ではなくunderrun 0件となる最長値を選ぶ
- 46,080 byte追加後も`.stack >= 128 KiB`をリンク時ASSERTで確認する。満たさない場合は
  24行または16行へ下げる

**完了条件**:

- scanout bufferへのCPU storeと全画面cache writebackが通常経路から消える
- dirtyな1セル、スクロール1行、全画面の3種類で画面が正しい
- 80 MHz fallbackを含め全画面更新100回でunderrun 0/100
- 更新が複数frameになる場合、その最大遅延を測定して記録する

## Stage 7: consoleと各appを段階移行する

一度に全画面を移さず、更新形態の小さい順に切り替える。

1. 座標チャートなど固定の診断画面
2. consoleの1セル更新とカーソル点滅
3. consoleの末尾スクロール、`clear`
4. paintとtouch
5. axis、battery、win

各画面は「状態更新」と「指定clipの描画」を分ける。タイルが論理X帯なので、全画面
sceneは必要な帯ごとに同じ描画関数をclip付きで再実行する。描画関数内でI/Oや状態更新を
行うと帯の数だけ副作用が起きるため、センサー読み出し、入力処理、window移動などは
renderの外に残す。

移行途中はPSRAM直接描画backendを残すが、画面単位でどちらか一方だけを使う。1回の
更新で両backendを混ぜてcache所有権を曖昧にしない。

**各画面の完了条件**:

- 変更前後のスクリーンショットまたは座標・色のreadbackが一致
- 100回の画面遷移または同等の連続操作でunderrun 0件
- CPU store、DMA2D transfer、転送frame数を診断ログで説明できる

## Stage 8: 互換経路の撤去と最終試験

- 全画面がタイルbackendへ移行した場合、productionのPSRAM直接pixel storeと
  `flush_rect`依存を撤去する。診断用readbackだけを明示的に残す
- `Framebuffer`という曖昧な名前を最終責務に合わせて整理し、呼び出し側から走査面の
  cache所有権を隠す
- Stage 0の診断コマンドは回帰試験用として残し、productionで選べない危険な設定には
  明示的な`diagnostic`表記を付ける
- 文書冒頭の全完了条件を80 MHz fallbackと200 MHzの両方で実施する
- `DISPLAY.md`、`DISPLAY_BANDWIDTH.md`、`GRAPHICS.md`、`PSRAM.md`、`FILE_LAYOUT.md`を
  最終実装へ同期し、本計画へ実測値と判断を記録する

## 実装順と主な変更対象

| Stage | 主なファイル | 変更の性質 |
| --- | --- | --- |
| 0 | `src/framebuffer.rs`, `src/app/shell.rs`, `src/dma2d.rs` | 診断分離 |
| 1〜3 | `src/psram.rs`, `src/startup.rs`, ELF検査tool | 200 MHz/fallback |
| 4 | 上記と現状文書 | 小規模な確定作業 |
| 5 | `src/framebuffer.rs`, 新規`src/raster.rs`, 新規`src/display_transfer.rs` | 責務分割 |
| 6 | 新規`src/display_update.rs`, `src/lcd.rs`, `memory.x` | SRAMタイルとframe budget |
| 7 | `src/console.rs`, `src/app/`以下 | 画面単位の移行 |
| 8 | 上記全体、`docs/` | 互換経路撤去と記録 |

新規ファイル名は責務を示す候補であり、Stage 5着手時に既存module構成と照合して確定する。

## 判断記録に残す値

各Stageの実機確認後、本節へ最低限次を追記する。

- firmware commit、build profile、CPU周波数、PSRAM profile
- DQSの合格範囲と選択点
- display/DMA2DのICM priorityとARQOS、DMA2D burst
- 操作種別、矩形、回数、開始位相、elapsed、underrun数
- 1 frame当たり転送上限と全画面更新に要したframe数
- `.data`、`.bss`、SRAM tile、残り`.stack`のbyte数
- cold boot、reboot、複合試験の成功回数と継続時間

## 参照実装

- ESP-IDF v5.5.3 `components/esp_lcd/dsi/esp_lcd_panel_dpi.c`。Bridgeのblue underrun、
  256-word burst、768-word threshold、GDMA設定の照合先
- ESP-IDF v5.5.3
  `components/esp_psram/device/esp_psram_impl_ap_hex.c`、
  `components/esp_hw_support/port/esp32p4/rtc_clk.c`、
  `components/hal/esp32p4/include/hal/clk_tree_ll.h`。MPLL、200 MHz timing、DQS学習の照合先
- Espressif `esp-bsp`のM5Stack Tab5 display設定。DPI 70 MHz、小さいdraw buffer、DMA2D、
  dirty rectangle転送を使う比較対象
- M5StackのTab5向け公開設定。Hex PSRAM 200 MHzを使う比較対象

これらはコードの移植元ではなく、レジスタ値と設計意図の照合先とする。本プロジェクトは
引き続きESP-IDF/RTOSをリンクせず、ECO2実機で各段階を確認する。
