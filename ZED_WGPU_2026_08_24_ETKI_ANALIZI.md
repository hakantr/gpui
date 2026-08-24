# Zed ve wgpu güncellemesi — GPUI etki analizi

## 1. Hüküm

Bu incelemenin sonucu üç parçalıdır:

1. `../zed` ve `../wgpu` güvenli biçimde yalnız fast-forward ile güncellenmiştir.
2. Güncel `wgpu`, mevcut standalone GPUI tarafından kaynak değişikliği olmadan tüketilebilmektedir.
   Doğrudan `wgpu-core` kullananlar için kırıcı olan queue hata-yönlendirme değişikliği GPUI'nin
   kullandığı public `wgpu` yüzünü kırmamaktadır.
3. Zed'den gelen GPUI değişikliklerinin alınması önerilir. En değerli kazanım Wayland'in
   talep-güdümlü kare döngüsüdür; X11'de üç ayrı doğruluk/donma düzeltmesi, foreground executor
   benchmark muhasebesi ve Windows Restart Manager desteği de alınmalıdır. Yeni bir yerel runtime
   sapması bu senkron commit'ine karıştırılmamalıdır.

Güncel Zed hâlâ dört kayıtlı yerel sapmanın hiçbirini kapsamamaktadır: standalone `gpui 0.3.0`
kimliği, zengin şekillendirilmiş satır geometrisi, bounded external-surface köprüsü ve sibling
`wgpu` checkout seçimi korunmalıdır.

## 2. İnceleme künyesi

| Alan | Değer |
|---|---|
| İnceleme zamanı | 2026-08-24T12:40:48+03:00 |
| Zed deposu | `/Users/hakanbiris/github/zed` |
| Zed önceki revizyon | `cef06d351bec10d0fb6176018ce8624e97baeb40` |
| Zed güncel revizyon | `1b86941cf7298912af31b56f16990cf65b3ecbd3` |
| Zed aralığı | 52 commit, 254 dosya, +9.544 / -3.295 |
| wgpu deposu | `/Users/hakanbiris/github/wgpu` |
| wgpu önceki revizyon | `bbac60da54794f532c890fe985c92616cfc5f2fd` |
| wgpu güncel revizyon | `d4359d74946b9908c58eab9e70db061b2b8c8343` |
| wgpu aralığı | 9 commit, 22 dosya, +387 / -105 |
| GPUI deposu | `/Users/hakanbiris/github/gpui` |
| GPUI mevcut revizyon | `cbf6da2afc16673efda63a5420df029bd65f4f4e` |
| GPUI kayıtlı Zed kaynağı | `cef06d351bec10d0fb6176018ce8624e97baeb40` |
| İncelenen extraction farkı | 28 dosya, +1.138 / -897 |

Zed ve wgpu çalışma ağaçları güncelleme sonunda temiz ve kendi uzak dallarıyla eşittir. GPUI
kaynak kodu bu incelemede senkronlanmamıştır; yalnız bu analiz ve eşlik eden uygulama planı
eklenmiştir.

## 3. Zed'den gelen GPUI değişiklikleri

### 3.1 Özet ve öncelik

| Commit | Sınıf | Etki | GPUI kararı |
|---|---|---|---|
| `eb354c8d50` | Mimari + davranış + düşük seviye API kırılması | Wayland render döngüsünü sürekli/heartbeat benzeri ilerlemeden talep-güdümlü duruma geçirir; park edilmiş pencereyi dirty state, pending present veya next-frame callback uyandırır | Zorunlu parite senkronu |
| `4d1935b8d0` | X11 davranış düzeltmesi | Pencere etkinleşince ICCCM urgency hint'ini temizler; diğer WM_HINTS alanlarını korur | Al |
| `f4178619ac` | X11 olay döngüsü düzeltmesi | Foreground runnable sonrasında x11rb'nin içeride buffer'ladığı olayları boşaltır; boş yeni pencere vakasını kapatır | Al |
| `d9ad6aff67` | X11 reentrancy düzeltmesi | WM_DELETE_WINDOW callback'inden önce client state borrow'unu bırakır; çift borrow/panik sınıfını kapatır | Al |
| `7316cf7745` | Yeni ölçüm yeteneği | `BenchReport::foreground_work()` ile task poll/action/input dispatch CPU süresini, pencere çizilmese bile raporlar | Al ve yetenek manifestine işle |
| `7f2a2c3c3e` | Platform davranışı + trait kırılması | Windows Restart Manager kapanışını destekler; shutdown'ın senkron tamamlanıp tamamlanmadığını callback sonucu ile bildirir | Al |
| `282f47a544` | Bağımlılık/mühendislik | cargo-machete yerine cargo-shear; çok sayıda kullanılmayan platform bağımlılığını kaldırır | Standalone kapanışa göre yeniden türet |
| `fe9556a11e` | Web tracing | Zed'in `ztracing` olaylarını browser Performance API'lerine bağlar | Extraction dışında; bugün alma |

### 3.2 Talep-güdümlü Wayland kare döngüsü

`eb354c8d50` bu aralıktaki en önemli mimari değişikliktir.

Eski modelde `PlatformWindow::completed_frame()` Wayland'in bir sonraki compositor callback'ini
ilerletmek için kullanılıyordu. Yeni modelde bu yüzey `schedule_frame()` olur ve kare talebi
aşağıdaki gerçek nedenlerden biri varsa oluşturulur:

- pencere invalidator'ı dirty ise;
- render edilmiş ama henüz sunulmamış bir kare varsa;
- `on_next_frame` callback'i bekliyorsa;
- throttling veya başarısız present nedeniyle kontrollü retry gerekiyorsa.

Wayland tarafı açık bir durum makinesi kullanır:

- `Unconfigured`
- `Ticking`
- `RescheduleRequested`
- `PresentationFailed`
- `AwaitingCallback`
- `Scheduled`
- `RetryScheduled`
- `Parked`

Park edilmiş döngü `calloop::ping::Ping` ile uyandırılır. Retry yalnız throttling veya present
başarısızlığı için yaklaşık 16,667 ms sonra alınır; idle pencere için sürekli timer çalışmaz.
İlk present öncesi hata ile en az bir present sonrasındaki hata ayrı tutulur. Sunulmuş yüzeyde
compositor callback'i pacing sağlar; ilk present öncesi veya throttling yüzünden draw'a ulaşmayan
durumda bounded timer kullanılır.

Bu değişikliğin gözlenebilir sonuçları:

- idle Wayland penceresi boş yere sürekli uyanmaz;
- fullscreen/yeniden boyutlandırma sonrası donma ve kaybolan forced render sınıfları kapanır;
- `AsyncApp::refresh`, effect kuyruğunu gerçekten flush edip park edilmiş platform döngüsünü
  uyandırır;
- kare içinde eklenen next-frame callback takip karesini kaybetmez;
- web tarafındaki boş `completed_frame` implementasyonu kalkar.

GPUI external-surface köprüsü açısından kritik sonuç şudur: gelecek retire/present korelasyonu
“her zaman bir sonraki tick gelir” varsayamaz. Özel grup iş ürettiğinde açıkça frame demand
oluşturmalı; özel grup yokken normal GPUI yolu timer, registry sorgusu veya sync maliyeti
ödememelidir.

### 3.3 X11 düzeltmeleri

Üç commit birbirinden bağımsız hata sınıflarını kapatır:

- `4d1935b8d0`: Urgency flag'i ayrı bir yardımcıyla read-modify-write yapar. Pencere
  inactive→active olduğunda flag temizlenir. Flag zaten kapalıysa property yazılmaz ve gereksiz X
  roundtrip oluşmaz.
- `f4178619ac`: Foreground runnable çalıştıktan sonra x11rb'nin socket dışında tuttuğu buffered
  olaylar işlenir. Ayrıca `set_input_focus` zorlaması kaldırılır; focus politikası window
  manager'a bırakılır.
- `d9ad6aff67`: WM_DELETE_WINDOW dalı, callback'e yeniden girmeden önce client borrow'unu bırakır.
  Böylece close callback içindeki platform çağrıları `RefCell` double-borrow paniği üretmez.

Bunlar unit/cross-compile ile runtime kanıtı sayılmaz. Senkron kapanışında gerçek X11 oturumunda
urgency temizleme, foreground iş ardından açılan pencerenin map edilmesi ve titlebar close yolu
ayrı ayrı çalıştırılmalıdır.

### 3.4 Foreground executor benchmark muhasebesi

`7316cf7745` public `ForegroundWorkSummary` tipini ve
`BenchReport::foreground_work() -> Option<ForegroundWorkSummary>` yüzünü ekler.

Raporlananlar:

- örnek sayısı;
- tam toplam süre;
- maksimum;
- p50, p90, p95, p99;
- toplam frame-budget taşımı;
- en uzun tek işin frame-budget taşımı.

Kapsanan işler task poll, action handler, input dispatch ve küçük poll kümeleridir. Draw ve present
bilinçli olarak dışarıda tutulur; bunlar mevcut frame raporunda ayrı kalır. Collector ölçüm setup'ı
tamamlandıktan sonra açıldığı için setup süresi ölçüme sızmaz. Pencere çizilmese bile uzun foreground
task görünür.

Bu yetenek GPUI ve gpui-ec performans raporlarında CPU foreground attribution açığını azaltır.
Ancak GPU completion, queue submit tamamlanması, external-surface lifetime güvenliği veya platform
present completion kanıtı değildir. Bu isimlerden biriyle yeniden etiketlenmemelidir.

### 3.5 Windows Restart Manager

`7f2a2c3c3e` iki düşük seviye yüzeyi değiştirir:

- `Platform::on_quit` callback'i `FnMut()` yerine `FnMut() -> bool` olur;
- Windows `WM_QUERYENDSESSION` / `WM_ENDSESSION` olaylarını işler.

`true` senkron shutdown'ın tamamlandığını, `false` ise `AppCell` o anda borrow edildiği için
shutdown'ın senkron yapılamadığını anlatır. Windows ikinci durumda log'u flush eder ve best-effort
`WM_QUIT` gönderir. Custom `Platform` implementor'ları için kaynak kırılmasıdır; normal GPUI
tüketicisinin public UI kodunu değiştirmez.

Gerçek kapanış kanıtı Windows oturum kapatma/yeniden başlatma akışında alınmalıdır. macOS'ta
cross-compile veya unit test, Restart Manager davranışını kanıtlamaz.

### 3.6 Bağımlılık temizliği

`282f47a544` Zed workspace'inde cargo-machete'yi cargo-shear ile değiştirir ve GPUI closure'da
kanıtlanan kullanılmayan bağımlılıkları kaldırır. Başlıca değişiklikler:

- `gpui` macOS bölümünden eski Cocoa/Core*/Metal/pathfinder ve build-time cbindgen bağımlılıkları;
- `gpui_linux` içinden image, itertools, pathfinder_geometry, pollster, profiling ve swash;
- `gpui_wgpu` içinden native pollster ile wasm-bindgen/wasm-bindgen-futures/js-sys;
- `media` içinden ctor;
- ilgili crate'lere cargo-shear ignore metadata'sı.

Bu liste standalone depoya körlemesine uygulanamaz. Zed'in kök dependency closure'ı ile standalone
closure aynı değildir; ayrıca yerel rich-text ve external-surface sapmaları ek bağımlılık
kullanabilir. Örneğin kök workspace'teki Apple/Metal bağımlılıklarının bir kısmı `gpui_apple` veya
`gpui_macos` tarafından hâlâ kullanılır. Doğru işlem:

1. crate düzeyindeki upstream kaldırmaları al;
2. `cargo metadata --locked` ile standalone kapanışı üret;
3. cargo-shear sonucunu kayıtlı sapmalarla birlikte değerlendir;
4. yalnız gerçekten tüketilmeyen kök workspace dependency'lerini kaldır;
5. lock dosyasını en son yeniden üret.

### 3.7 ztracing web eşlemesi neden alınmıyor?

`fe9556a11e` Zed'in kendi `ztracing` crate'ini browser Performance API'lerine bağlar. Standalone
extraction, bağımlılık kapanışı ve uyumluluk nedeniyle Zed'in `ztracing` entegrasyonunu taşımıyor;
normal `tracing` yolunu kullanıyor. Bu commit GPUI kaynak API'si değil, Zed'e özgü tracing
altyapısıdır.

Tüketici kazancı ve bağımlılık maliyeti ölçülmeden ztracing'i standalone closure'a geri sokmak
upstream paritesi değil yeni extraction politikası olur. Bu nedenle bugün karar “alma” değil
“ayrı ölçüm ihtiyacı doğarsa yeniden değerlendir”dir.

## 4. Zed uygulamasındaki extraction dışı değişiklikler

Aşağıdaki işler güncel Zed'in yeteneklerini geliştirir fakat standalone GPUI kaynak kapanışına
girmez. GPUI senkronuna taşınmaları yanlış olur.

| Alan | Commitler | Gelen yetenek/düzeltme |
|---|---|---|
| AI sağlayıcıları ve agent UI | `c3b365d276`, `1e9f1ef4d1`, `5255bd7f27`, `54230ad8fd`, `53dbfe4073`, `f36aec822b`, `1b86941cf7` | Tool öncesi reasoning gösterimi, Baseten provider, ek Anthropic hata eşlemeleri, OpenAI subscription autocomplete, transport host bilgisi, ask_user varsayılanının kapanması, Gemini 3.5 Flash-Lite |
| Editor/LSP/language | `dbdcb310d1`, `3ea4d186a1`, `deb194b49b`, `b0e37a6c18`, `cb1352a29d`, `b427d4ecf0`, `5b70f793d3`, `ab208db8d2`, `10b2925e7c`, `907ed09c9f`, `a7e23df67b`, `6805d952f9`, `d6449a9e3f` | LSP sonuç konumu/case dedup, range-format dedup, status path, inline completion debounce, OTP autofill kapatma, Duration tip güvenliği, bracket test düzeltmesi, auto-indent belgeleri, readonly autosave koruması, Markdown fallback highlighting, Verilog LSP seçimi, hover truncation |
| Workspace/project/navigation/terminal | `58006060d1`, `09adbb01f6`, `53b39e8e89`, `debf6b218c`, `107ee1a60a`, `93f07f6d17` | Rename undo temizliği, navigation history persistence, global gitignore doğruluğu, Flatpak CLI argüman sırası, terminal restore sırası, modal açıkken focus koruması |
| Git ve extension ekosistemi | `075520b968`, `4c763e1563`, `51a3ac29de`, `1ea16c1ab9`, `fd82517a11`, `7eec89207c`, `6bf539cd52` | Git graph arama odağı, context-menu flicker, extension manifest validation, diff base toggle, Tangled provider, eski extension API ayar uyumu, recursive blame toast temizliği |
| UI/ayar/inceleme | `f5e87e5343`, `91bf967e27`, `ec18126b1d` | Settings arama temizleme, inspector flag, Mermaid renderer güncellemesi |
| Paketleme, dokümantasyon ve repo bakımı | `1b04e4caf0`, `2b37a3ed5e`, `84aaa52595`, `ef50ad95b5`, `9b5b58607b`, `bcf033f8a0`, `35aab214c2` | Linux arşivinde GLib paketlememe, link/doküman/workflow temizliği, draft PR cleanup, contribution rule, uninstall ve Tailwind belgeleri |

Bu bölümün kararı “özellikler değersiz” değildir; yalnız ownership sınırıdır. Bunlar Zed
uygulamasına aittir, standalone GPUI senkronuna değil.

## 5. wgpu güncellemesi

### 5.1 Queue hata-yönlendirme

`8c7317b1f` ve `98aa89efa`:

- core `Queue::write_buffer` ve `Queue::write_texture` artık `Result` döndürmez;
- core `Queue::submit` doğrudan `SubmissionIndex` döndürür;
- gerçek `Result` üreten mantık iç `*_inner` fonksiyonlarında kalır;
- hata `Device::handle_error` üzerinden error scope veya uncaptured error handler'a gider;
- hata bağlamı queue label'ını ve çağrı adını taşır.

Doğrudan `wgpu-core` API'si kullanan kod için kaynak kırılmasıdır. GPUI doğrudan
`wgpu-core`/`core-remote` kullanmaz; public `wgpu::Queue` yüzünü kullanır. Public yüz zaten
senkron `Result` döndürmediği için callsite değişikliği gerekmedi.

GPUI'nin `WgpuRenderer`'ı `device.on_uncaptured_error` ile son hatayı saklıyor. Yeni queue
validation hataları bu kanala düşer ve sonraki frame başında mevcut failure sayacına girer. Hata
artık senkron dönüş değeriyle eşlenemez; herhangi bir gelecek GPUI hata modeli error-scope/handler
semantiğini korumalıdır.

### 5.2 core-remote yüzeyi

`4bc035d6f`, `87efa40fd`, `5d1d6f5eb` ve `d99c241a3`:

- spec dışı remote queue yüzlerini kaldırır;
- remote queue için submission/work-done alias'larını ekler;
- `CommandEncoderCommand` ve `RenderBundleEncoderCommand` mesajlarını ve server işleyicilerini
  ekler.

Bu, serialize edilmiş/uzak WebGPU protokolü uygulayanlar için yeni yetenektir. GPUI bu crate'lere
bağlı değildir. Yalnız “yeni özellik var” diye dependency closure'a eklenmemelidir.

### 5.3 Metal composite alpha fail-fast

`48d3a109b` Metal HAL'de tanınmayan composite alpha mode'u sessizce yoksaymak yerine
`unreachable!` ile durdurur. Metal HAL'in kabul ettiği iki durum `Opaque` ve `PreMultiplied`'dır;
`Inherit` gibi seçimlerin core katmanında çözülmüş olması beklenir.

GPUI:

- saydam pencere için önce `PreMultiplied`, sonra `Inherit`;
- opak pencere için önce `Opaque`, sonra `Inherit`;
- hiçbiri yoksa capabilities listesinin ilk elemanını

seçiyor. Güncel Apple Metal runtime testleri yeni wgpu ile geçti; bugünkü adapter capability yolu
desteklenmeyen mode'u HAL'e indirmiyor. Üretim seçim mantığını sırf ihtimal için yerel olarak
değiştirmek bugün gerekli değildir. Senkron sonrası focused test/capability assertion, bu upstream
varsayımının regression'ını erken yakalamalıdır. Gerçek bir unsupported capability görülürse
fail-closed davranış kararı ayrıca alınır.

### 5.4 Diğer wgpu değişiklikleri

| Commit | Değişiklik | GPUI etkisi |
|---|---|---|
| `6d1bb1959` | Passthrough testlerinde DXC eksikliği için açık hata | GPUI runtime etkisi yok |
| `d4359d749` | Standalone örnekleri Rust edition 2024'e geçirir | Kütüphane/API etkisi yok; GPUI zaten edition 2024 |

Bu dokuz commit için henüz yeni bir wgpu `CHANGELOG.md` girdisi yoktur; hüküm commit ve kaynak
diff'lerinden türetilmiştir.

## 6. Mevcut GPUI ile doğrulanan tüketici etkisi

Güncel wgpu revizyonuna karşı, GPUI kaynak senkronu yapılmadan şu sonuçlar alındı:

| Kapı | Sonuç | Kanıt sınırı |
|---|---|---|
| `cargo check -p wgpu --all-features -p wgpu-core-remote -p wgpu-core-remote-types` | GEÇTİ | Güncel wgpu kaynak derlemesi |
| `cargo check -p gpui_wgpu --all-targets --locked` | GEÇTİ | Mevcut GPUI public wgpu tüketici uyumu |
| `cargo check --workspace --all-targets --locked` | GEÇTİ | Mevcut standalone GPUI host workspace'i, güncel sibling wgpu ile |
| `cargo test -p gpui_wgpu --lib --locked` | 75/75 GEÇTİ | macOS host; gerçek Metal external-surface draw testleri dahil |
| `./scripts/verify-sapmalar.sh` | GEÇTİ | 38 portable + 13 CoreText + 30 cosmic-text focused testi |
| `cargo metadata --locked --format-version 1` | GEÇTİ | Standalone dependency graph ve lock tutarlılığı |
| `cargo fmt -- --check` | GEÇTİ | GPUI-owned/default workspace biçim kapsamı |
| wasm multithreaded `gpui_wgpu + gpui_web --lib` | GEÇTİ | Cross-target compile; browser runtime değil |
| wasm `--no-default-features --lib` | GEÇTİ | Cross-target compile; browser runtime değil |
| wasm `--all-targets` | GEÇMEDİ | Üretim kodu değil; host-only Criterion benchmark wasm için derlendi ve Rayon'u reddetti |
| Windows `gpui_windows --all-targets` cross-check | TAMAMLANMADI | Kaynağa ulaşmadan macOS'ta `lib.exe` bulunamadı; Windows runtime kanıtı değildir |
| `cargo fmt --all -- --check` | GEÇMEDİ | Tek fark temiz upstream `../wgpu/deno_webgpu/surface.rs` import sırası; GPUI dosyasında fark yok |

Gözlenen upstream uyarıları:

- `wgpu-core-remote/Cargo.toml` içindeki iki `default-features` tanımı workspace dependency
  tanımı yüzünden bugün yok sayılıyor;
- `wgpu-core/src/lock/ranked.rs` içinde bir `expect(unused)` beklentisi karşılanmıyor;
- GPUI closure'daki eski `block 0.1.6` future-incompat uyarısı veriyor.

Bu uyarılar GPUI'nin yeni wgpu ile API kırılması değildir. Yine de final senkron raporunda görünür
kalmalı; upstream veya extraction ownership'i ayrı ayrı yazılmalıdır.

Biçim farkı da aynı sahiplik ayrımına tabidir. `surface.rs` hem GPUI'nin Rust 1.97 rustfmt'i hem
wgpu'nun Rust 1.93 rustfmt'i tarafından farklı bir import sırasına taşınmak isteniyor; dosya
`d4359d749...` revizyonundaki temiz upstream içeriğindedir. Güncel olma/temiz ağaç talebi
korunduğu için bu analiz sırasında yerel fmt-only fark yeniden üretilmedi. GPUI-owned biçim
kapsamını çalıştıran `cargo fmt -- --check` ise geçmektedir.

## 7. GPUI parite ve çatışma haritası

### 7.1 Kayıtlı sapmaların bırakma koşulu denetimi

Güncel Zed `1b86941cf7` içinde aşağıdaki yerel yüzeylerin karşılığı bulunmadı:

- `ExternalSurface*` ve `paint_external_surface`;
- `shape_rich_line` / `RichTextRun`;
- `ResolvedFontFace`;
- `CaretAffinity`;
- `LinePlacement`.

Zed'in GPUI package sürümü de `0.2.2` olarak kalıyor. Sonuç:

| Sapma | Güncel Zed kapsıyor mu? | Senkron kararı |
|---|---|---|
| Yerel `gpui 0.3.0` sürümü | Hayır | Koru |
| Zengin şekillendirilmiş satır geometrisi | Hayır | Koru ve focused kanıtı yeniden koş |
| Bounded external-surface köprüsü | Hayır | Koru ve altı profil kapısını yeniden aç |
| Sibling wgpu checkout seçimi | Hayır | Yeni `d4359d749...` revizyonuna güncelle |

### 7.2 Kuru yama sonucu

Zed eski→yeni aralığının extraction patch'i mevcut GPUI üzerine `git apply --check --verbose` ile
kuru uygulandı.

- Bütün kaynak kodu hunks'ları uygulanabildi.
- Çatışan alanlar: kök `Cargo.toml`, kök `Cargo.lock`, `crates/gpui/Cargo.toml` ve
  `crates/sum_tree/Cargo.toml`.
- Diğer platform manifestleri ve app/window/Wayland/X11/Windows kaynak hunks'ları offset ile veya
  doğrudan uygulanabildi.

Bu “kaynaklar güvenlidir” anlamına gelmez; yalnız metinsel birleşme kanıtıdır. Özellikle
`platform.rs`, `window.rs`, Wayland/X11 pencereleri ve Web window yerel external-surface seam'leri
taşıdığı için patch sonrası semantik audit ve divergence testleri zorunludur.

Manifest çatışmalarının doğru çözümü:

- Zed'e özgü kök workspace üyeleri standalone'a alınmaz;
- `gpui 0.3.0` korunur;
- local rich-text/external-surface dependency kullanımı `cargo metadata` ve kaynak taramasıyla
  yeniden doğrulanır;
- cargo-machete metadata'sı cargo-shear'a çevrilir;
- lock dosyası elle kopyalanmaz, standalone workspace'te `--locked` öncesi tek kez yeniden
  üretilir.

## 8. Kazanç, risk ve karar matrisi

| Öncelik | Kalem | Kazanç | Ana risk | Karar |
|---|---|---|---|---|
| P0 | Zed parite senkronu | Wayland idle/verim ve correctness, X11 correctness, Windows shutdown | Yerel sapmaların sessiz kaybı | Uygula |
| P0 | Sibling wgpu pin güncellemesi | Güncel hata semantiği ve Metal fail-fast ile uyum | Hata kanalı/provenance yanlış yorumlanabilir | Uygula, handler testleriyle |
| P1 | Foreground work raporu | Pencere dışı CPU stall attribution | GPU/present metriği sanılması | Uygula, semantik adı koru |
| P1 | Standalone dependency kapanış temizliği | Daha az eski/native bağımlılık ve build yükü | Zed closure'ını kör kopyalayıp yerel sapmayı kırmak | cargo-shear + metadata ile uygula |
| P1 | Wasm benchmark hedef izolasyonu | `--all-targets` kapısını yeniden anlamlı yapar | Runtime davranışına yanlışlıkla dokunmak | Test-only extraction düzeltmesi olarak ayrı commit |
| P2 | ztracing web entegrasyonu | Browser performance timeline potansiyeli | Yeni dependency/politika, ölçülmemiş tüketici kazancı | Ertele |
| P2 | core-remote dependency | Remote WebGPU protokolü | GPUI'nin ihtiyacı yok, closure büyür | Alma |
| Karar kapısı | Yeni retire/observer GPUI seam'i | gpui-ec resource/lifetime kanıtını host gerçeğine bağlayabilir | İkinci sapma yüzeyinin büyümesi, normal path maliyeti | Senkrondan sonra ayrı SAPMALAR kararı |

## 9. Sonuç

En doğru yol, önce Zed ve wgpu baseline'ını standalone GPUI'ye parite-disipliniyle almak; sonra
yalnız kanıtlanan extraction hijyenini ayrı commit'lerde kapatmaktır. gpui-ec için düşünülen yeni
retire/registry hook'ları bu senkrona eklenmemelidir. Talep-güdümlü Wayland modelinin ve güncel wgpu
hata semantiğinin üzerine tasarlanmalı, public diagnostics'in yetmediği ölçüldükten sonra
`SAPMALAR.md` ile ayrı yetkilendirilmelidir.
