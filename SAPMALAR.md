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
geometrisi” ise açıkça kayıtlı runtime ve public API yüzeyini değiştirir. Üçüncü madde
“Dış yüzey (external surface) ilkeli” dışarıda üretilmiş bir GPU yüzeyini GPUI sahnesine
yerleştiren yetenektir ve **uygulanmıştır**. Dördüncü madde “Kardeş `wgpu` checkout'unun
seçilmesi” bu yeteneğin wgpu profillerinde üreticiyle cihaz paylaşmasının önkoşuludur. Beşinci
madde “Dış yüzeyde izlenen yayın, bırakma güvenliği ve registry gözlemi” ile altıncı madde “Dış
yüzey nesil geçişi” aynı yeteneğin yaşam döngüsü doğruluğu için gereken yüzeylerdir; ikisi de
**uygulanmıştır**. Altısı da senkronda korunmaları gerektiği için kayıtlıdır.

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
  zaman fiziksel yüze geri çözülemiyor ve karma BiDi sınırlarında aynı UTF-8
  indeksi için affinity ve yön taşıyan birden çok görsel caret durağı yok.
  Yeni upstream `LineLayout::paint` ve `split_at` ile temel geometri/boya ve bölme
  giriş noktalarını açmış olsa da tek ölçülü layout'u affinity/yönlü caret veya
  fiziksel yüz kimliği taşımıyor; `split_at` glif indekslerinin artan mantıksal
  sırada olduğunu varsayan `partition_point` kullanıyor. Üstelik görsel sırada
  gezilen BiDi gliflerinin mantıksal byte indeksleri geri
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
  Yeni upstream'in `LineLayout::paint`, `LineLayout::split_at` ve
  `ShapedLine::split_at` public giriş noktaları korunmuştur. Yerel `split_at`
  semantiği artık ortak `LineLayout` seam'inde uygulanır; `ShapedLine` yalnız
  boya koşumlarını ve metni bölerken ona delege eder. `with_len` ve `split_at`,
  byte sınırı ile caret geometrisini yeniden eşlemek
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
  duraklarını tamamlar. Debug derlemesindeki genel karşılaştırıcı da yalnız
  CoreText enumerator'ının geçerli saydığı caret taraflarını sınar;
  `CTLineGetOffsetForStringIndex`in emoji-ZWJ içindeki geçersiz skaler sınırlara
  döndürdüğü sentetik `x=0` değerlerini gerçek caret durağı saymaz. Hedefli
  testler combining ve ZWJ kümelerinin yalnız dış kenarlarının
  kullanılabildiğini sabitler. Aynı
  yaklaşık 3,8 KB Helvetica satırında homojen rich süre 26,874 ms'den 1,517
  ms'ye (`%94,35`), 64 baseline koşumu 26,646 ms'den 1,623 ms'ye (`%93,91`)
  indi. Caret üretmeyen legacy yol
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
  `crates/gpui_web/src/text_system.rs`, `crates/gpui_web/src/run_replacements.rs`,
  `crates/gpui_wgpu/benches/layout_line.rs`, eşdeğer benchmark codegen profilini
  sabitleyen kök `Cargo.toml`, yeni ortak alanlarla eski
  DirectWrite yolunu kaynak-uyumlu tutan
  `crates/gpui_windows/src/direct_write.rs`, `scripts/verify-sapmalar.sh` ve
  hedefe özgü doğrulama testleri.
- **Senkron damgası — 2 Ekim 2026, Zed `309f91f` (Linux'ta uygulandı):**
  - Upstream'in `ShapedLineCursor`'u (#64185) public yüzeyiyle korunur; her parça yerel bölme
    seam'inden (`ShapedLine::split_at`) üretilir. Küme sınırı şartı ve parçanın kendi caret
    sınırlarına göre tabanlanması aynen geçerlidir. Upstream'in bayt sıralı satırlar için doğrusal
    kopyalama yolu bu depoda yoktur: her parça kalan uzunlukla orantılı bir bölme maliyeti öder.
  - Alt çizgi işleyicisi (`paint_with_underline_handler`) korunur. Bir segmentin aralığı, kapsadığı
    mantıksal dekorasyon koşumlarının bayt aralığı zarfıdır; koşumlar mantıksal indeksle çözüldüğü
    için BiDi'de iki yöne genişleyebilir. Düşey konum yerel yüz-descent kuralını izler, yalnız
    homojen formülü veren upstream `underline_y_offset` yardımcısı da public kalır.
  - Upstream'in yazı tipi nesli önbellek geçersizlemesi zengin satır önbelleğini de kapsar;
    `layout_rich_line` girişte nesli denetler.
  - Web artık upstream'in `WebTextSystem`'ini kullanır. `font_face` ve `layout_rich_line`
    cosmic-text yerel sistemine delege edilir; Canvas glif yedeği yalnız legacy satırda kalır, çünkü
    şekillendirme sonrası glif değiştirmek zengin satırın caret ve fiziksel yüz güvencesini bozar.
    Zengin satır çözemediği grafemleri eksik glif olarak raporlar. Canvas yedeğinin böldüğü koşumlar
    koşum ölçülerini taşır; Canvas yüzü fiziksel kimlik taşımaz.
  - cosmic-text'te upstream'in `FontMatchProperties::attributes` yolu yerel heterojen ölçülerle
    birleşir; eksik glif raporu zengin koşumları yazı tipi/uzunluk çiftine indirger.
  - Upstream'in yeni testleri yerel alanlar ve bu kurallarla uyarlandı (`EXTRACTION.md`, test-only
    uyarlamalar). Linux kanıtı: `scripts/verify-sapmalar.sh` 50 taşınabilir + 35 cosmic-text testi;
    CoreText ve DirectWrite bu hostta koşmadı.
- **Bırakma koşulu:** Kayıtlı Zed revizyonu aynı gözlenebilir güvenceleri veren
  koşum-bazlı ölçü, exact fallback-yüz kanıtı, affinity+yönlü caret durakları ve
  geometri/yerleşim/boya ayrımını birlikte sunduğunda yerel API kaldırılır,
  upstream karşılığı normal senkronla alınır ve bu madde silinir.
- **Upstream durumu:** Gönderilmedi. `../zed` bu çalışma için salt okunur kaynak
  deposudur ve onu değiştirme ya da upstream'e değişiklik gönderme yetkisi
  verilmemiştir.

### Dış yüzey (external surface) ilkeli

- **Durum:** Uygulandı. Altı runtime profili aynı ilkeli taşır: macOS Metal, Windows D3D11,
  Linux wgpu-Vulkan, Linux wgpu-GL, Browser WebGPU ve Browser WebGL2. Dış sözleşme sürümü
  `EXTERNAL_CONTRACT_VERSION = 2.0`'dır (`crates/gpui/src/external_surface.rs`).
- **Sınır:** Kayıtlı upstream GPUI, dışarıda üretilmiş bir GPU yüzeyini sahneye doğru draw order ile
  yerleştiremez. `PaintSurface`'in yükü yalnız macOS'taki `CVPixelBuffer`'dır ve `SurfaceSource`'un tek
  varyantı odur. Metal yalnız NV12 çizer, D3D11 `draw_surfaces` boştur, wgpu'da
  `PrimitiveBatch::Surfaces` işlem yapmaz. Hiçbir backend'de dış doku ithali yoktur.
- **Elenen tüketici yolu:** Sahneyi CPU'da rasterleyip `Window::paint_image` ile yüklemek doğru sonuç
  verir, ama kare başına tam yüzey yükleme öder (1100×720'de kare başına yaklaşık 3 MiB). İçerik her
  karede değiştiğinde önbellek de isabet etmez.
- **Kazanç:** GPU'da üretilen içerik GPUI karesine geri okuma ve yükleme olmadan, doğru draw order,
  kırpma, içerik maskesi ve grup opaklığıyla girer.
- **Ne yapar:**
  - `Window::paint_external_surface` ve `Window::paint_external_surface_tracked`, opak bir
    `ExternalSurfaceHandle` taşıyan `ExternalSurfaceDescriptor` ile
    `PaintSurface { source: SurfaceSource::External, .. }` ilkelini sahneye ekler. Yerleşim sırası:
    kırpma → `bounds`'a yerleştirme → `bounds`'un sol üst köşesi etrafında afin dönüşüm → içerik
    maskesi → grup opaklığı.
  - `Window::external_surface_capabilities` desteklenen byte sırasını (BGRA/RGBA), örneklemeyi
    (nearest/linear), senkronizasyon belirtecini, afin/kırpma/maske desteğini, CPU yedeğini ve eski
    nesli yeniden kullanma politikasını bildirir. Sabit boyut, piksel, bayt ya da yüzey sayısı tavanı
    yoktur.
  - Backend registry'leri (`gpui_wgpu`, `gpui_windows`, `gpui_apple`) yüzeyi GPUI'nin kendi cihazında
    oluşturur ve handle'ı çizim anında çözer. Tek boyut sınırı cihazın kendisidir: wgpu'da
    `max_texture_dimension_2d`, D3D11'de `CreateTexture2D`'nin reddi, Metal'de GPU ailesinin en büyük
    2B doku kenarı. Cihazın oluşturamayacağı yüzey `TransientFailure` ile reddedilir.
  - Üretici erişicileri GPUI'nin cihazını (ve wgpu'da kuyruğunu), kayıt/bırakma işlemlerini ve cihaz
    neslini verir; GPUI'nin render pass'ini, swap chain'ini ya da shader kaynağını vermez:
    `gpui_wgpu::WgpuRenderer::external_surface_producer`, `gpui_windows::external_surface_producer`,
    `gpui_macos` üzerinden `gpui_apple::ExternalSurfaceProducer`, `gpui_linux`'ta Wayland ve X11
    erişicileri, `gpui_web::external_surface_producer`.
  - `gpui_linux`'taki iki üretici yeniden dışa vurumu kendi pencere özelliğine bağlıdır
    (`#[cfg(feature = "wayland")]`, `#[cfg(feature = "x11")]`); böylece pencere özellikleri bağımsız
    seçilebilir.
  - `gpui_apple`, Apple registry'sinin kullandığı `collections` bağımlılığını macOS hedefinde taşır.
  - Hatalar `ExternalSurfaceError` ile türlenir: `UnsupportedCapability`, `InvalidGroup`, `DeviceLost`,
    `SynchronizationFailed`, `TransientFailure`, `FormatMismatch`, `StaleGeneration`,
    `ContractVersionMismatch`. Hiçbir ihlal panik ya da sessiz düşüş değildir.
  - Cihaz kaybı bütün handle'ları birlikte geçersiz kılar; handle'ın nesli zorunludur.
- **Dosyalar:**
  - çekirdek: `crates/gpui/src/external_surface.rs` (yeni), `crates/gpui/src/gpui.rs`,
    `crates/gpui/src/scene.rs`, `crates/gpui/src/elements/surface.rs`, `crates/gpui/src/window.rs`,
    `crates/gpui/src/platform.rs`;
  - wgpu: `crates/gpui_wgpu/src/external_registry.rs` (yeni), `crates/gpui_wgpu/src/wgpu_renderer.rs`,
    `crates/gpui_wgpu/src/shaders.wgsl`, `crates/gpui_wgpu/src/gpui_wgpu.rs`;
  - D3D11: `crates/gpui_windows/src/external_registry.rs` (yeni),
    `crates/gpui_windows/src/directx_renderer.rs`, `crates/gpui_windows/src/shaders.hlsl`,
    `crates/gpui_windows/src/gpui_windows.rs`, `crates/gpui_windows/src/window.rs`,
    `crates/gpui_windows/build.rs` (SM 4.0 + SM 5.0 iki shader varyantı);
  - Metal: `crates/gpui_apple/src/external_registry.rs` (yeni), `crates/gpui_apple/src/metal_renderer.rs`,
    `crates/gpui_apple/src/shaders.metal`, `crates/gpui_apple/src/gpui_apple.rs`,
    `crates/gpui_apple/build.rs` (cbindgen girdileri), `crates/gpui_apple/Cargo.toml`;
  - macOS: `crates/gpui_macos/src/external_registry.rs` (yeni), `crates/gpui_macos/src/gpui_macos.rs`,
    `crates/gpui_macos/src/window.rs`;
  - Linux: `crates/gpui_linux/src/gpui_linux.rs`, `crates/gpui_linux/src/linux.rs`,
    `crates/gpui_linux/src/linux/wayland.rs`, `crates/gpui_linux/src/linux/wayland/client.rs`,
    `crates/gpui_linux/src/linux/wayland/window.rs`, `crates/gpui_linux/src/linux/x11.rs`,
    `crates/gpui_linux/src/linux/x11/client.rs`, `crates/gpui_linux/src/linux/x11/window.rs`;
  - web: `crates/gpui_web/src/external_surface.rs` (yeni), `crates/gpui_web/src/gpui_web.rs`,
    `crates/gpui_web/src/window.rs`;
  - `Cargo.lock`.
- **Kurallar:** Sıradan GPUI karesi dış yüzey yoluna girmez; sahnede dış yüzey yoksa ek tahsis, dal,
  pass ya da senkronizasyon maliyeti doğmaz. Yüzey içeriği premultiplied, sRGB kodlu unorm'dur; grup
  opaklığını yalnız GPUI uygular. Senkron sırasında ilkel upstream'in renderer yapısına yeniden uygulanır.
- **Bırakma koşulu:** Kayıtlı Zed revizyonu dışarıda üretilmiş bir yüzeyi aynı karede, aynı
  gözlenebilir güvencelerle (draw order, kırpma, maske, opaklık, nesil) çizmeyi sağladığında madde
  silinir.
- **Upstream durumu:** Gönderilmedi. `../zed` bu depo için salt okunur kaynak deposudur.

### Kardeş `wgpu` checkout'unun seçilmesi

- **Durum:** Uygulandı (`ed6a8c815b`, `339f9f257a`). Kayıt bu senkronda geriye dönük olarak
  açıldı: değişiklik `AGENTS.md`'nin olağan standalone uyarlama izni kapsamında sayılmış, fakat
  wgpu'nun ana sürüm atlaması uygulama tarafından gözlenebilir davranış ve validasyon değişikliği
  taşıdığı ve seçim tüketiciye gerçek bir yetenek verdiği için sıradan bir uyarlama değildir.
- **Sınır:** Kayıtlı Zed revizyonu `wgpu = "29.0.4"` seçiyor ve onu crates.io'dan alıyor. Dış
  yüzeyin üretici yüzü (`ExternalSurfaceProducer`) üreticiye `Arc<wgpu::Device>` ve
  `Arc<wgpu::Queue>` uzatır. Rust için bir registry crate'i ile bir path crate'i, sürümleri aynı olsa
  bile ayrı tiplerdir; iki depo wgpu'yu farklı kaynaklardan çözdüğünde üreticinin `wgpu::Device`'ı
  ile GPUI'nin uzattığı `wgpu::Device` birleşmez ve ödünç alma **hiç derlenmez**. Bu, dış yüzeyin
  wgpu profillerinde çalışmasının önkoşuludur, bir hız tercihi değil.
- **Elenen tüketici yolu:** Üretici tarafında çözülemiyor. Üretici GPUI'nin cihazında çizmek
  zorundadır, çünkü yüzey o cihazda yaşar; kendi cihazını yaratamaz, dolayısıyla tipi kendi
  tarafında üretemez. Tip kimliğini zorlamanın geri kalan yolları elendi: `unsafe` transmute cihaz
  ömrü ve iç değişmezleri hakkında hiçbir garanti vermez; wgpu'yu üretici deposunda vendor'lamak aynı
  iki-instance sorununu ters yönde kurar; her iki tarafı da yayınlanmış `29.0.4`'e sabitlemek ise
  gereken `Queue::present` ve `SurfaceColorSpace` yüzeyini geri alır.
- **Kazanç:** İkili bir kazanç, ölçüm değil: iki depo aynı checkout'u gösterdiğinde üreticinin cihaz
  ödünç alması derlenir, göstermediğinde derlenmez. Dış yüzeyin altı runtime profilinden dördü
  (Browser WebGL2, Browser WebGPU, Linux wgpu-Vulkan, Linux wgpu-GL) bu yüze bağlıdır.
- **Dosyalar:** `Cargo.toml` (`wgpu` workspace bağımlılığı), `Cargo.lock`,
  `crates/gpui_wgpu/src/wgpu_context.rs`, `crates/gpui_wgpu/src/wgpu_renderer.rs`,
  `crates/gpui_wgpu/src/wgpu_atlas.rs`, `crates/gpui_wgpu/src/external_registry.rs`. Çağrı yeri
  imza uyarlamalarının tek tek dökümü `EXTRACTION.md`'dedir; burada kayıtlı olan, uyarlamaların
  kendisi değil **girdinin seçimi**dir.
- **Kurallar:** Seçilen revizyon sabitlenemez — path bağımlılığının amacı zaten iki deponun aynı
  çalışma ağacını paylaşmasıdır — bu yüzden her senkronda gözlemle kaydedilir ve `EXTRACTION.md`
  içindeki tabloya yazılır. Kardeş ağaç ilerlediğinde ortaya çıkan API değişiklikleri çağrı yeri
  uyarlamasıyla karşılanır; wgpu'nun davranış veya validasyon değiştiren maddeleri korunan
  shader'lara ve pipeline'lara karşı denetlenir ve sonucu `EXTRACTION.md`'ye yazılır. Kapılar
  host hedefiyle sınırlı tutulmaz: `cfg`-kapılı çağrı yerleri yüzünden wasm32 kontrolü de
  çalıştırılır.
- **Bırakma koşulu:** Kayıtlı Zed revizyonu, tüketicinin de crates.io'dan alabileceği bir wgpu
  sürümüne (30 hattı veya sonrası) geçtiğinde madde düşer: o noktada iki depo da yayınlanmış
  crate'i seçer, tipler registry üzerinden birleşir ve path bağımlılığı gereksizleşir.
- **Upstream durumu:** Gönderilmedi ve gönderilecek bir şey yok. Bu, Zed'de bir eksiklik değil,
  bu deponun bir tüketiciyle kaynak paylaşma biçimidir.

- **Senkron damgası — 1 Ekim 2026, upstream trunk'a geçiş (yalnız ekleme):**
  - **Sahip kararları (birebir).**
    - **EK-7:** *"ilk önce ../wgpu orjinal repo ile senkronla ondan sonra 2 seçenekği uygula"*;
      senkron hedefi *"En son upstream'e güncelle"*.
    - **EK-8:** *"şimdi bu ../wgpu ve ../gpui son haline senkronlama işi macosta olsun ve bu
      senkronu pushla. linux ortamında halen mevcut ../wgpu ve ../gpui ile çalışan farklı bir
      proje olduğu için linux ortanımdaki kardeş replar mevcut halini korusun. senkron işi macos
      ve windows ortamlarında bulunsun. en son diğer projede senkronlu kardeş repolar seviyesine
      çıktığında linux ortamını düzeltiriz. herşeyin linuxta ilk önce halledilecek kararı burada
      kırlıyor."*
    - **Taşıma yöntemi:** *"Ben koşarım: ff birleştirme (Önerilen)"*. Açıklaması: *"Force gerekmez,
      geçmiş yeniden yazılmaz: ağacı upstream'e eşit, ebeveynleri eski main (5b0edc1) +
      upstream/trunk olan tek birleştirme commit'i; main fast-forward ilerler. Eski commit
      geçmişte kalır, etiket de push edilir."* Bu seçim EK-7'deki "etiketle koru + force"
      kararının yerine geçti; force kullanılmadı.
  - **Seçilen girdi:**
    - `../wgpu` = `hakantr/wgpu` `main` `298906f40029e7e2331ff0b9ad61c2a920cef4aa`; birleştirme
      commit'i, ebeveynleri `5b0edc1` ve upstream `dd033bfb7fb3690eb2e80a971877a1ef8ecabe59`.
    - Ağaç `d718aa30…`, gfx-rs/wgpu trunk `dd033bf` (2026-10-01T19:16:14Z) ağacıyla **birebir
      aynı**. Commit mesajı `40f4aac8` der; ağaç bir commit sonraki `dd033bf`'nin ağacıdır. Geçmiş
      yeniden yazılmadı.
    - Eski çatal `5b0edc1` (`d4359d74` ağacı + yamalı `deno_webgpu/surface.rs`) birinci ebeveyndir
      ve `arsiv/ilk-kommit-5b0edc1` etiketiyle korunur. Yama, upstream'de zaten bulunan bir
      import sırasıydı; pin commit'i gerekmedi.
    - Sürüm `30.0.0`; MSRV en yüksek `1.95`. Toolchain 1.97.1 değişmedi.
  - **Kapsam (bu depoda):** yalnız `Cargo.lock` (wgpu ailesinde `portable-atomic`/
    `portable-atomic-util` yer değiştirir, `scopeguard` eklenir; 7 satır). Kaynak değişikliği yok.
    `d4359d74..dd033bf` arasındaki 298 upstream commit'inden hiçbiri buradaki çağrı yerlerini
    kırmadı. Yeni `TextureViewDescriptor::swizzle` alanı `..Default::default()` ile karşılanır.
  - **Ölçüm (macOS 27.0.1, Apple M4 Pro, Rust 1.97.1):**
    - `cargo check --workspace --all-targets` rc 0;
    - `cargo test --workspace --no-fail-fast` rc 0, 34 ikili 575/0 (4 ignored, upstream'den önceden
      var);
    - `cargo check -p gpui_wgpu --target wasm32-unknown-unknown` rc 0.
    - `gpui_web` wasm32, `wasm_thread`'in `#![feature]`'ı yüzünden stable'da düşer. Bu eski
      pinlerde de aynıdır ve wgpu'dan bağımsızdır.
    - `gpui_wgpu`'nun `x86_64-unknown-linux-musl` çapraz check'i bu host'ta `psm` C araç
      zincirinde düşer (araç zinciri sınırı).
  - **Linux:** EK-8 gereği Linux makinesindeki kardeş checkout'lar eski pinde (wgpu `5b0edc1`,
    gpui `6f0915b`) kalır. Bu senkron Linux'ta ölçülmedi.
  - **Bırakma koşulu değişmedi.**

- **Senkron damgası — 2 Ekim 2026, Linux (Zed `309f91f` senkronuyla birlikte):**
  - Sahip Linux ertelemesini kaldırdı; Linux checkout'ları wgpu `4cf1a22e6be82cb0159676676de8b9071114ef16`
    (`298906f` + Metal ithal kuyruk bekleyen command buffer sınırı ve senkron fault düzeltmesi) ve bu
    GPUI senkronuna ilerledi. Düzeltme buradaki çağrı yerlerine ve lock'a dokunmaz.
  - **Test uyarlaması:** `gpui_wgpu` shader testlerinin dev `naga`'sı crates.io `29.0.4` yerine kardeş
    `../wgpu/naga`'dır (çalışma alanı bağı). Naga 29, WGPU 30'un shader'ları derlediği doğrulayıcı
    değildir; testler aynı WGSL'i değişmeyen yetenek kümeleriyle (`Capabilities::empty()`, subpixel
    için upstream'in kümesi) gerçek kardeş doğrulayıcıdan geçirir ve geçti.
  - Upstream'in yeni headless kodundaki çağrı yeri uyarlamaları `EXTRACTION.md`'dedir.
  - **Ölçüm (Ubuntu 26.04, Rust 1.97.1, GeForce RTX 5070 Ti + Radeon iGPU):** `cargo check` ve
    `clippy -D warnings` (`--workspace --all-targets`) rc 0; seri çalışma alanı testleri 904/0
    (12 yoksayılan); wasm32 `gpui_wgpu --all-targets`, `gpui_web --no-default-features --all-targets`
    ve `gpui --lib` rc 0, varsayılan özellikli `gpui_web` önceki pinlerdeki `wasm_thread` engelinde.
  - **Bırakma koşulu değişmedi.**

### Dış yüzeyde izlenen yayın, bırakma güvenliği ve registry gözlemi

- **Durum:** Uygulandı (dış sözleşme 1.1 ve 1.2'de geldi; 2.0'da aynen geçerlidir).
- **Sınır:** Upstream, yayımlanmış bir dış yüzey için GPUI'nin kendi bildiği iki olguyu sormanın
  yolunu vermez: bir oluşumun bir tüketici çizim komutuna bağlanıp bağlanmadığı ve yayın geleceğe
  kapatıldıktan sonra artık hiçbir canlı sahnenin onu çözemeyeceği (monoton bırakma eşiği).
  Registry'nin anlık doluluğu (canlı kayıt sayısı, toplam bayt, ölçüldüğü kapsam ve nesil) da
  dışarıdan görülemez.
- **Elenen tüketici yolu:** Kaynağı sabit sayıda kare sonra bırakmak tahmindir: kare kırpılmış,
  sahneden düşmüş ya da cihaz kaybında hiç çizilmemiş olabilir. Hiç bırakmamak belleği sızdırır.
  Üreticinin gölge sayacı nesil ilerlemesini ve terminal düşüşleri göremez; ham registry'yi dışarı
  vermek gereğinden geniş olur.
- **Kazanç:** Üretici iki olguyu ve registry doluluğunu tahmin etmeden okur; bırakma kararı üreticide
  kalır. Fiziksel GPU tamamlanması ya da ekrana basılma iddia edilmez; adım doğruluk adımıdır.
- **Ne yapar:** `PublicationId`, `BindingProof`, `RetireWatermark` ve `RetireSafety`, yayını gelecek
  çizimlere kapatan `close`, atomik sahne devri (`SceneHandover`, `SceneReplaceOutcome`),
  `PublicationLedger`, `TrackedPublishError` ve salt okunur `RegistryObservation` (ölçülemeyen hâl
  türlenir). Sayaç tükenmesi (`CounterExhausted`) yapışkandır ve sarmaz.
- **Kurallar:** Sıradan yol sıfır maliyetlidir: dış yüzeyi olmayan pencerede devir boş küme ile
  işlem yapmaz. Bırakma eşiği canlı bir yayının üstünden atlamaz.
- **Dosyalar:** `crates/gpui/src/external_surface.rs`, `crates/gpui/src/window.rs`,
  `crates/gpui/src/platform.rs`, üç backend registry'si ve renderer'ların çizim kancaları,
  `gpui_linux` Wayland/X11, `gpui_windows`, `gpui_macos` ve `gpui_web` pencere uygulamaları.
- **Bırakma koşulu:** Kayıtlı Zed revizyonu eşdeğer yayın kimliği, bağlanma kanıtı, bırakma eşiği ve
  salt okunur registry gözlemi sağladığında madde silinir.
- **Upstream durumu:** Gönderilmedi.

### Dış yüzey nesil geçişi

- **Durum:** Uygulandı (dış sözleşme 1.3'te geldi; 2.0'da aynen geçerlidir).
- **Sınır:** Registry'ler gerçek cihaz kaybında nesli ilerletebilir, ama üretici bir pencerenin dış
  yüzey alanını bilinçli ve topluca yeniden kuramaz: GPUI'deki fiziksel kaynaklar, defter ve yayın
  kimlikleri üreticinin kendi nesliyle aynı işlemde eskimez. Eski nesilden bir üretici de yeni nesil
  durumuna dokunabilir.
- **Elenen tüketici yolu:** Yalnız bir tarafın neslini ilerletmek iki tarafı ayrıştırır. Handle'ları
  tek tek bırakmak toplu nesil anlamını bozar ve yarım başarıya açıktır. Ham registry ya da cihaz
  vermek gerçek sınırdan daha geniş bir API olur.
- **Kazanç:** Doğruluk: güncel nesildeki üretici sahip olduğu handle kümesini tam olarak sunduğunda
  tek başarılı işlem `{G, N, B} → {G+1, 0, 0}` makbuzu döner (`ExternalSurfaceGenerationRollover`);
  eski handle ve yayın kimliklerinin hepsi birlikte eskir. Eski nesilden üretici yeni nesli
  değiştiremez. `u64::MAX`'ta sarma yoktur: bilinçli geçiş mutasyonsuz `CounterExhausted` döner, gerçek
  kayıp terminaldir ve GPU yardımcılarını bırakır.
- **Ne yapar:** Üç registry ailesinde `rebuild_external_surface_domain`,
  `ExternalSurfaceGenerationRollover`, `ExternalSurfaceGenerationRolloverError`; renderer'larda nesil
  sonunda terminal kurtarma.
- **Dosyalar:** `crates/gpui/src/external_surface.rs`, `crates/gpui_wgpu/src/external_registry.rs`,
  `crates/gpui_windows/src/external_registry.rs`, `crates/gpui_apple/src/external_registry.rs` ve üç
  renderer.
- **Bırakma koşulu:** Kayıtlı Zed revizyonu eşdeğer sınırlı, toplu nesil geçişini sağladığında madde
  silinir.
- **Upstream durumu:** Gönderilmedi.

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
