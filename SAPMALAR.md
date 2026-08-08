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

GPUI'nin tutulmuş kaynak kodu ve public API'si varsayılan olarak `UPSTREAM.md`'deki
revizyonla paritededir. Aşağıdaki her madde bu varsayılandan izinli bir istisnadır:
ilk iki madde bağımsız deponun manifest metadata'sını/bağımlılık çözümünü, “Zengin
şekillendirilmiş satır geometrisi” ise açıkça kayıtlı runtime ve public API yüzeyini
değiştirir. Hepsi senkronda korunmaları gerektiği için burada kayıtlıdır.

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

### Zengin şekillendirilmiş satır geometrisi

- **Sınır:** Kayıtlı upstream GPUI tek `font_size` ile satır şekillendiriyor;
  `TextRun` koşum-bazlı yazı ölçüsü, asgari satır yüksekliği veya taban çizgisi
  kaydırması taşımıyor. Fallback ile gerçekten seçilen `FontId` değerleri her
  zaman fiziksel yüze geri çözülemiyor, karma BiDi sınırlarında aynı UTF-8
  indeksi için affinity ve yön taşıyan birden çok görsel caret durağı yok ve
  `ShapedLine` geometrisi boya/dekorasyon yükünden public API'de ayrılamıyor.
  Üstelik görsel sırada gezilen BiDi gliflerinin mantıksal byte indeksleri geri
  sıçrayabildiği hâlde eski boya yolu `DecorationRun`ları yalnız ileri tüketiyor;
  bu durumda glif, arka plan, alt çizgi veya üstü çizgi yanlış mantıksal koşumun
  boyasını alabiliyor.
- **Elenen tüketici yolu:** Tüketici satırı parçalara ayırıp ayrı ayrı
  şekillendirirse Arapça, İndik, ligatür ve BiDi bağlamını bozuyor. Glifleri
  sonradan ölçeklemek veya kaydırmak raster ölçüsünü ve fallback seçimini
  değiştirmiyor. `get_font_for_id` yalnız istek önbelleğini ters taradığı için
  platformun seçtiği fallback yüzünü kanıtlayamıyor. Yeni boya için ikinci bir
  `shape_line` çağrısı ise font seçimini yeniden çalıştırıp geometri kimliğini
  değiştirebiliyor. Bu sınırlara tüketici tarafından doğru ve orantılı bir
  geçici çözüm yoktur.
- **Kazanç:** Tek fiziksel satır çağrısında farklı yazı ölçüsü/asgari satır
  yüksekliği/taban kaydırması taşıyan koşumlar şekillendirilebilir; her shaped
  koşum aynı `TextSystem` kapsamındaki opak fiziksel yüz kimliğini taşır; karma
  BiDi sınırı byte indeksi + affinity + yön + x duraklarını verir; aynı değişmez
  geometri ve tek yerleşim dönüşümü yeni bir font seçimi yapmadan farklı boya
  yükleriyle tekrar çizilebilir. Boya koşumu her görsel glif için mantıksal
  `glyph.index` üzerinden yeniden çözülür; hedefli birim testi görsel sıra
  ilerlerken mantıksal sıranın geri döndüğü yolu sabitler. macOS CoreText ve
  WGPU cosmic-text kanıtları
  sırasıyla 12/18/24 px karma koşum, fallback yüz, çift BiDi durağı ve
  paint-only yeniden kullanım vakalarını çalıştırır. Boş zengin satır ilk
  koşumun font metriklerini ve asgari satır yüksekliğini korur; sıfır yüksekliğe
  sessizce daralmaz.
- **Parite ve maliyet sınırı:** Homojen legacy satır, exact rich yüz/caret kanıtı
  istemedikçe fallback yüzü çözmez ve caret duraklarını üretmez; platformdan
  bağımsız uyumluluk durakları ilk caret sorgusunda tembel kurulur. Rich yol
  platform duraklarını ve fiziksel yüzü eager taşır. Arka plan birleştirmesi
  upstream ile aynı bildirilmiş renk sınırını izler. Upstream alt/üstü çizginin
  bildirilmiş stilini etkin çözülmüş stille karşılaştırdığı için miras alınan
  renkli özdeş stilleri koşum sınırında ayırır; yerel yol iki tarafı da çözüp
  görünür stili özdeş bitişik koşumları tek segmentte tutar. Böylece düz çizgide
  dikiş ve dalgalı çizgide koşum-bazlı faz sıfırlaması oluşmaz. Karma BiDi'de
  mantıksal koşum yeniden çözülür ve wrap sınırında segment yeni boya satırına
  ayrılır. Üstü çizgi yerel yolda fiziksel koşum taban çizgisinin `0.25 × ascent`
  üstüne sabitlenir. Upstream'in homojen formülü ayrıca satır üst boşluğunun
  yarısını çıkarır; bu nedenle iki konum yalnız satır yüksekliği `ascent + descent`
  olduğunda aynıdır. Yerel sapma, istenen satır yüksekliği değiştiğinde çizgiyi
  glif kutusu içinde kaydırmak yerine taban çizgisine bağlı tutar ve aynı kuralı
  koşum-bazlı metriklere uygular. Kesintisiz alt çizginin düşey konumu shaper'ın
  bütün satır descent'i yerine katılan fiziksel yüzlerin koşum-bazlı descent
  değerlerinden en derin olanı kullanır; farklı koşumlarda basamak üretmez.
  `with_len` ve `split_at`, byte sınırı ile caret geometrisini yeniden eşlemek
  zorunda oldukları için legacy uyumluluk duraklarını eager kurabilir; depoda
  üretim çağıranı bulunmayan bu dönüşümler sıradan shape/paint sıcak yolunun
  tembelliğini etkilemez.
- **Kimlik sınırı:** Fiziksel yüz kimliği kaynak içeriği veya native kaynak
  anahtarı, gerçek bilinen koleksiyon yüz indeksi ve PostScript ayrımıyla
  nitelendirilir. Parmak izi kriptografik bir kanıt değil, aynı `TextSystem`
  kapsamındaki olasılıksal bir kimliktir. GPUI bugün değişken font eksen
  koordinatı taşımadığından var olmayan koordinatlar kimlik güvencesi olarak
  ileri sürülmez; böyle bir yüzey eklenirse koordinatlar kimliğe ayrıca katılır.
- **Hedef sınırı:** Zengin layout ve içerik-parmak izli fiziksel yüz kanıtı bugün
  macOS CoreText (`font-kit`) ile WGPU/cosmic-text backend'lerinde uygulanmıştır.
  Test amaçlı `NoopTextSystem` koşum ölçülerini taşır ancak fiziksel yüz kanıtı
  üretmez. DirectWrite eski homojen yolu derlenebilir tutar; Windows'ta
  `layout_rich_line` desteklenmiyor hatası döndürür ve olumlu hedef sayılmaz.
  Cosmic-text küme içi kesin caret koordinatı vermediğinden WGPU ligatür içi
  grafem durakları kümenin iki fiziksel kenarı arasında doğrusal yaklaşıktır;
  testler bu durakların varlığını ve kenarlar içinde kalmasını kanıtlar, fontun
  yerel ligatür-caret tablosunu kullandığını ileri sürmez.
- **Dosyalar:** `AGENTS.md`, `SAPMALAR.md`, `UPSTREAM.md`, `EXTRACTION.md`,
  `yetenek.md`, `crates/gpui/src/platform.rs`,
  `crates/gpui/src/text_system.rs`,
  `crates/gpui/src/text_system/line_layout.rs`,
  `crates/gpui/src/text_system/line.rs`,
  `crates/gpui_macos/src/text_system.rs`,
  `crates/gpui_wgpu/src/cosmic_text_system.rs`, yeni ortak alanlarla eski
  DirectWrite yolunu kaynak-uyumlu tutan
  `crates/gpui_windows/src/direct_write.rs`, `scripts/verify-sapmalar.sh` ve
  hedefe özgü doğrulama testleri.
- **Bırakma koşulu:** Kayıtlı Zed revizyonu aynı gözlenebilir güvenceleri veren
  koşum-bazlı ölçü, exact fallback-yüz kanıtı, affinity+yönlü caret durakları ve
  geometri/yerleşim/boya ayrımını birlikte sunduğunda yerel API kaldırılır,
  upstream karşılığı normal senkronla alınır ve bu madde silinir.
- **Upstream durumu:** Gönderilmedi. `../zed` bu çalışma için salt okunur kaynak
  deposudur ve onu değiştirme ya da upstream'e değişiklik gönderme yetkisi
  verilmemiştir.

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
