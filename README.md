# ai-yagni-data-sdk-example-payment

EC の例（[ai-yagni-data-sdk-example-backend-rust](https://github.com/pad01g/ai-yagni-data-sdk-example-backend-rust)）で使う、模擬の決済サービスです。HTTP + JSON で話します。

このサービスを呼ぶのは [backend-proxy](https://github.com/pad01g/ai-yagni-data-sdk-example-backend-proxy) だけです。
backend の compose では、このサービスを proxy と同じ内部ネットワークにだけ置きます。そのため backend-rust からは届きません。

## API

| メソッド | パス | 中身 |
|---|---|---|
| POST | `/charges` | `{"idempotency_key", "amount", "currency", "card_token"}` を請求する。承認なら 201、拒否なら 402 |
| GET | `/charges/{id}` | 決済を返す |
| GET | `/charges?idempotency_key=...` | その冪等キーの決済を返す（なければ 404）。決着しない注文を、呼ぶ側が後から突き合わせるため |
| GET | `/health` | `ok` |

- 同じ冪等キーには同じ結果を返します。中身の違う依頼に同じキーを使うと 409 です。
- 依頼の形が正しくないとき（額が 0、通貨が 3 文字の大文字でない、トークンが長すぎるなど）は 400 です。拒否（402）とは別で、記録もしません。
- 決済 ID は乱数で作ります。再起動しても前の ID と重なりません。
- 本物の決済サービスと同じく、冪等キーと決済は再起動をまたいで覚えています。
  - 決済は 1 件ずつ、追記だけのファイル（環境変数 `JOURNAL`、イメージでは `/data/charges.jsonl`）に書きます。
  - 起動したときに、そのファイルを読み直します。
  - 書けなかった決済は行いません（503）。書きかけの分はファイルから消し、消せなければそれ以上の決済を断ります。
  - 書いている途中で落ちて最後の行が切れていたら、起動したときにその行を捨てます（その決済は呼ぶ側に返していません）。
- 承認する決済の数に上限（100 万）があります。拒否は数えません。
  - `JOURNAL` がなければメモリにだけ置きます（テストと手元の試し用）。

カードのトークンごとの結果は次のとおりです。

| トークン | 結果 |
|---|---|
| `tok_visa` | いつも承認する |
| `tok_declined` | いつも拒否する |
| `tok_limited` | 累計の利用枠 10,000 まで承認する（全員で 1 枚のカードを共有する想定。再起動しても戻らない） |
| それ以外 | 拒否する |

## Verus

[`verified/`](verified/src/lib.rs) は、利用枠の判定 `authorize` を証明しています。

- 承認した額は 0 より大きい。
- 足し算はあふれない。
- 新しい使った額は枠を超えない。
- 拒否するのは、額が 0 のときか、枠を超えるときだけ。

## 動かし方（ホストには docker 以外何も入れない）

```sh
make test     # テスト
make lint     # rustfmt と clippy
make verify   # Verus（amd64 のイメージ）
make run      # http://localhost:8081 で起動する
```

## ライセンス

MIT
