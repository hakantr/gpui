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
“Bounded external-surface köprüsü” yetkilendirilmiştir ve **uygulanmıştır**; kaydı
`AGENTS.md` §Deliberate divergence maddesi 4 gereği koddan önce girmiş, kod indikçe
güncellenmiştir. Hangi adımın indiği maddenin kendi “Durum” alanında yazılıdır.
Dördüncü madde “Kardeş `wgpu` checkout'unun seçilmesi” o köprünün wgpu profillerinin
önkoşuludur ve kaydı koddan sonra, 20 Ağustos 2026 senkronunda açılmıştır. Beşinci madde
“Bounded external-surface köprüsünde izlenen yayın ve bırakma güvenliği” üçüncü maddenin alt
adımıdır; kaydı, tasarımı ve uygulaması yetkilendirilmiştir ve **uygulanmıştır**. Hangi hücrenin
kanıtlandığı ve hangisinin iddia edilmediği maddenin kendi “Durum” alanında yazılıdır. Beşi de senkronda korunmaları gerektiği için kayıtlıdır.

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

### Bounded external-surface köprüsü

- **Durum:** Yetkilendirildi; **uygulandı (altı runtime profilinin altısı da indi).** Kayıt
  `AGENTS.md` §Deliberate divergence maddesi 4 gereği koddan önce girmişti. Bugünkü durum:
  - **Uygulandı (core descriptor + scene, `8be17e6197`):** `crates/gpui/src/external_surface.rs`
    (yeni), `crates/gpui/src/gpui.rs`, `crates/gpui/src/elements/surface.rs`,
    `crates/gpui/src/scene.rs`, `crates/gpui/src/window.rs`,
    `crates/gpui_apple/src/metal_renderer.rs` (yalnız `match` genişletmesi; NV12 mantığı
    dokunulmadı). `gpui_windows` ve `gpui_wgpu` bu adımda değişiklik gerektirmedi.
  - **Uygulandı (D3D11 çizim yolu):** `crates/gpui_windows/src/external_registry.rs` (yeni;
    registry + D-K16 üretici erişimi `external_surface_producer`),
    `crates/gpui_windows/src/shaders.hlsl` (`external_surface_vertex`/`_fragment`),
    `crates/gpui_windows/src/directx_renderer.rs` (premultiplied blend'li external pipeline,
    feature-level'a göre shader modeli, `draw_surfaces`, device-lost'ta `invalidate_all`),
    `crates/gpui_windows/build.rs` (SM 4.0 + SM 5.0 iki varyant),
    `crates/gpui_windows/src/window.rs`, ve capability'yi platforma indiren additive
    `PlatformWindow::external_surface_capabilities` (`crates/gpui/src/platform.rs`,
    `crates/gpui/src/window.rs`). Windows'ta capability artık `supported: true` bildirir;
    bütçe sayıları A-K05 kapanana kadar geçicidir (4096×4096, 3 in-flight).
  - **Uygulandı (wgpu çizim yolu):** `crates/gpui_wgpu/src/external_registry.rs` (yeni; registry +
    D-K16 üretici erişimi `ExternalSurfaceProducer`), `crates/gpui_wgpu/src/shaders.wgsl`
    (`vs_external_surface`/`fs_external_surface`; ortak kaynakta olduğu için her iki shader
    varyantında da derlenir), `crates/gpui_wgpu/src/wgpu_renderer.rs` (premultiplied blend'li
    external pipeline, `draw_external_surfaces`, content_mask → `set_scissor_rect`, device-lost'ta
    `invalidate_all`), `crates/gpui_wgpu/src/gpui_wgpu.rs`, ve capability'yi platforma indiren
    aynı additive seam (`crates/gpui_linux/src/linux/x11/window.rs`,
    `crates/gpui_linux/src/linux/wayland/window.rs`, `crates/gpui_web/src/window.rs`). Tek
    uygulama dört runtime profilini birden açar: Browser WebGL2, Browser WebGPU, Linux
    wgpu-Vulkan, Linux wgpu-GL. Bu backend'lerde capability artık `supported: true` bildirir;
    byte sırası context'in seçtiğidir (BGRA; wasm+GL'de RGBA), sync token WebGL2'de
    `ContextOrdered`, diğerlerinde `SameQueueOrdered`, bütçe sayıları A-K05 kapanana kadar
    geçicidir (4096×4096 — cihaz `max_texture_dimension_2d`'si daha düşükse ona indirilir —,
    3 in-flight). **WebGL2 kısıtı:** external-surface shader'ı storage buffer kullanamaz; tek
    instance'lık uniform buffer ile yüzey başına bir draw yapılır.
  - **Uygulandı (Metal çizim yolu — altıncı ve son profil):**
    `crates/gpui_apple/src/external_registry.rs` (registry + D-K16 üretici yüzü),
    `crates/gpui_macos/src/external_registry.rs` (pencere kimliği olarak
    `HasWindowHandle`'ın verdiği `AppKitWindowHandle` NSView işaretçisini üretici yüzüne çözen
    AppKit seam'i), `crates/gpui_apple/src/shaders.metal`
    (`external_surface_vertex`/`_fragment`), `crates/gpui_apple/src/metal_renderer.rs`
    (premultiplied blend'li external pipeline, clamp-to-edge nearest/linear iki sampler,
    `draw_surfaces` artık batch'i kaynağa göre ardışık koşulara bölüp CoreVideo/NV12 yolunu
    olduğu gibi bırakıyor, content_mask → `set_scissor_rect`),
    `crates/gpui_apple/build.rs` (cbindgen'e `ExternalSurfaceInstance`/`ExternalSurfaceInputIndex`),
    `crates/gpui_apple/src/gpui_apple.rs`, `crates/gpui_macos/src/gpui_macos.rs`,
    `crates/gpui_macos/src/window.rs` (aynı additive
    `PlatformWindow::external_surface_capabilities` seam'i). macOS'ta capability artık
    `supported: true` bildirir: byte sırası BGRA, iki sampling modu, sync token
    `SameQueueOrdered` (fence veya keyed-mutex **iddia edilmez**), `cpu_fallback`/`sync_cpu_ready`
    kayıtlı yüzeyin storage mode'undan türetilir (unified'da `Shared`, değilse `Managed`;
    ikisinde de `replaceRegion` yasal olduğu için `true`), bütçe sayıları A-K05 kapanana kadar
    geçicidir (4096×4096, 3 in-flight = layer'ın `maximumDrawableCount`'u). **Metal'e özgü kayıt:**
    macOS'ta programatik device-loss bildirimi yoktur; bu yüzden `invalidate_all` vardır ve nesli
    artırır fakat bu backend'de onu otomatik çağıran hiçbir yol yoktur — uydurma bir tetikleyici
    eklemek yerine durum böyle kaydedilmiştir. Üretici pass'i AYRI bir `MTLCommandBuffer`'da,
    aynı `MTLCommandQueue` üzerinde, tüketiciden önce commit edilir (S3 kanıtı) ve GPU corpus
    testi bu sırayı bekleme koymadan doğrular.
  - **Uygulanmadı:** yok. `gpui_linux/headless` penceresinin renderer'ı olmadığı için varsayılan
    `unsupported()` snapshot'ında kalır; bu bir eksik adım değil, renderer'sız pencerenin doğru
    cevabıdır.
  Karşı taraf kaydı: `gpui-ec` deposu, öneri
  `a67b5c5504c354290b3ae1ebcf30c8847e3cb994` (`docs/B2_SAPMA_ONERISI.md`), dondurulmuş sözleşme
  `381951be5c25caa6dd7cc7ae435f669cadf93eaf` (`docs/KOPRU_SOZLESMESI.md`, contract v1.0;
  GPUI tarafıyla arasındaki bilinçli farklar orada §6a'da kayıtlıdır).
- **Sınır:** Kayıtlı upstream GPUI, dışarıda üretilmiş bir GPU yüzeyini sahneye doğru draw
  order ile yerleştiremiyor. `crates/gpui/src/scene.rs` içindeki `PaintSurface`'in yük alanı
  `#[cfg(target_os = "macos")] image_buffer: CVPixelBuffer`, yani alan diğer platformlarda
  hiç yok; `Window::paint_surface` ve `SurfaceSource`'un tek varyantı macOS'a kapalı.
  Renderer tarafında `metal_renderer.rs::draw_surfaces` NV12'ye hard-assert ediyor
  (`kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`), `directx_renderer.rs::draw_surfaces`
  boş stub, `wgpu_renderer.rs` karşılığı `PrimitiveBatch::Surfaces(_) => {}` no-op. Üç
  backend'in atlasları da yalnız `TEXTURE_BINDING | COPY_DST`; hiçbirinde dış texture import
  yolu, shared-handle veya `as_hal`/`from_raw` kullanımı yok. Sınır bir performans tercihi
  değil, yokluk.
- **Elenen tüketici yolu:** Tüketici bugün `Window::paint_image` ile sahneyi CPU'da rasterleyip
  tam yüzeyi `RenderImage` olarak yüklüyor (`gpui_canvas2/src/gpui_host.rs`). Yol doğru sonuç
  veriyor; elenme sebebi maliyeti: kanonik 1100×720 yüzeyde kare başına 3,02 MiB, 60 Hz'de
  ≈181 MiB/s, ve kamera hareket ettiği karelerde generation değiştiği için cache hiç isabet
  etmiyor. Aynı olgunun kontrollü ölçümü: aynı pixel corpus'unu birebir geçen iki modda
  CPU-upload yolu 57 normal karede 59,77 MB öderken doğrudan yol 0 byte ödedi.
- **Kazanç:** GPU'da üretilen gruplar için kare başına tam-yüzey upload'ı ortadan kalkar.
  Altı gerçek runtime profilinde ortak pixel corpus'u (7 kontrol × 3 içerik nesli) doğru draw
  order, scissor clip ve stale okuma olmadan geçti; normal karelerde readback ve upload 0:
  Windows D3D11 (FL 10.1) 42/42, Browser WebGL2 21/21, macOS Metal 35/35, Browser WebGPU
  21/21, Linux wgpu-Vulkan 21/21, Linux wgpu-GL 21/21. Zero-readback ayrıca araç düzeyinde
  doğrulandı: RenderDoc dökümünde normal pencerede 0 `CopyResource`/`Map`, kanıt karesinde 1.
  Dürüst sınır: köprü upload'ı yalnız GPU'da üretilen gruplar için kaldırır; CPU rasterizer
  tüketicisi için kazanç otomatik değildir, köprü o grupları GPU'ya taşımanın önkoşuludur.
- **Dosyalar (uygulanacak kapsam):** `crates/gpui/src/scene.rs`,
  `crates/gpui/src/window.rs`, `crates/gpui/src/elements/surface.rs`,
  `crates/gpui_apple/src/metal_renderer.rs`, `crates/gpui_apple/src/shaders.metal`,
  `crates/gpui_windows/src/directx_renderer.rs`, `crates/gpui_windows/src/shaders.hlsl`,
  `crates/gpui_wgpu/src/wgpu_renderer.rs`, `crates/gpui_wgpu/src/shaders.wgsl`,
  `crates/gpui_wgpu/src/shaders_webgl.wgsl`, `crates/gpui_apple/src/external_registry.rs`,
  platforma özgü producer lookup dosyaları ve karşılık gelen testler.
- **Kurallar:** Yüzey semantiği contract v1.0'a sabittir: opak `{ id, generation }` registry
  handle'ı, 8 bit/kanal `unorm` (BGRA birinci, RGBA fallback), **premultiplied** alpha,
  crop → yerleşim → transform → clip → opacity sırası ve render thread'i süresiz bekletmeyen
  sync politikası. Grup opaklığının tek sahibi GPUI composite'idir. Ham device/queue/encoder,
  consumer shader kaynağı, keyfî render pass veya callback GPUI public API'sine açılmaz.
  Değişiklik additive'dir: **yeni `PrimitiveKind` eklenmez** — mevcut `Surfaces` batch'i ve
  `Scene::batches()` draw-order interleave'i olduğu gibi kullanılır, `SurfaceSource` zaten
  enum olduğu için genişleme upstream'in kendi noktasındadır. Capability sorgusu `false`
  döndüğünde normal GPUI yolu ek allocation, branch, pass veya sync maliyeti ödemez.
  Backend'e özgü zorunluluklar: external pipeline her backend'de premultiplied blend kullanır
  (Metal'de genel `build_pipeline_state` straight alpha olduğundan path-sprite pipeline'ının
  blend çifti alınır); WebGL2 shader varyantı storage buffer kullanamaz; D3D11 surface
  shader'ları feature level'a göre shader modeli seçer (SM 5.0 bytecode FL 10.1'de reddedilir).
- **Bırakma koşulu:** Kayıtlı Zed revizyonu aynı gözlenebilir güvenceleri veren bounded
  external texture/surface import ve same-frame composite yüzeyini sağladığında madde silinir;
  `gpui-ec` upstream yüzeye adapter olur.
- **Upstream durumu:** Gönderilmedi. Zed'e önerilmeye aday genel bir yetenektir, fakat
  `../zed` bu çalışma için salt okunur kaynak deposudur ve upstream'e değişiklik gönderme
  yetkisi verilmemiştir. Yerel madde olarak tutulması, `AGENTS.md`'nin “özellikler Zed'de
  doğar” kuralından kullanıcı tarafından 13 Ağustos 2026'da açıkça verilmiş bir istisnadır.

### Kardeş `wgpu` checkout'unun seçilmesi

- **Durum:** Uygulandı (`ed6a8c815b`, `339f9f257a`). Kayıt bu senkronda geriye dönük olarak
  açıldı: değişiklik `AGENTS.md`'nin olağan standalone uyarlama izni kapsamında sayılmış, fakat
  wgpu'nun ana sürüm atlaması uygulama tarafından gözlenebilir davranış ve validasyon değişikliği
  taşıdığı ve seçim tüketiciye gerçek bir yetenek verdiği için sıradan bir uyarlama değildir.
- **Sınır:** Kayıtlı Zed revizyonu `wgpu = "29.0.4"` seçiyor ve onu crates.io'dan alıyor. Bounded
  external-surface köprüsünün üretici yüzü (`ExternalSurfaceProducer`) tüketiciye
  `Arc<wgpu::Device>` ve `Arc<wgpu::Queue>` uzatır. Rust için bir registry crate'i ile bir path
  crate'i, sürümleri aynı olsa bile ayrı tiplerdir; iki depo wgpu'yu farklı kaynaklardan
  çözdüğünde tüketicinin `wgpu::Device`'ı ile GPUI'nin uzattığı `wgpu::Device` birleşmez ve ödünç
  alma **hiç derlenmez**. Bu, köprünün wgpu profillerinde çalışmasının önkoşuludur, bir hız
  tercihi değil.
- **Elenen tüketici yolu:** Tüketici tarafında çözülemiyor. `gpui_ec_wgpu` kendi cihazını
  yaratmıyor — bu, dondurulmuş sözleşmenin D-K12 kararıdır ve tüketicinin GPUI'nin cihazında
  çizmesinin tek yolu odur — dolayısıyla tipi kendi tarafında üretemez. Tip kimliğini zorlamanın
  geri kalan yolları elendi: `unsafe` transmute cihaz ömrü ve iç değişmezleri hakkında hiçbir
  garanti vermez; wgpu'yu tüketici deposunda vendor'lamak aynı iki-instance sorununu ters yönde
  kurar; her iki tarafı da yayınlanmış `29.0.4`'e sabitlemek ise köprünün ihtiyaç duyduğu
  `Queue::present` ve `SurfaceColorSpace` yüzeyini geri alır.
- **Kazanç:** İkili bir kazanç, ölçüm değil: iki depo aynı checkout'u gösterdiğinde tüketicinin
  cihaz ödünç alması derlenir, göstermediğinde derlenmez. Kanıt karşı tarafta somuttur —
  `gpui-ec/crates/gpui_ec_wgpu/Cargo.toml` hem `wgpu`'yu hem `gpui_wgpu`'yu aynı iki kardeş yola
  bağlar, `crates/gpui_ec_wgpu/src/adapter.rs` ödünç alınan `Arc<wgpu::Device>`'ı taşır. Köprünün
  altı runtime profilinden dördü (Browser WebGL2, Browser WebGPU, Linux wgpu-Vulkan, Linux
  wgpu-GL) bu yüze bağlıdır.
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

### Bounded external-surface köprüsünde izlenen yayın ve bırakma güvenliği

- **Durum:** **Contract 1.1 uygulandı; M1–M4/N1–N24 birim/yapısal kabul kümesi belirtilen host
  ve hedeflerde yeşildir. Altı-profil runtime, fiziksel GPU completion veya present kanıtı
  tamamlanmadı ve iddia edilmiyor.** Kayıt `AGENTS.md` §Deliberate divergence maddesi 4 gereği
  **koddan önce** girmiştir. Üçüncü maddedeki köprünün alt adımıdır; contract **1.0 → 1.1**
  additive ilerledi (`EXTERNAL_CONTRACT_VERSION`, `crates/gpui/src/external_surface.rs:30`).
  Ölçüm hostları: macOS 26.6.2 / Apple M4 Pro (wgpu→Metal), Ubuntu 26.04 / RTX 5070 Ti + Radeon
  iGPU (wgpu), Windows 11 Pro / GeForce 210 sürücü 21.21.13.4201 (D3D11). Kanıt gpui-ec'te
  `evidence/f20-1-kapanis-yesil` girdisidir (`status: current`,
  `baseline_of: f20-0-kirmizi-temel`); pinler `gpui@7f203b7a0daf6032ea1cec9b48f245104ba6b13d`
  ve `gpui-ec@3ecdef24bc1a6a6f3ce938e55f57c0b6bd8aa666`.
- **Provenans:** `gpui-ec@14b359c55090da842a1b65be410347d3b0784d32`, karar kaydı **A-K24/0**.
- **Sınır:** Upstream, yayımlanmış bir external surface hakkında GPUI'nin kendi bildiği **iki
  olguyu** sormanın yolunu vermez. **Birincisi:** bir occurrence'ın başarılı bir tüketici draw
  komutuna bağlanıp bağlanmadığı. **İkincisi:** yayın geleceğe kapatıldıktan sonra artık hiçbir
  canlı GPUI sahnesince çözülemeyeceğini bildiren **monoton bırakma eşiği**. İkisi ayrı
  olgulardır ve biri diğerini vermez. `ExternalSurfaceHandle` kaynağı adlandırır, fakat bu iki
  olgunun hiçbirini bildiren gözlenebilir bir yüzey yoktur. `paint_external_surface` başarı
  döndürdüğünde bile bu, çizimin gerçekleştiğini değil, primitive'in kabul edildiğini söyler.
- **Elenen tüketici yolu:** Tüketici tarafında çözüm iki uca düşer, ikisi de yanlıştır. Kaynağı
  sabit bir kare sayısı sonra bırakmak **tahmindir**: kırpılma, device loss veya elenen sahne
  yüzünden kare hiç çizilmemiş olabilir; erken bırakma kullanım-sonrası-bırakmadır. Hiç
  bırakmamak ise bütçeyi sızdırır ve A-K21 atlasının tavanını anlamsızlaştırır. Tüketicinin kendi
  fence'ini koyması da yukarıdaki iki olgunun yerini tutmaz: hangi occurrence'ın draw komutu
  aldığını ve yayının artık çözülemez olduğunu yalnız GPUI bilir, tüketici bunları dışarıdan
  türetemez. Fiziksel completion bu alt adımın kapsamı değildir;
  sahipliği ve ölçümü mevcut backend/kanıt sözleşmelerinde kalır.
- **Kazanç/kabul hedefi:** Hedef, tüketicinin `Sınır`daki iki olguyu **tahmin etmeden**
  okuyabilmesidir; bırakma kararı tüketici politikasında kalır; fiziksel completion bu kabul
  hedefinin parçası değildir. Kabul, aşağıdaki dondurulmuş nöbet kümesinin **iddiaları
  gevşetilmeden** yeşile dönmesiydi ve **karşılandı**: üç hostta 205 yolun tamamı `ok`, on dört
  koşumun tamamı çıkış kodu 0. Hiçbir performans kazancı iddia edilmemektedir; adım doğruluk
  adımıdır ve **sıfır maliyet şartına** tabidir.
- **Dosyalar (uygulanacak kapsam):** `crates/gpui/src/external_surface.rs` (yayın tipleri,
  `SceneHandover`, `RetireWatermark`, contract 1.1); `crates/gpui/src/platform.rs` (üç additive
  `#[doc(hidden)]` seam, hepsi varsayılan gövdeli); `crates/gpui/src/window.rs`
  (`paint_external_surface_tracked`, mevcut `paint_external_surface`'in mutasyonsuz seam bağı,
  sahne devri çağrısı). Registry sahipleri: `crates/gpui_apple/src/external_registry.rs`,
  `crates/gpui_windows/src/external_registry.rs`,
  `crates/gpui_wgpu/src/external_registry.rs`. Çizim komutu noktaları:
  `crates/gpui_apple/src/metal_renderer.rs`, `crates/gpui_windows/src/directx_renderer.rs`,
  `crates/gpui_wgpu/src/wgpu_renderer.rs`. Seam'i override eden **beş** pencere:
  `crates/gpui_macos/src/window.rs`, `crates/gpui_windows/src/window.rs`,
  `crates/gpui_web/src/window.rs`, `crates/gpui_linux/src/linux/wayland/window.rs`,
  `crates/gpui_linux/src/linux/x11/window.rs`. Varsayılanı devralan ve **değişmeyen** iki
  implementor: `crates/gpui_linux/src/linux/headless/window.rs`,
  `crates/gpui/src/platform/test/window.rs`. Mantık eklenmeyen erişim seam'leri:
  `crates/gpui_macos/src/external_registry.rs`, `crates/gpui_web/src/gpui_web.rs`, linux
  x11/wayland producer yeniden dışa vurumları. `crates/gpui/src/scene.rs` **değişmez**: ayrık
  canlı handle kümesi zaten public olan `Scene::surfaces` alanından çıkarılır.
- **Kurallar:**
  - **Public yüzey tam olarak dört metottur:** `Window::paint_external_surface_tracked` ve
    `ExternalSurfaceProducer`'ın `close`, `binding_proof`, `retire_safety` metotları.
  - **Üç `#[doc(hidden)]` seam** `PlatformWindow` üzerindedir, hepsi varsayılan gövdelidir:
    `publish_external_tracked` (varsayılan
    `Err(TrackedPublishError::Surface(ExternalSurfaceError::UnsupportedCapability))`),
    `handover_external_scene` (varsayılan `SceneReplaceOutcome::Unsupported`) ve **mutasyonsuz**
    `external_publication_admission` (varsayılan `PublicationAdmission::Untracked`).
    `TrackedPublishError`'a ayrı bir `Unsupported` varyantı **eklenmez**.
  - **Durum ve hata türleri:** `PublicationId` opaktır (`Ord` yok, ham serial yok).
    `TrackedPublishError = Surface(ExternalSurfaceError) | CounterExhausted { counter } |
    AlreadyPublishedUntracked | ClosedPublication`.
    `PublicationAdmission = Untracked | Tracked(PublicationId) | Closed`.
    `BindingProof = Bound | Pending | Superseded | Unknown | StaleGeneration | Unsupported`.
    `WatermarkCoverage = Covered | NotYet | ForeignScope | StaleGeneration`.
    `RetireSafety = Through(RetireWatermark) | NoneYet | StaleProducer |
    CounterExhausted { counter } | Unsupported`.
    `SceneReplaceOutcome = Replaced | NoOp | CounterExhausted | Unsupported`.
    `CloseOutcome = Closed | AlreadyClosed | Unknown`. Hepsi `#[non_exhaustive]`'dir.
  - **Kimlik registry'dedir:** tam `ExternalSurfaceHandle { id, generation }` bağıdır; public
    descriptor **değişmez**. Boya sayısı publication sayısı değildir: ilk başarılı tracked boya
    serial basar, sonraki boyalar ve `Scene::replay` klonları **aynı `PublicationId`'yi** taşır.
  - **Tek registry sahipliği:** `SceneGeneration`, sticky tükenme, liveness ve watermark'ın tek
    sahibi **pencere-başı platform registry**'dir; core bunların hiçbirini tutmaz. Core'un
    üretilemez `SceneHandover` token'ı (alanları private, kurucusu yalnız core'a açık) **yalnız
    ayrık canlı handle kümesini** taşır.
  - **Atomik sahne devri:** checked nesil artışı, canlı kümenin yerleşmesi ve düşen kümenin
    terminal değerlendirmesi registry'de **tek işlemdir**; yeni küme yerleşmeden eski nesil
    düşürülmez. Gerçek no-op **yalnız** eski ve yeni kümenin ikisi de boşken; eski küme doluyken
    yeni kümenin boş olması değerlendirmeyi **mutlaka** çalıştırır.
  - **İki fazlı boya:** bütün fallible denetimler (descriptor/placement, kapalı durum, untracked
    geçmiş, publication sayacı taşması, sticky nesil tükenmesi) **önce** biter. Publication bağı
    ile primitive ekleme aynı pencere iş parçacığında **senkron ve yeniden girişsiz** kuyruktadır;
    araya başka registry ya da device-generation geçişi giremez. Hata hâlinde serial basılmaz,
    bağ kurulmaz, primitive eklenmez. Kuyruktan sonra gözlenen device loss handle ile
    `PublicationId`'yi **birlikte** stale yapar; `Bound` veya başka nesilden watermark kanıtı
    üretmez.
  - **Untracked geçmiş iki yerde denetlenir:** core **building scene**'i, registry **canlı
    scene**'i denetler; biri handle'ı untracked görmüşse tracked boya reddedilir ve **yeni handle**
    gerekir.
  - **`Bound` yalnız başarılı tüketici draw komutu kaydında doğar**; `resolve` kanıt değildir.
    Kısmen kırpılmış ama çizilmiş occurrence `Bound` yapar, tamamen kırpılan yapmaz. `Bound`
    geriye dönmez.
  - **Terminal kural tektir:** yalnız geleceğe **kapalı**, **hiç bağlanmamış** ve **canlı
    occurrence'ı kalmamış** yayın `Superseded` olur. **Açık** yayın sahneden düşse de `Pending`
    kalır ve watermark'ı bloke eder.
  - **Kapatma** atomik, idempotent ve yeniden açılamazdır; taze boyayı engeller, mevcut canlı
    sahne ve replay devamlarını **öldürmez**. Kapalı handle'a taze boya mevcut API'de
    `ExternalSurfaceError::InvalidGroup` üretir.
  - **Watermark sorgusu bool değildir:** `RetireWatermark::coverage` dört durumu ayırır; host'a
    `Ord`, ham serial veya kapsamı belirsiz `false` verilmez.
  - **Registry mutasyonları backend-private kalır:** `note_drawn` `PlatformWindow`'a konmaz ve
    `ExternalSurfaceProducer` ile `gpui-ec` tarafından çağrılamaz.
  - **GPUI'ye `Backpressure`, `AdapterSurfaceSlots` veya host bütçe-politikası tipi eklenmez.**
    Canlı registry snapshot'ı bu adımın **kapsamı dışındadır** ve `Unsupported` kalır.
  - **Sorgunun sınırı (normatif):** `BindingProof` **gösterim devrini** yönetir;
    `RetireWatermark` host/registry tarafındaki **çözümleme ve bırakma güvenliğini** yönetir.
    **İkisi de fiziksel GPU completion veya present garantisi değildir.** Encode veya submit
    edilmiş GPU işinin kaynak ömrü bu sorgunun **kapsamı dışındadır** ve bu API ondan completion
    sonucu **çıkarmaz**.
  - **Sıfır maliyet şartı:** capability `false` iken normal GPUI yolu **ek allocation, branch,
    pass veya sync maliyeti ödemez**; seam mutasyonsuzdur ve varsayılan gövde sabit döner.
- **Kabul kümesi (dondurulmuş):** *Matris* — aynı handle için dört hücrede de aynı
  `PublicationId` korunur, serial yalnız ilk başarılı tracked boyada basılır: **M1** tek boya /
  taze sahne · **M2** çoklu boya / taze sahne · **M3** tek boya / replay · **M4** çoklu boya /
  replay. *Nöbetler* — **N1** kapalı `Pending` + sahnede yok → `Superseded`; **N2** açık `Pending`
  + sahnede yok → hâlâ `Pending`, watermark bloke, sonraki sahnede aynı kimlikle dönmesi kabul;
  **N3** untracked geçmiş sonrası tracked boya reddi (core building ve registry canlı, iki yön
  ayrı); **N4** açık tracked handle'ın eski metottan boyasının aynı publication'a dahil olması;
  **N5** kapatma sonrası taze boya reddi ve mevcut replay'in yaşaması; **N6** terminal
  `Superseded` kimliğin replay'e dönememesi; **N7** (§terminal tanımından türer): geleceğe
  **kapalı**, hiç `Bound` olmamış ve canlı occurrence'ı kalmamış bir yayın renderer'a ulaşmadan
  elenirse `Superseded` olur; **açık** yayın ise **N2** gereği `Pending` kalır; **N8** nesil
  tükenmesinde fail-closed; **N9** eski küme dolu ve yeni küme boşken no-op **değil**; **N10**
  `u64::MAX-1` publication sayacı; **N11** `n4a` adopted-ref tepesi ≤ 2; **N12** `n4b`
  `BudgetExceeded{InFlightSurfaces}`; **N13** bağlanma ile
  bırakma bağımsızlığı, iki yönlü; **N14** stale generation **ve kuyruk sınırı**: kuyruktan sonra
  gözlenen device loss handle ile `PublicationId`'yi birlikte stale yapar, `Bound` üretmez, başka
  nesilden watermark kanıtı üretmez; **N15** resolve edilmiş ama draw edilmemiş occurrence `Bound`
  **değildir**; **N16** kısmen kırpılmış ama başarılı draw komutu almış occurrence `Bound`'dur
  (pozitif sınır); **N17** yayın öncesi birleştirilen istek serial tüketmez; **N18** tüketicisiz
  pencerede eşik ilerlemez; **N19** özel grup yokken sıfır maliyet; **N20** fallible faz ile
  kuyruk sınırı: hatada serial, bağ ve primitive doğmaz; **N21** `coverage` dört durumu ayırır,
  `ForeignScope` ile `NotYet` karışmaz; **N22** sticky tükenme sonrası yeni eşik üretilmez ve
  mevcut yayınlar erken bırakılmaz; **N23** `note_drawn` producer veya `gpui-ec` tarafından
  çağrılamaz (derleme nöbeti); **N24** `SceneHandover` core dışında kurulamaz (derleme nöbeti).
- **Bırakma koşulu:** Kayıtlı Zed revizyonu aynı gözlenebilir güvenceleri veren yayın kimliği,
  bağlanma kanıtı, terminal durum ve bırakma eşiği yüzeyini sağladığında madde silinir; `gpui-ec`
  upstream yüzeye adapter olur.
- **Geri alma koşulları:** Sıfır maliyet nöbeti (**N19**) düşerse; watermark'ın monotonluğu veya
  atlamazlığı çürütülürse; ya da dondurulmuş kabul kümesinden (**M1–M4**, **N1–N24**) herhangi
  biri **iddiası gevşetilmeden** yeşile dönmüyorsa madde geri alınır.
- **Upstream durumu:** Gönderilmedi. `../zed` bu çalışma için salt okunur kaynak deposudur;
  upstream'e değişiklik gönderme yetkisi verilmemiştir.
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
