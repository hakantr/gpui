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
ilk madde bağımsız deponun manifest metadata'sını, “Zengin şekillendirilmiş satır
geometrisi” ise açıkça kayıtlı runtime ve public API yüzeyini değiştirir. İkisi de
senkronda korunmaları gerektiği için burada kayıtlıdır.

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
  tembelliğini etkilemez. `split_at`, görsel glif sırasının mantıksal byte
  sırasıyla aynı olduğunu varsaymaz: iki yarının gliflerini mantıksal indekse
  göre seçer, her yarıyı kendi caret sınır kutusuna göre yeniden tabanlar ve
  sağ indeksleri güvenli biçimde düşürür. Böylece RTL koşumundaki azalan
  indeksler aritmetik taşma/panik üretmez. Var olan glifleri bölmek bir ligatür
  veya combining kümesini yeniden şekillendiremeyeceğinden `split_at` yalnız
  gerçek shaped glif-küme sınırlarını kabul eder; küme içi ayrım için iki yarı
  yeniden şekillendirilmelidir. Legacy boya koşumu çözümü son bulunan
  aralığı tekrar kullanır ve yalnız BiDi sıçramasında ikili aramaya döner;
  homojen platform şekillendirme yolu koşum-bazlı ölçü araması yapmaz. Public
  `caret_stops: Vec<_>` alanı korunurken doc-hidden legacy tembel deposu ince
  bir `Arc` sapı taşır; `ResolvedFontFace` içeriği klon başına çoğaltılmak
  yerine paylaşılır ve backend kaynak parmak izleri fiziksel yüz anahtarına
  göre önbelleklenir. Üst `TextSystem` ve CoreText sıcak yüz-cache okumaları
  sıradan paralel okuma kilidi kullanır; CoreText'in pahalı ilk kaynak çözümü
  yüz anahtarına bağlı `OnceLock` ile tek atımlıdır. Bunlar zengin yolun
  gözlenebilir güvencelerini ve public alan tipini korurken legacy sıcak yol ve
  satır başına bellek maliyetini sınırlar. WGPU'nun ASCII/LTR ortak yolu caret
  kümelerini zaten mantıksal sırada olan cosmic-text gliflerinden doğrusal
  üretir; `HashMap`, genel BiDi küme birleştirmesi ve koşulsuz sıralamayı yalnız
  karma düzene bırakır. Rich ölçü koşumu seçimi de mantıksal indeksler ilerlediği
  sürece tek geçişli cursor kullanır, BiDi indeks gerilemesinde ikili aramaya
  döner. Genel algoritmayla sonuç eşitliği debug testlerinde, ASCII ligatür içi
  duraklar dahil, birebir karşılaştırılır. Yaklaşık 3,8 KB ASCII Criterion
  örneğinde bu iki hızlı yol önceki rich uygulamaya göre `%10–14` süre azalması
  verdi; aynı son ikilide legacy'ye göre fark homojen rich için `%2,4`, dört
  farklı ölçü koşumu için `%5,5`, 64 koşum stresinde `%17,8` ölçüldü. CoreText'in
  tek native koşumunu yalnız
  `baseline_shift` gibi
  şekillendirmeye katılmayan ölçülerle çok sayıda GPUI koşumuna ayırırken her
  alt koşuma native koşumun tam glif kapasitesi ayrılmaz; hedefli test 256 glif
  için karesel 65.536 kapasite rezervasyonunun doğrusal sınıra indiğini kanıtlar.
  CoreText zengin caret üretimi artık her UTF-16 sınırı için ayrı
  `CTLineGetOffsetForStringIndex` çağrısı yapmak yerine native
  `CTLineEnumerateCaretOffsets` akışını bir kez tüketir; native küme kenarları
  doğrudan kullanılır ve enumerator yalnız çok karakterli kümelerin iç
  duraklarını tamamlar. Debug derlemesi aynı sonucu eski genel algoritmayla her
  çağrıda birebir karşılaştırır. Aynı yaklaşık 3,8 KB Helvetica satırında
  homojen rich süre 26,874 ms'den 1,517 ms'ye (`%94,35`), 64 baseline koşumu
  26,646 ms'den 1,623 ms'ye (`%93,91`) indi. Caret üretmeyen legacy yol
  175,87 µs ve 176,56 µs arasında istatistiksel olarak değişmedi. cosmic-text
  için baseline-only koşumları önceden birleştiren daha saldırgan bir tasarım da
  ölçüldü; backend eşdeğer bitişik nitelikleri zaten birleştirdiği için 1,721
  ms'den 1,775 ms'ye yaklaşık `%3` geriledi ve üretim kodundan çıkarıldı.
- **Kimlik sınırı:** Fiziksel yüz kimliği kaynak içeriği veya native kaynak
  anahtarı, gerçek bilinen koleksiyon yüz indeksi ve PostScript ayrımıyla
  nitelendirilir. Parmak izi kriptografik bir kanıt değil, aynı `TextSystem`
  kapsamındaki olasılıksal bir kimliktir. GPUI bugün değişken font eksen
  koordinatı taşımadığından var olmayan koordinatlar kimlik güvencesi olarak
  ileri sürülmez; böyle bir yüzey eklenirse koordinatlar kimliğe ayrıca katılır.
- **Hedef sınırı:** Zengin layout ve içerik-parmak izli fiziksel yüz kanıtı bugün
  macOS CoreText (`font-kit`), Linux/Web'in kullandığı WGPU/cosmic-text ve
  Windows DirectWrite backend'lerinde işletim sisteminin kendi metin motoruna
  uyarlanmıştır. DirectWrite per-range font boyutunu native layout'a verir;
  baseline/asgari yükseklik ölçülerini shaping girdisinden ayırır, BiDi caret
  kenarlarını `bidiLevel` ve glyph-run geometrisinden kurar, yalnız ligatür içi
  sınırlar için `HitTestTextPosition` çağırır ve fallback yüzünü DirectWrite
  dosya referans anahtarı + koleksiyon yüz indeksiyle niteler. macOS ve WGPU
  yolları çalışma-zamanı testleriyle doğrulanmıştır. DirectWrite bu çalışmada
  `x86_64-pc-windows-msvc` için type-check edilmiştir fakat Windows hostu
  bulunmadığından çalışma-zamanı/BiDi ve performans kanıtı henüz olumlu hedef
  sayılmaz. Aynı WGPU backend'i bu çalışmada `x86_64-unknown-linux-musl` için,
  Web platformu ise hem tek iş parçacıklı hem Zed CI atomics/build-std
  yapılandırmasıyla type-check edilmiştir. Test amaçlı `NoopTextSystem` koşum
  ölçülerini taşır ancak fiziksel yüz kanıtı üretmez.
  Cosmic-text küme içi kesin caret koordinatı vermediğinden WGPU ligatür içi
  grafem durakları kümenin iki fiziksel kenarı arasında doğrusal yaklaşıktır;
  testler bu durakların varlığını ve kenarlar içinde kalmasını kanıtlar, fontun
  yerel ligatür-caret tablosunu kullandığını ileri sürmez.
- **Dosyalar:** `AGENTS.md`, `SAPMALAR.md`, `UPSTREAM.md`, `EXTRACTION.md`,
  `yetenek.md`, `crates/gpui/src/platform.rs`,
  `crates/gpui/src/text_system.rs`,
  `crates/gpui/src/text_system/line_layout.rs`,
  `crates/gpui/src/text_system/line.rs`,
  `crates/gpui_macos/src/text_system.rs`, `crates/gpui_macos/Cargo.toml`,
  `crates/gpui_macos/benches/layout_rich_line.rs`,
  `crates/gpui_wgpu/src/cosmic_text_system.rs`,
  `crates/gpui_wgpu/benches/layout_line.rs`, eşdeğer benchmark codegen profilini
  sabitleyen kök `Cargo.toml`, yeni ortak alanlarla eski
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

## İleride değerlendirilecek iyileştirmeler

Bu bölüm uygulanmış veya senkron sırasında korunacak bir sapma kaydı değildir.
Buradaki bir iş üretim koduna alınmadan önce güncel upstream yeniden karşılaştırılır,
gerçek tüketici sınırı tekrar kanıtlanır ve gerekiyorsa yukarıdaki bilinçli sapma
kaydı uygulamadan önce güncellenir.

### Rich ikincil geometrinin lazy/cache tabanlı API'si

- **Amaç:** Çizim veya ölçüm için rich şekillendirme isteyen fakat caret/hit-test
  geometrisini hemen kullanmayan çağıranların bütün caret duraklarını peşin
  üretme maliyetini ödememesini sağlamak. Mevcut yaklaşık 3,8 KB CoreText
  örneğinde legacy yol 176,56 µs iken eager rich yol homojen koşumda 1,517 ms,
  64 baseline koşumunda 1,623 ms ölçüldü; koşum sayısının ek maliyeti sınırlı,
  kalan farkın başlıca adayı eager ikincil geometri üretimidir.
- **Korunacak ihtiyaç:** Koşum-bazlı ölçüler, exact fiziksel fallback-yüz kimliği,
  affinity+yönlü çift BiDi durakları, ligatür/combining sınırları ve aynı değişmez
  geometriyi paint-only yeniden kullanma güvenceleri kaybedilemez. Optimizasyon
  gözlenebilir sonucu eksiltemez veya caret sorgusunu yaklaşıklaştıran yeni bir
  genel kural getiremez.
- **İncelenecek tasarım:** Temel glif/koşum geometrisini hemen üret; sorgu-amaçlı
  caret haritasını ilk kullanımda tek kez kurup sonuçla birlikte cache'le. Tek
  nokta hit-test ve tek indeks konumu için bütün haritayı kurmadan platformun
  doğrudan sorgusunu kullanma, ardışık editör sorgularında ise toplu haritayı bir
  kez üretme seçeneklerini ayrı ölç. Public `caret_stops: Vec<_>` alanı gerçek
  tembelliği engellediğinden kırıcı alan değişikliği yerine additive sorgu/yapı
  yüzeyi ve aşamalı uyumluluk yolu önceliklidir.
- **Platform uyarlaması:** CoreText'te tekil offset/index API'leri ile toplu
  `CTLineEnumerateCaretOffsets`; DirectWrite'ta tutulmuş layout ve native hit-test;
  WGPU/cosmic-text'te mevcut cluster verisi ve yalnız gerekli genel BiDi
  fallback'i kullanılmalıdır. Bir platformun primitive'i diğerine yapay olarak
  kopyalanmamalı; ortak API aynı semantiği platforma özgü en ucuz yolla vermelidir.
- **Ölçüm ve doğrulama kapısı:** Benchmarklar `shape/paint için caret kullanılmadı`,
  `ilk caret sorgusu`, `cache-hit caret sorgusu` ve `bütün durakları materialize et`
  evrelerini ayrı raporlamalıdır. Legacy, homogeneous rich, 64 baseline ve gerçek
  metrik koşumları macOS ile WGPU'da yeniden ölçülmeli; Windows'ta runtime ve
  throughput kanıtı alınmadan çalışma çapraz-platform tamamlanmış sayılmamalıdır.
  İlk rich oluşturma ölçülebilir biçimde iyileşirken ilk-sorgu dahil toplam süre
  mevcut eager yolun üzerine anlamlı biçimde çıkmamalı; bütün caret/face/run
  çıktıları mevcut testlerle birebir eşit kalmalıdır.

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
