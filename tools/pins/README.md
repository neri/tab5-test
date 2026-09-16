# SPKI pin table

> 索引: [`../../DESIGN.md`](../../DESIGN.md) ／
> [`../../docs/NETWORK.md`](../../docs/NETWORK.md) ／
> [`../../docs/plans/archive/TLS_PLAN.md`](../../docs/plans/archive/TLS_PLAN.md)

`generate.py`が`pins.txt`と`fixture_pins.txt`を読み、firmwareがリンクする
`src/net/pins/generated.rs`を書きます。firmwareはこのテキストを解析しません。
生成物はコミット済みで、同じ入力からは常に同じバイト列になります。

```sh
tools/pins/generate.py                      # 再生成
tools/pins/generate.py --check              # 生成物が入力と一致するか
tools/pins/generate.py --from-cert cert.pem # 追記する行を計算する
tools/pins/generate.py --check-release ELF  # fixture pinが入っていないこと
```

## pinとは何か

leaf証明書のDER `SubjectPublicKeyInfo`のSHA-256（32 byte）です。証明書全体
ではなく公開鍵の構造にかけるので、**同じ鍵のまま証明書を更新してもpinは
変わりません**。有効期限切れによる更新でfirmware更新が要らないのはこのため
です。algorithm identifierを含む構造にかけるのも意図的で、鍵のバイト列だけに
かけると別のalgorithmで同じ鍵材料を使われたときに一致してしまいます。

## 行の書式

```text
<hostname>  <64桁の16進>
```

`#`以降と空行は無視します。hostnameは小文字で、URLの中の文字列と完全に一致
させます（`Url::host()`が返すもの）。IPv4リテラルも書けます。

1つのhostにつき**最大2 pin**です。「今使っている鍵」と「次に移る鍵」で、
それ以上は増やせません。3つ目を許すと、攻撃者が持っているかもしれない鍵が
1つ増えるだけで、pinの意味が薄れていくためです。

## 鍵のrotation

1. 新しい鍵を作り、その証明書を`--from-cert`にかけてpinを得る
2. **新旧2つのpinを並べた行**を`pins.txt`へ入れ、firmwareを配布する
3. 全ての基板が更新を取り込むのを待つ
4. serverの鍵を新しい方へ切り替える
5. 次のfirmware更新で古い方のpinを消す

順番が逆になると、更新前の基板がserverへ繋がらなくなります。pinの不一致は
未認証TLSへfallbackせず`tls-pin`で失敗するので、これは「警告が出る」では
なく「繋がらない」です。

## serverからpinを取る

```sh
openssl s_client -connect example.com:443 -servername example.com </dev/null \
  | openssl x509 -noout -pubkey \
  | openssl pkey -pubin -outform DER \
  | openssl dgst -sha256 -r
```

手元に証明書ファイルがあるなら`tools/pins/generate.py --from-cert`でも同じ
値が出ます（実際にOpenSSLを呼びます）。

**接続して得た値をそのまま信用しないでください。** その接続自体が攻撃されて
いれば、攻撃者の鍵をpinすることになります。pinの入手経路は、pinしようとして
いる接続とは別でなければ意味がありません。

## fixture pinは通常releaseに入れない

`fixture_pins.txt`のpinは`tools/tls/`の鍵のもので、**秘密鍵がリポジトリに
入っています**。安全なのは、これらが`tls-fixture-pins` feature を付けた
ビルドにしか入らないからです。

```sh
cargo build --release --features tls-fixture-pins   # 試験用
cargo build --release                                # 通常
tools/pins/generate.py --check-release target/riscv32imafc-unknown-none-elf/release/tab5-hello-world
```

`--check-release`はELF全体からpinの32 byteを探し、見つかったら失敗します。
シンボルテーブルではなくバイト列を探すのは、将来どのセクションに置かれても
「入っていない」を確かめたいからです。

`fixture_pins.txt`の中のLANアドレスの行は、`browser_fixture_server.py`を
動かす機械のアドレスに合わせて書き換えてから再生成してください。
