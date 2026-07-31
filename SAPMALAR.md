# Bilinçli sapmalar

Bu depo `UPSTREAM.md`'de kayıtlı Zed revizyonuyla parite tutar. Aşağıdaki
maddeler o pariteden bilinçli olarak ayrılan değişikliklerdir; koşulları ve
süreci `AGENTS.md` içindeki **Deliberate divergence** bölümü tanımlar.

Kayıt tutmayan sapma yoktur: bir değişiklik burada yazılı değilse, upstream
senkronunda çatışma çıktığında Zed revizyonu kazanır ve değişiklik düşer.

Her madde şunları taşır:

- **Sınır** — upstream'in bugün engellediği veya desteklemediği şey.
- **Elenen tüketici yolu** — tüketici tarafında neden çözülemedi ya da maliyeti
  neden orantısızdı.
- **Kazanç** — ölçülebilir sonuç.
- **Dosyalar** — dokunulan kaynaklar.
- **Bırakma koşulu** — upstream neyi kapsarsa bu madde silinir.

Senkron sırasında her madde yeniden uygulanır ve sınırın hâlâ geçerli olup
olmadığı sınanır; upstream kapsadıysa madde silinir.

## Kayıtlı sapmalar

GPUI'nin tutulmuş kaynak kodu ve public API'si `UPSTREAM.md`'deki revizyonla
paritededir. Aşağıdaki maddeler yalnızca bağımsız deponun manifest metadata'sını ve
bağımlılık çözümünü değiştirir; senkronda korunmaları gerektiği için burada kayıtlıdır.

### gpui paket sürümü

- **Sınır:** Upstream `crates/gpui/Cargo.toml` içinde `version = "0.2.2"` bildirir ve
  bu, Zed'in crates.io'da yayınladığı en son sürümün ta kendisidir (22 Ekim 2025).
  Yerel geliştirme derlemesi dağıtımdaki crate ile birebir aynı sürüm dizesini
  raporladığından ikisi geliştirme sırasında sürekli karıştırıldı. Upstream'in
  numarayı değiştirmesini beklemek dışında bu ayrımı yapmanın bir yolu yok.
- **Elenen tüketici yolu:** Ayrım tüketici tarafında ancak `cargo tree` veya lock
  dosyası incelenerek yapılabiliyor; sürüm dizesinin kendisi her iki tarafta da aynı
  kaldığı için hata kaynağı yerinde duruyor. Ayrımın kaynağın kendisinde olması
  gerekiyordu.
- **Kazanç:** `cargo pkgid`, `cargo tree`, derleme çıktısı ve lock dosyası artık iki
  crate'i tek bakışta ayırıyor: yayınlanan `0.2.2`, bu depo `0.3.0`. Yanlış crate
  üzerinde çalışma sınıfı hata ortadan kalkıyor.
- **Dosyalar:** `crates/gpui/Cargo.toml`, `Cargo.lock`
- **Kurallar:** Sürüm yayınlanmış `0.2.x` hattının üzerinde tutulur ve asla Zed'in
  crates.io'ya çıkardığı bir numaraya düşürülmez. Prerelease eki (`-alpha`, `-beta`,
  `-rc`) kullanılmaz — bu bir önizleme değil, sıradan bir kaynak anlık görüntüsüdür.
  Zed'in kendi sürümü `0.3.0`'a ulaşır veya geçerse yerel minor yeniden yükseltilir ve
  bu madde, `AGENTS.md` ile `UPSTREAM.md` birlikte güncellenir.
- **Bırakma koşulu:** Zed, GPUI'yi yayınlanan sürümden ayrılan bir numarayla
  bildirmeye başlarsa veya bu depo crate'i kendi adıyla yayınlamaya geçerse madde
  düşer.
- **Upstream durumu:** Gönderilmedi. Sapma bu deponun kimliğine dair; Zed'i
  ilgilendiren bir eksiklik değil.

### stacksafe 1.x bağımlılığı

- **Sınır:** Upstream workspace `stacksafe = "0.1"` seçtiği için çözümleme
  `stacksafe 0.1.4 -> stacksafe-macro 0.1.4 -> proc-macro-error2 2.0.1` zincirini
  kullanıyor. Rust 1.97 bu son crate için gelecekte kesin hataya dönüşecek
  `E0365` uyumsuzluğunu her derlemede bildiriyor; dolayısıyla Barcoders'ın yerel
  GPUI özelliğini içeren temiz kararlı araç zinciri derlemesi uyarısız tamamlanmıyor.
- **Elenen tüketici yolu:** Barcoders'a doğrudan `stacksafe 1.x` eklemek GPUI'nin
  `0.1` sürüm gereksinimini değiştirmiyor ve iki sürümü yan yana çözümlüyor.
  Tüketici tarafındaki `[patch]` çözümü ise aynı `0.1.4` kimliği altında bakımı
  üstlenilen bir fork/vendor kopyası gerektiriyor; tek manifest satırını kaynağında
  yükseltmeye göre orantısız ve uyarıyı diğer GPUI tüketicilerinde bırakıyor.
- **Kazanç:** `stacksafe 1.0.3`, GPUI'nin kullandığı `StackSafe` ve `#[stacksafe]`
  yüzeyini korurken `proc-macro-error2` bağımlılığını kaldırıyor. Kararlı Rust ile
  GPUI ve Barcoders derlemeleri sıfır gelecek-uyumluluk uyarısıyla tamamlanıyor.
- **Dosyalar:** `Cargo.toml`, `Cargo.lock`, `EXTRACTION.md`, `SAPMALAR.md`
- **Bırakma koşulu:** Kayıtlı Zed revizyonu `stacksafe 1.x` veya daha yeni uyumlu
  bir seri kullandığında yerel sürüm satırı upstream ile eşitlenir ve bu madde silinir.
- **Upstream durumu:** Gönderilmedi. Değişiklik Zed workspace'inde ayrıca ele
  alınmalı; bu depo yalnızca yerel tüketiciyi bugün etkileyen uyarıyı kaldırıyor.

<!--
Şablon:

### <kısa ad>

- **Sınır:** …
- **Elenen tüketici yolu:** …
- **Kazanç:** …
- **Dosyalar:** …
- **Bırakma koşulu:** …
- **Upstream durumu:** (gönderildi / gönderilmedi, gerekçe)
-->
