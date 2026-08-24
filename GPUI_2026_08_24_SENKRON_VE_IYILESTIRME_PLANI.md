# GPUI — 2026-08-24 senkron ve iyileştirme planı

## 1. Durum ve amaç

Bu belge bir uygulama yetkisi değil, uygulanabilir ve denetlenebilir iş planıdır. Yazıldığı anda:

- `../zed` = `1b86941cf7298912af31b56f16990cf65b3ecbd3` ve temiz;
- `../wgpu` = `d4359d74946b9908c58eab9e70db061b2b8c8343` ve temiz;
- standalone `../gpui` kaynak baseline'ı = `cbf6da2afc16673efda63a5420df029bd65f4f4e`;
- GPUI'nin kayıtlı Zed kaynağı hâlâ `cef06d351bec10d0fb6176018ce8624e97baeb40`;
- GPUI kaynak senkronu yapılmamıştır.

Amaç, güncel Zed GPUI kaynaklarını ve güncel sibling wgpu gerçeğini standalone depoya almak;
kayıtlı dört sapmayı korumak; platform/runtime davranışını kanıtlamak ve ancak ondan sonra gpui-ec
için gerçekten gerekli yeni host seam'lerini ayrı karar kapısına getirmektir.

## 2. Kapsam sınırı

### Bu planda var

- Zed `cef06d...→1b86941...` GPUI extraction senkronu;
- wgpu `bbac60...→d4359d...` tüketici etkisinin GPUI'de kapatılması;
- Wayland talep-güdümlü frame loop, X11 düzeltmeleri, foreground work raporu ve Windows Restart
  Manager desteği;
- standalone manifest/lock ve cargo-shear kapanışı;
- kayıtlı sapmaların korunması ve bırakma koşulu denetimi;
- host, wasm, cross-target ve altı runtime profil doğrulama matrisi;
- ileride gerekebilecek bounded retire/observer seam'i için karar ve uygulama şablonu.

### Bu planda yok

- `../zed` içinde kaynak, dal veya commit üretmek;
- gpui-ec kaynaklarını değiştirmek;
- Zed uygulama özelliklerini standalone GPUI'ye kopyalamak;
- ztracing veya wgpu core-remote'u sırf upstream'de yeni olduğu için dependency closure'a eklemek;
- external-surface köprüsünü general shader, raw device, raw encoder veya keyfi render-pass API'sine
  çevirmek;
- cross-compile sonucunu Windows/Linux/browser runtime kanıtı saymak;
- yeni bir GPUI runtime/API sapmasını `SAPMALAR.md` kaydı ve sahip kararı olmadan uygulamak.

## 3. Dondurulmuş kararlar

1. **Önce parite baseline'ı.** Yeni GPUI seam'i, Zed senkron commit'ine karışmaz.
2. **Dört mevcut sapma korunur.** Güncel Zed bırakma koşullarını karşılamıyor.
3. **wgpu public yüzü kırılmadı.** Queue hata-yönlendirmesi nedeniyle üretim callsite rewrite
   yapılmaz; mevcut device error handler semantiği doğrulanır.
4. **Wayland demand-driven model yeni zemin olur.** Gelecek dış-yüzey lifetime işi sürekli tick
   varsaymaz.
5. **Bağımlılık temizliği kaynak kullanımıyla yeniden türetilir.** Zed kök manifesti standalone'a
   kör kopyalanmaz.
6. **Ölçüm adları korunur.** Foreground executor work, GPU completion veya present completion diye
   raporlanmaz.
7. **WIP=1.** Parite senkronu, extraction hijyeni, yeni divergence kararı ve backend uygulamaları
   ayrı iş paketleridir.

## 4. Bağımlılık sırası

| Sıra | Paket | Neden önce/sonra |
|---|---|---|
| 0 | Mevcut baseline kanıtı | Senkronun bozduğu şeyi eski çevresel arızadan ayırır |
| 1 | Zed + sibling wgpu parite senkronu | Bütün sonraki tasarım güncel gerçek üzerinde yapılır |
| 2 | Standalone manifest/test hijyeni | Parite kaynak kodundan ayrı, extraction-owned iştir |
| 3 | Platform ve runtime kanıtı | API/derleme başarısını davranış kanıtına dönüştürür |
| 4 | Yetenek/provenance dokümantasyonu | Yalnız doğrulanmış sonucu “destek” olarak bildirir |
| 5 | Yeni host seam'i karar kapısı | Public/upstream yolun yetmediği güncel baseline'da ölçülür |
| 6 | Yetkilendirilmiş additive GPUI işi | Karar kaydı koddan önce gelir |
| 7 | gpui-ec tüketici değişiklikleri | GPUI sağlayıcı sözleşmesi ve runtime kanıtından sonra konuşulur |

## 5. Aşama S0 — senkron öncesi baseline

### İş

1. Üç deponun revizyonunu, branch'ini ve temizliğini kaydet.
2. GPUI'nin kayıtlı dört sapmasını `scripts/verify-sapmalar.sh` ile doğrula.
3. Mevcut host kapılarını çalıştır:
   - `cargo fmt -- --check`
   - `cargo metadata --locked --format-version 1`
   - `cargo check --workspace --all-targets --locked`
   - `cargo test --workspace --locked -- --test-threads=1`
4. Güncel sibling wgpu ile mevcut GPUI'nin focused kontrol sonuçlarını baseline'a ekle.
5. Çevresel engelleri kaynak hatası gibi göstermeden kaydet.

### Bugün elde olan kanıt

- güncel wgpu ve core-remote paketleri derleniyor;
- mevcut `gpui_wgpu` yeni sibling wgpu ile derleniyor;
- mevcut standalone host workspace bütün hedefleriyle yeni sibling wgpu karşısında derleniyor;
- macOS hostta `gpui_wgpu` 75/75 testi geçiyor;
- kayıtlı rich-text sapma kapısı 38 portable + 13 CoreText + 30 cosmic-text testiyle geçiyor;
- locked metadata kapısı geçiyor;
- wasm üretim kitaplıkları multithreaded ve `--no-default-features` yollarında derleniyor;
- wasm `--all-targets` host-only Criterion/Rayon hedef kapsamı nedeniyle düşüyor;
- Windows cross-check `lib.exe` yokluğunda kaynağa ulaşmadan duruyor.
- full fmt kontrolü yalnız temiz upstream sibling
  `wgpu/deno_webgpu/surface.rs` import sırasını işaretliyor; GPUI dosyasında biçim farkı yok.

### Çıkış

Tarih/revizyon/komut/sonuç içeren, senkron sonrası karşılaştırmaya uygun baseline. Bu aşamada kod
değişmez.

## 6. Aşama S1 — atomik Zed/wgpu baseline senkronu

### 6.1 Kaynak aktarımı

Zed'den aşağıdaki kaynak grupları alınır:

| Grup | Dosyalar | Özel denetim |
|---|---|---|
| Core app/frame | `app.rs`, `app/async_context.rs`, `window.rs`, `platform.rs` | `completed_frame→schedule_frame`; local external-surface API korunur |
| Benchmark/profiler | `app/bench_context.rs`, `profiler/journal.rs` | Foreground setup dışlama ve draw/present ayrımı |
| Test platform | `platform/test/platform.rs`, `platform/test/window.rs`, `platform/visual_test.rs` | Yeni scheduled-frame nöbetleri gerçek talebi ölçmeli |
| Wayland | `linux/wayland/client.rs`, `linux/wayland/window.rs` | Durum makinesi, Ping, retry; external capability/producer seam'i korunur |
| X11 | `linux/x11/client.rs`, `linux/x11/window.rs` | Buffered-event, urgency ve close-reentrancy; external seam korunur |
| Platformlar | Linux/macOS/Web/Windows `platform.rs`, Web `window.rs`, Windows `events.rs` | Yeni `on_quit -> bool` imzası ve web no-op kaldırma |
| Manifests | gpui, linux, web, wgpu, windows, media, sum_tree, kök | Standalone kapanışa elle uyarlanır |

Kuru yama kaynak code hunks'larının uygulanabildiğini gösterdi. Buna rağmen şu dosyalar mutlaka
semantik review'dan geçer:

- `crates/gpui/src/platform.rs`:
  `PlatformWindow::schedule_frame` alınırken
  `external_surface_capabilities()` default'u ve bounded API korunur.
- `crates/gpui/src/window.rs`:
  yeni scheduling akışı alınırken `paint_external_surface`, special-group doğrulaması ve mevcut
  testleri korunur.
- `crates/gpui_linux/src/linux/wayland/window.rs`:
  `redraw_requested` ile renderer `needs_redraw` etkileşimi local external-surface çiziminde de
  takip karelerini kaybetmemeli.
- `crates/gpui_linux/src/linux/x11/window.rs` ve `crates/gpui_web/src/window.rs`:
  external capability/producer erişimi upstream değişiklik içinde kaybolmamalı.

Rich-text kaynak dosyalarına upstream aralığında doğrudan hunk gelmiyor; yine de manifest temizliği
onların backend bağımlılıklarını etkileyebileceği için focused testleri zorunludur.

### 6.2 Manifest çatışmalarının çözümü

Kuru uygulamada metinsel çatışma veren dört alan:

- kök `Cargo.toml`;
- kök `Cargo.lock`;
- `crates/gpui/Cargo.toml`;
- `crates/sum_tree/Cargo.toml`.

Çözüm kuralları:

1. Zed'e ait olmayan standalone workspace üye listesi korunur.
2. `gpui` package version `0.3.0` korunur.
3. Zed'in crate düzeyinde kaldırdığı unused dependency satırları aday olarak alınır.
4. Kök workspace dependency satırı, herhangi bir retained crate veya kayıtlı sapma kullanıyorsa
   korunur.
5. cargo-machete metadata'sı cargo-shear'a geçirilir; ignore listesi retained kaynaklara göre
   yeniden üretilir.
6. Local `gpui_wgpu` sibling path seçimi yeni `d4359d749...` pin gerçeğiyle korunur.
7. `Cargo.lock` Zed'den kopyalanmaz; standalone workspace'te bir kez yeniden üretilir, sonra bütün
   kapılar `--locked` çalışır.

Özellikle `core-video` local external-surface/AppKit yolunda, Apple/Metal/CoreText bağımlılıklarının
bir kısmı `gpui_apple`, `gpui_macos` veya rich-text sapmasında kullanılabilir. Kök satır yalnız
“Zed gpui crate'inden kalktı” diye silinmez.

### 6.3 Provenance'ın atomik güncellenmesi

Aynı senkron commit'inde:

- `UPSTREAM.md` → `1b86941cf7298912af31b56f16990cf65b3ecbd3`;
- `NOTICE` kaynak revizyonu/tarihi;
- `EXTRACTION.md` senkron kaydı, dependency uyarlamaları, doğrulama sınırı ve sibling wgpu
  gözlenen revizyon tablosu → `d4359d749...` (`SAPMALAR.md` kuralı gereği gözlenen revizyonun
  kayıt yeri burasıdır; `SAPMALAR.md` revizyon hash'i taşımaz);
- `scripts/check-upstream.sh` baseline'ı;
- `yetenek.md` provenance başlığı ve yeni doğrulanmış yetenekler;
- varsa eski baseline'a bağlı plan/provenance satırları

birlikte güncellenir. Kaynak yeni, provenance eski veya tersi bir commit üretilmez.

### 6.4 Bu commit'e girmeyecekler

- wasm Criterion hedef izolasyonu gibi önceden var olan extraction hijyeni;
- yeni alpha-mode production politikası;
- retire-safe/observer/registry getter API'si;
- gpui-ec değişikliği;
- ztracing ve zlog: aralıkta `crates/ztracing` ile `crates/zlog/src/filter.rs` değişiyor, fakat bu
  crate'ler extraction'a dahil değildir; `check-upstream.sh` bu yolları yalnız izleme amacıyla
  raporlar, hunks'ları alınmaz;
- unrelated formatting veya cleanup.

### Çıkış

Tek, atomik, yeşil “Zed 2026-08-24 + sibling wgpu baseline” commit'i. Kayıtlı dört sapmanın diff'i
yeniden sınıflandırılmış ve focused kanıtı alınmış olmalıdır.

## 7. Aşama S2 — standalone dependency ve hedef hijyeni

Bu aşama parite baseline'ından ayrı commit'tir.

### 7.1 cargo-shear kapanışı

1. Kullanılan cargo-shear sürümünü kaydet.
2. Workspace ve her retained crate için raporu al.
3. Her kaldırmayı “upstream de kaldırdı” ve “standalone kaynakta gerçekten kullanılmıyor”
   kanıtlarının ikisiyle eşle.
4. Kayıtlı sapma dosyalarının cfg-gated hedeflerini ayrıca tara:
   - macOS/CoreText/Metal;
   - Windows/DirectWrite/D3D11;
   - Linux Wayland/X11;
   - wasm WebGPU/WebGL2.
5. Lock dosyasını güncelle ve bütün `--locked` kapıları yeniden çalıştır.

### 7.2 wasm benchmark hedef izolasyonu

Bugünkü `--all-targets` düşüşü üretim kitaplığı hatası değildir: `gpui_wgpu` Criterion benchmark'ı
wasm hedefinde derlenmeye çalışır ve Criterion'un default Rayon yolu wasm'i reddeder.

Hedef:

- host benchmark'ı hostta aynen çalışır;
- wasm üretim crate'leri hem default hem `--no-default-features` altında derlenir;
- wasm `--all-targets` artık host-only bench'i yanlış kapsamda derlemez;
- üretim cfg'si veya runtime davranışı değişmez.

Çözüm, Cargo hedef tanımı/feature gating düzeyinde en dar test-only extraction uyarlaması olmalıdır.
Bu uyarlama `EXTRACTION.md` içinde açıkça kaydedilir; Zed runtime sapması diye
`SAPMALAR.md`'ye yazılmaz.

### 7.3 Uyarı sahipliği

- sibling wgpu `default-features` ve `expect(unused)` uyarıları upstream ownership'inde;
- standalone `block 0.1.6` future-incompat uyarısı dependency closure ownership'inde;
- cargo-shear sonrası `block` hâlâ gerçekten kullanılıyorsa zorla silinmez; replacement ancak
  upstream veya ayrı divergence kararıyla yapılır.

## 8. Aşama S3 — wgpu tüketici semantiği

### 8.1 Queue hataları

Üretim GPUI callsite'ları `wgpu::Queue` public API'sini kullanmaya devam eder. Aşağıdakiler
doğrulanır:

- `on_uncaptured_error` queue write/submit validation hatalarını gözlüyor;
- hata aynı frame'de senkron `Result` bekleniyormuş gibi eşlenmiyor;
- frame failure sayacı hatayı bir sonraki güvenli kontrol noktasında görüyor;
- queue label ve çağrı bağlamı log/evidence'da kaybolmuyor;
- error scope kullanılan adapter probe'u uncaptured handler ile yarışmıyor.

Bu doğrulama mümkünse test-only kontrollü validation hatasıyla yapılır. Üretim hata modeli
değişikliği gerekirse upstream karşılığı aranır; yoksa `SAPMALAR.md` kararı olmadan yapılmaz.

### 8.2 Metal alpha modu

Bugünkü gerçek Metal external-surface draw testleri yeni wgpu ile geçti. Ek karar:

- production picker şimdilik değiştirilmez;
- helper düzeyindeki tercih testi `PreMultiplied/Opaque` seçimini sabitler;
- gerçek Metal capability künyesi kaydedilir;
- capabilities gelecekte yalnız unsupported mode verirse sessiz fallback değil açık sahip kararı
  gerekir.

`PostMultiplied` veya başka bir mode sırf panic'i aşmak için seçilmez. Core katmanının
`Inherit` çözümünü HAL sözleşmesi diye GPUI içinde yeniden uygulamaya çalışma.

### 8.3 core-remote

GPUI'nin remote WebGPU protokol tüketicisi yoktur. Yeni
`wgpu-core-remote(-types)` yüzeyleri dependency closure'a girmez. Bu karar ancak gerçek consumer
ve ölçülmüş sınır doğarsa yeniden açılır.

## 9. Aşama S4 — davranış ve runtime kabulü

### 9.1 Host ve statik kapılar

Senkron commit'i aşağıdakilerin tamamını geçmeden kapanmaz:

~~~text
./scripts/check-upstream.sh ../zed 1b86941cf7298912af31b56f16990cf65b3ecbd3
./scripts/verify-sapmalar.sh
cargo fmt -- --check
cargo metadata --locked --format-version 1
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=1
~~~

Path dependency olan sibling wgpu'dan gelen upstream lint uyarısı `-D warnings` ile kapıyı
etkilerse ownership'i raporlanır; uyarı örtülmez veya GPUI kaynağına anlamsız allow eklenmez.
`cargo fmt --all -- --check` ayrıca dependency-kapsam tanısı olarak çalıştırılır. Yalnız temiz
upstream `wgpu/deno_webgpu/surface.rs` import sırasını işaretlediği sürece bu bilinen upstream
istisnası kaydedilir; herhangi bir GPUI dosyası görünürse kapı düşer. Sibling depoya sırf GPUI
toolchain'i için yerel fmt farkı bırakılmaz.

### 9.2 Focused divergence kapıları

- local package version/lock identity;
- portable rich-text geometri/caret testleri;
- macOS CoreText rich-text testleri;
- wgpu/cosmic-text rich-text testleri;
- Metal external registry + gerçek draw;
- wgpu external registry + gerçek draw;
- D3D11 external registry/draw cross-build ve gerçek Windows runtime;
- external group ordering, clip, crop, alpha ve stale-generation nöbetleri.

Test sayıları kaynak değişince yeniden sayılır. Eski “38/13/30” gibi sayılar koşul olarak
kopyalanmaz; yeni committe gerçek sayı ve filtre künyesi yazılır.

### 9.3 Cross-target kapılar

| Hedef | Kapı | Kanıt sınırı |
|---|---|---|
| wasm default | Zed atomics/build-std bayraklarıyla `gpui_wgpu + gpui_web` | Compile |
| wasm no-default | Aynı hedefte tek iş parçacıklı feature yolu | Compile |
| Windows MSVC | `gpui_windows` + DirectWrite + D3D11 | Cross compile; runtime değil |
| Linux | `gpui_linux` Wayland/X11 feature kombinasyonları | Cross/host compile; runtime ayrıca |
| macOS | gpui_apple/gpui_macos/gpui_wgpu | Host compile + seçili Metal runtime |

Windows cross-build için dar build-tool shim kullanılırsa shim komutu ve sınırı rapora girer.
“Derlendi” hiçbir zaman “D3D11 çalıştı” diye yazılmaz.

### 9.4 Gerçek platform matrisi

| Profil | Zorunlu davranış kanıtı |
|---|---|
| macOS Metal | GPUI pencere açılışı, saydam/opak alpha seçimi, external draw/crop/clip, idle cadence |
| Windows D3D11 | Restart Manager kapanışı, device-lost/registry generation, external draw, gerçek driver künyesi |
| Linux wgpu-Vulkan | Wayland demand-driven idle/fullscreen/retry ve external draw |
| Linux wgpu-GL | Aynı Wayland/X11 davranışları, GL backend ve external draw |
| Browser WebGPU | parked/next-frame uyanma, external surface, context loss; gerçek browser |
| Browser WebGL2 | aynı sınırlar, uniform-per-surface yolu ve context-order sync |

X11'e özel üç vaka ayrıca:

1. urgency aktif pencereyle temizlenir;
2. foreground runnable sonrası açılan pencere boş kalmaz/map event işlenir;
3. titlebar close callback'i reentrancy paniği üretmez.

Wayland için idle CPU/poll sayısı baseline ile karşılaştırılır. “Talep-güdümlü” iddiası yalnız
kaynak state machine'inden değil, park edilmiş pencerede gözlenen wakeup/cadence verisinden gelir.

Her sonuç GPUI, Zed, wgpu, Rust, OS, GPU, driver, backend, fixture ve timeline revizyonlarını
taşır. Ölçülemeyen alan boş bırakılmaz; nedeni ile `unsupported`/`ölçülemedi` yazılır.

## 10. Aşama S5 — yetenek ve kullanım dokümantasyonu

Runtime/statik sonuçlar geldikten sonra:

- `yetenek.md` provenance header'ı güncellenir;
- `ForegroundWorkSummary` yalnız foreground CPU işi olarak belgelenir;
- `schedule_frame` düşük seviye platform değişikliği custom implementor kırılmasıyla yazılır;
- Wayland demand-driven davranışının “destek” seviyesi gerçek Linux koşumuna göre
  `uygulandı/derlendi/kanıtlandı` diye ayrılır;
- Windows Restart Manager aynı şekilde compile ve runtime statülerini ayırır;
- wgpu queue hata semantiği device error scope/handler üzerinden tarif edilir;
- core-remote ve ztracing'in neden capability manifestine alınmadığı ownership notuyla korunur.

Eski `2026-08-12` veya `6ae523...` provenance satırları güncelmiş gibi bırakılmaz.

## 11. Aşama D0 — gpui-ec için olası yeni host seam'i karar kapısı

Bu aşama senkronun parçası değildir. Yalnız S0–S5 kapandıktan sonra açılır.

### 11.1 Önce tüketici yolu

Şu soru kaynak ve ölçümle cevaplanır:

> Güncel GPUI public diagnostics, demand-driven frame scheduling ve mevcut bounded
> external-surface producer yüzeyi; dış GPU işini bir GPUI frame'iyle eşlemek, adopted yüzeyleri
> güvenle retire etmek ve gerçek registry kaynağını ölçmek için yeterli mi?

Yeterliyse yeni GPUI API açılmaz. Yetersizse eksik her olgu ayrı yazılır:

- güvenli retire serial'ı;
- registry live count/byte/view/generation gerçeği;
- opaque adapter identity→slot eşlemesi;
- ABA/generation nöbeti;
- gerekirse frame/present korelasyonu.

Timer, occlusion, “N frame geçti”, CPU submit veya queue sırası GPU completion kanıtı sayılmaz.
Foreground benchmark özeti lifetime kanıtı değildir.

### 11.2 Yetki paketi

Koddan önce `SAPMALAR.md` taslağı hazırlanır ve şunları taşır:

1. gerçek consumer sınırı;
2. public/consumer alternatifleri ve neden yetmedikleri;
3. gözlenebilir kazanç veya kapanan correctness riski;
4. public imza ve semantik;
5. hata/unsupported/fail-closed davranışı;
6. lifetime, thread ve drop modeli;
7. dokunulacak platform crate'leri;
8. bırakma koşulu ve upstream karşılığı;
9. normal GPUI yolunda sıfır allocation/query/sync maliyeti;
10. gpui-ec karar kaydındaki ilgili sekiz-madde eşlemesi ve `SAPMALAR.md` taslağı.

Bu paket sahip onayı almadan library code'u değişmez.

### 11.3 Tasarım sınırları

Yetki verilirse hedef genel GPU API'si değil, mevcut bounded external-surface divergence'ının en
dar uzantısıdır:

- typed, monoton ve backend gerçeğine bağlı güvenli-retire bilgisi;
- salt okunur, bounded registry gözlemi;
- opak kimlik; raw device/texture/encoder yok;
- ölçülemeyen backend sahte sıfır dönmez;
- normal paint yolu özel grup yokken yeni maliyet ödemez;
- parked Wayland döngüsü yalnız gerçek özel-grup demand'iyle uyanır;
- test seam'i gerçek GPUI platform yarısı kanıtı gibi sunulmaz.

İkinci frame/present correlation hook'u ancak mevcut public diagnostics'in dış GPU işini frame'le
eşleyemediği ölçülürse açılır. Aksi halde tek divergence bounded external-surface köprüsü olarak
kalır.

### 11.4 Uygulama sırası — yalnız yetki sonrası

| Adım | İş | Kapı |
|---|---|---|
| D1 | Ortak additive tipler ve default unsupported | Unit, ordinary-path zero-cost |
| D2 | D3D11 registry/retire gerçeği | Windows gerçek runtime |
| D3 | WebGL2 registry/retire gerçeği | Gerçek browser |
| D4 | Metal | macOS Metal runtime |
| D5 | wgpu Vulkan/GL/WebGPU | Üç gerçek backend profili |
| D6 | Gerekirse frame correlation hook | Ayrı ölçüm ve ayrı sahip onayı |

D3D11 ve WebGL2 ilk feasibility kapılarıdır. Yalnız Metal/Vulkan'da çalışan tasarım portable kabul
edilmez.

## 12. Commit planı

| Commit | İçerik | Birlikte olmaması gereken |
|---|---|---|
| P0 | Bu analiz ve plan belgeleri | Kaynak senkronu |
| S1 | Atomik Zed source sync + standalone manifest/lock + provenance + sibling wgpu pin | Yeni runtime/API divergence |
| S2 | Wasm benchmark target izolasyonu ve gerekiyorsa cargo-shear extraction hijyeni | GPUI davranış değişikliği |
| S3 | Yalnız gerekli focused tüketici testleri | Production error/alpha politika değişikliği |
| S4 | Runtime kanıt ve yetenek statüsü belgeleri | Kanıtsız “destek” iddiası |
| D0 | Yeni seam için `SAPMALAR.md` karar kaydı | Library code |
| D1+ | Yetkilendirilmiş additive uygulama, backend başına reviewable commit | gpui-ec tüketici değişikliği |

S1 büyük ama atomik olmak zorundadır: kaynak revizyonu, standalone uyarlama ve provenance birbirini
tanımlar. S1 geçici kırmızı commit olarak ana dala girmez. D0'dan sonraki kırmızı nöbetler ayrı
feature dalında, hangi committe düştükleri görünür biçimde korunabilir; squash ile “hep yeşildi”
görüntüsü verilmez.

## 13. Risk kaydı

| Risk | Erken belirti | Önlem |
|---|---|---|
| External-surface seam'i senkronda sessiz düşer | Capability default unsupported olur veya test sayısı azalır | Symbol diff + focused altı-profile test |
| Rich-text sapması dependency cleanup ile kırılır | macOS/CoreText veya cosmic-text cfg hedefi derlenmez | cfg-target metadata + focused tests |
| Wayland parked loop özel grubu kaybeder | dirty/pending external iş varken frame schedule edilmez | next-frame/pending-present ve gerçek external fixture |
| Queue validation hatası kaybolur | handler/log/failure count görmez | controlled error-scope/uncaptured test |
| Metal alpha mode HAL panic'i | capability picker unsupported mode seçer | gerçek Metal surface configure/draw + preference test |
| cargo-shear fazla dependency siler | yalnız uzak hedef kırılır | dört cfg ailesi ve runtime matrix |
| Wasm all-target kapısı yanlış güven verir | bench hiç sınanmıyor veya production crate atlanıyor | host bench ayrı, wasm lib/all-target ayrı |
| Windows cross-compile runtime sanılır | D3D11/Restart Manager “kanıtlandı” yazılır | statü sözlüğü ve gerçek Windows künye şartı |
| Yeni seam genelleşir | raw device/encoder veya normal path hook önerilir | D0 imza/sınır review ve drop condition |
| Zed checkout yeniden kirlenir | local commit/diff | fast-forward-only, final status audit |

## 14. Tamamlanma tanımı

GPUI işi ancak aşağıdakilerin hepsi sağlandığında “tamamlandı” denir:

1. `UPSTREAM.md` ve retained kaynaklar aynı Zed revizyonunu gösterir.
2. Sibling wgpu revizyonu dokümantasyon, path dependency ve kanıt künyesinde aynıdır.
3. Dört kayıtlı sapmanın bırakma koşulu yeniden denetlenmiş ve focused kanıtı geçmiştir.
4. Host format/metadata/check/clippy/serial test kapıları yeşildir.
5. Wasm iki feature yolu compile edilir; all-target benchmark kapsamı dürüstçe kapatılır.
6. Windows ve Linux cross sonuçları runtime statüsüyle karıştırılmaz.
7. Wayland/X11/Windows değişiklikleri ilgili gerçek platformda kanıtlanır veya açık
   “ölçülmedi/unsupported” statüsü taşır.
8. Metal/Vulkan/GL/WebGPU/WebGL2 external-surface profilleri eski desteğini kaybetmez.
9. Yeni GPUI seam'i senkron commit'inde yoktur; gerekirse D0 sahip kararından sonra ayrı başlar.
10. Zed ve wgpu temiz, GPUI hedef iş dışında temizdir; commit/push durumu açıkça raporlanır.
