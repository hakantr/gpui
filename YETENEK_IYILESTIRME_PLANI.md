# GPUI boya, kompozit ve offscreen yetenek iyileştirme planı

| Alan | Değer |
|---|---|
| Plan tarihi | 12 Ağustos 2026 |
| Belge durumu | Karar kapılı yürütme planı; tek başına kod veya sapma yetkisi vermez |
| Upstream baseline | Zed `6ae52316bedfc46e07ad740d647c669206853503` |
| Standalone GPUI baseline | `bc164088d2cd43357e320cd0ebd129a9cc55b33f` |
| Canvas2 zincir kanıtı | `8d359de59786ba0ec763c03137e21afc63ddccc1` |
| wgpu 30 inceleme baseline'ı | `422b1c3a2c08feb39e63fdb7ca2798b26803d427` |
| Rust araç zinciri | `1.97.1` |
| Tüketici | `gpui_canvas2` |
| Yönetim kuralı | Upstream paritesi varsayılandır; her runtime/public API farkı koddan önce `SAPMALAR.md` kapısını ayrı ayrı geçer. |

## 1. Amaç

Canvas2'nin gradient, kompozit, dönüşümlü katman, filtre ve gölge işlerini normal karede
`GPU -> CPU readback -> RenderImage -> GPU upload` zinciri kurmadan GPUI sahnesine taşıyabilmesini
sağlamak. CPU renderer doğruluk sahibi ve deterministik fallback olarak kalır; tek atomik kare,
bütçe ve device-loss davranışı korunur.

Bu belge tek başına yerel sapma yetkisi vermez. Her dilim önce tüketici ölçümüyle gerçek bir sınırı
kanıtlar, sonra upstream'de karşılığın bulunmadığını yeniden doğrular.

### 1.1 Kapsam ve kapsam dışı

Kapsam; ölçüm/telemetri, bounded boya ve kompozit seçimi, dönüşümlü image, aynı karede composable
offscreen, bounded path shadow/filter ve yalnız kanıtlanırsa backend hızlı yoludur. Aşağıdakiler bu
planın parçası değildir:

- genel amaçlı render graph veya sınırsız consumer shader API'si;
- Zed'e değişiklik hazırlamak/göndermek ya da `../zed` Git durumunu değiştirmek;
- sırf yeni major olduğu için wgpu 30'a geçmek;
- CPU doğruluk renderer'ını kaldırmak veya platforma göre sessiz semantik değiştirmek;
- cross-compile sonucunu gerçek backend runtime, GPU completion ya da present kanıtı saymak;
- önceki performans/golden kayıtlarını yeni araç zinciriyle geriye dönük yeniden adlandırmak.

### 1.2 Bugünkü karar özeti

| Konu | Karar | Yeniden açma koşulu |
|---|---|---|
| GPUI runtime/public API değişikliği | **Henüz yetkili değil** | G1a/G1b ölçümü gerçek consumer sınırını ve public alternatifin yetersizliğini gösterir. |
| R0–R5 | Zincir düzeyinde taşınabilir | İlgili iş paketi giriş kanıtı ve G2 doğruluk sözleşmesi tamamlanır. |
| R6 | Genel yetenek olarak taşınamaz | Yalnız Metal hızlı yol + R4/P5 fallback + bayt eşitliği ve net kazanç. |
| R7 | Yol haritası dışında | En az iki bağımsız consumer aynı bounded API sınırını kanıtlar ve ayrı tasarım yetkisi alınır. |
| wgpu 30 | **Kapalı/koşullu** | Zed upstream sync'i veya aynı Linux/Web fixture'ında ölçülmüş 29→30 runtime kazanımı. |

Bu tablo fizibiliteyi yetkiden ayırır: zincirin taşıyabilmesi değişikliğin yapılması gerektiğini
kanıtlamaz.

## 2. Başlangıç raporunda düzeltilen noktalar

Plan aşağıdaki doğrulanmış kaynak gerçeklerini esas alır:

1. `Background` yalnız iki tür taşımaz: `Solid`, `LinearGradient`, `PatternSlash` ve
   `Checkerboard` vardır. Eksik olan genel pattern değil diye genellenemez; eksik yüzey
   radyal/konik ve ikiden çok duraklı gradient ile consumer image-pattern boyasıdır.
2. Windows renderer D3D12 değil **Direct3D 11** kullanır.
3. Canvas2'de 26 modun 11'i Porter-Duff grubudur; `SourceOver` zaten bu 11'in içindedir.
   Eksik Porter-Duff modu 10, sanatsal blend modu 15'tir.
4. GPUI backend'lerinde tek bir blend state yoktur; path, subpixel ve yüzey alpha moduna göre
   birden çok iç state vardır. Eksik olan consumer'ın primitive/composite modunu seçebildiği
   public sözleşmedir.
5. Beş Porter-Duff modu kaynak geometrisinin dışındaki hedef pikselleri de etkiler:
   `SourceIn`, `SourceOut`, `DestinationIn`, `DestinationAtop`, `Copy`. Yalnız primitive blend
   state'i bu pikselleri çalıştırmaz; bunlar offscreen/tam-clip maskesi olmadan R2'nin "ucuz"
   dilimine alınamaz.
6. `paint_layer` render target veya clip API'si değildir; çakışmayan geometri için draw-order/
   batching katmanıdır.
7. Mevcut shadow shader'ı keyfî maskeyi bulanıklaştırmaz; rounded-rect SDF'yi analitik hesaplar.
   Keyfî path shadow, bir path maskesi ve genel blur geçişi gerektirir.
8. Üç renderer da path çizimi için iç ara hedef taşır: Metal'de MSAA eşlikçili
   `path_intermediate_texture`, Windows'ta `ID3D11Texture2D` + SRV, wgpu'da
   `path_intermediate_texture`. Bu R4'ü sıfırdan backend altyapısı kurma işinden mevcut makineyi
   genelleştirip açma işine indirir; yine de tek başına public/composable offscreen kanıtı değildir.

### 2.1 Kanıt defteri ve eskime kuralı

| İddia | Kanıt yüzeyi | Durum |
|---|---|---|
| Windows ana renderer D3D11 | `crates/gpui_windows/src/directx_renderer.rs`: `ID3D11Device` | Kaynakla doğrulandı |
| Üç renderer'da path intermediate var | Metal/DirectX/wgpu renderer kaynakları | Kaynakla doğrulandı |
| wgpu 30 R6'yı açmıyor | `../wgpu/wgpu-types/src/features.rs`, `render.rs` | Yerel checkout ile doğrulandı |
| R0 dışındaki mevcut public GPUI telemetrisi CPU draw/present cadence ölçer | `FrameDurationSnapshot` ve `Window::present` | Kaynakla doğrulandı; GPU completion değildir |
| Canvas2 istek/zincir analizi | `../gpui_canvas2/reports/GPUI_IMPROVEMENTS.md`, `8d359de` | Plan girdisi; güncel kaynakla kritik maddeleri yeniden doğrulandı |

GPUI/Zed/wgpu baseline'larından biri değişirse bu defter ve etkilenen zincir satırları yeniden
doğrulanmadan uygulama dilimi açılmaz. Eski benchmark/provenance dosyaları dönem kanıtıdır; güncel
başarı iddiası olarak kullanılmaz.

## 3. Değişmez kapılar

Yeni bir istek plana girmeden önce **Canvas2 → GPUI public yüzeyi → renderer içi emsal → en zayıf
bağımlılık/backend → donanım** zincirinden geçirilir. İstek önce primitif, veri genişletmesi, geçiş
veya telemetri olarak sınıflandırılır. Public yüzeyde zaten varsa consumer işidir; yalnız renderer
içinde emsali varsa "inşa et" değil "genelleştir ve aç" olarak küçültülür. En zayıf halka isteği
doğrudan taşıyamıyorsa ancak **hızlı yol + taşınabilir fallback + bayt eşitliği** biçiminde plana
alınabilir; fallback de yoksa istek dosyalanmaz ve kırık halka adıyla kaydedilir. R0 ölçümü bu
zincirin önkoşuludur: ölçülemeyen ihtiyaç GPUI sapması doğuramaz.

Her uygulama dilimi için sıra aynıdır:

1. Aynı fixture ve timeline'da mevcut consumer yolu ölçülür: CPU prepare/raster/encode,
   GPUI submission/upload, draw, GPU completion/present ve frame pacing ayrı scope olur.
2. Public GPUI ile consumer alternatifi uygulanır veya neden orantısız olduğu ölçülür.
3. Kazanç p50/p95, upload/readback byte'ı, CPU/RSS ve varsa GPU süre/bellek olarak sayısallaştırılır.
4. Güncel Zed `main` yeniden taranır. Upstream karşılık varsa normal senkron yapılır; yerel fark açılmaz.
5. Yerel fark gerekiyorsa `SAPMALAR.md` girdisi koddan önce sınır, elenen consumer yolu, kazanç,
   dosyalar ve bırakma koşuluyla eklenir.
6. API mümkün olduğunca additive olur; mevcut primitive davranışı değiştirilmez.
7. CPU referans corpus'u ile son 8-bit çıktı doğrulanır. Cross-compile, backend runtime kanıtı
   veya present ölçümü diye raporlanmaz.

Her kapı tek sonuç üretir:

- **devam:** limit, consumer alternatifi ve kazanım kanıtlı; zincir/fallback tam;
- **consumer'da kapat:** public GPUI alternatifi hedef bütçeyi karşılıyor;
- **ertele:** yetenek taşınabilir ama ölçüm, cihaz veya corpus kanıtı eksik;
- **reddet:** en zayıf halkada yetenek ve kabul edilebilir fallback yok ya da ölçülmüş kazanç bakım
  maliyetini karşılamıyor.

Karar kaydında en az fixture hash'i, GPUI/Canvas2/Zed revizyonu, Rust sürümü, OS/GPU/sürücü,
backend profili, çözünürlük/DPR, feature'lar, warm-up/örnek sayısı, p50/p95/p99, upload/readback
byte'ı, CPU/RSS ve ölçümün semantik adı bulunur. `gpu_complete` yalnız GPU query/fence gerçekten
tamamlandığında; `present_complete` yalnız platform sunum tamamlanmasını bildirdiğinde kullanılır.

Onaylı hedef kare bütçesi ve karşılaştırma cihazları G1b sonunda açıkça kaydedilmeden hiçbir
performans dilimi "başarılı" sayılmaz.

## 4. Aşamalar ve bağımlılıklar

### G0 — upstream ve kanıt baseline'ı

- `../zed` yalnız fast-forward ile güncellenir; kaynak dosyaları değiştirilmez.
- Standalone extraction yeni revizyona alınır, repo-local `gpui 0.3.0` ve kayıtlı rich-text
  sapması yeniden uygulanır.
- Upstream'in kapsadığı sapmalar düşürülür. Bu baseline'da `stacksafe 1.x` sapması düşmüştür.
- `UPSTREAM.md`, `NOTICE`, `EXTRACTION.md`, `yetenek.md` ve sync aracı aynı commit'i gösterir.
- Format, locked workspace check, seri workspace test ve `scripts/verify-sapmalar.sh` çalışır.

**Giriş:** Uygulama için seçilmiş Zed revizyonu ve temiz, salt-okunur `../zed` checkout'u.

**Çıkış:** Temiz parite fark listesi, doğrulama çıktısı ve tek baseline commit'i. Mevcut
`bc164088...` baseline kaydı bu planın başlangıç noktasıdır; ilk runtime diliminden önce kapılar
yeniden çalıştırılır. macOS pasteboard/sandbox gibi bir test ortamı engeli kaynak başarısı diye
yorumlanmaz, ayrı ve yeniden üretilebilir engel kaydı olur.

**Geçersizleşme:** Zed/GPUI baseline, Rust aracı veya kayıtlı sapmalar değiştiğinde G0 yeniden açılır.

### G1a — Canvas2 consumer alternatifi ve ölçüm açığı

Canvas2 aynı immutable fixture'larda aşağıdaki sayaçları üretir:

- frame başına solid/linear/radial/conic/multi-stop gradient primitive sayısı ve kaplanan piksel;
- 26 composite modunun kullanım dağılımı ve CPU composite süresi;
- kamera rotate/scale sırasında yeniden rasterlanan layer sayısı/byte'ı;
- shadow/filter halo alanı, blur yarıçapı ve CPU band-margin maliyeti;
- tam image ile public `paint_image` tile-grid alternatifinin upload/atlas churn karşılaştırması;
- GPUI `frame-duration-histogram` ile draw ve animasyon present aralığı. GPU fence yoksa ölçüm
  `gpu_complete` adını almaz.

Önce public consumer rotaları denenir: immutable tile-grid, mevcut `Background`, image yeniden
raster/cache, CPU offscreen/filter ve mevcut frame diagnostics. Her rotanın neden yeterli olduğu
veya maliyetinin neden orantısız kaldığı aynı fixture'da kaydedilir.

**Giriş:** G0 ve değişmez fixture/timeline manifesti.

**Çıkış:** Her R maddesi için `consumer'da kapat / kanıtlandı / ertele / reddet` kararı; ayrıca
mevcut CPU draw/present cadence verisinin darboğazı ayırmaya yetip yetmediği. Sayısal limit
göstermeyen madde GPUI koduna geçmez.

### P0 — R0: ölçüm ve GPU zamanlama sözleşmesi

R0 diğer runtime işlerinin önkoşuludur, fakat kendi yetkisi G1a'daki ölçüm açığından gelir. Mevcut
`FrameDurationSnapshot` CPU `Window::draw` süresi ile animasyon sırasında ardışık present çağrıları
arasındaki cadence'ı ölçer; GPU completion veya platform present completion ölçmez.

Asgari additive sözleşme:

- diagnostics kapalıyken query/allocation ve runtime davranış maliyeti yok;
- sonuç `unsupported(reason)`, `pending(frame_id)` veya `complete(frame_id, scopes)` durumlarından
  biri; desteklenmeyen backend sahte `0 ns` döndürmez;
- query readback render thread'ini bloklamaz, gecikmeli sonuç ve düşen örnek sayısı görünürdür;
- CPU encode/submit, GPU scope ve present cadence ayrı alanlardır; biri diğerinin adıyla sunulmaz;
- Metal counter sample, D3D11 timestamp+disjoint query, wgpu `TIMESTAMP_QUERY` destekliyse gerçek
  GPU sonucu; WebGPU adapter desteği yoksa ve WebGL2'de yapılandırılmış `unsupported`;
- özellik/kabiliyet sorgusu consumer'ın ölçüm kapsamını önceden seçmesine izin verir.

**Giriş:** G1a'da en az bir kararın GPU attribution eksikliği yüzünden verilemediği ve platform
profilerinin sürdürülebilir consumer çözümü olmadığı gösterilir.

**Çıkış:** Aynı frame kimliğiyle CPU draw, submit/present cadence ve desteklenen profillerde GPU
scope'u ayrıştıran snapshot; etkinleştirme maliyeti G1a/D-R0'da önceden onaylanan telemetri
bütçesini aşmaz.

**Durdurma/bırakma:** Mevcut public diagnostics + platform araçları karar için yeterliyse public R0
açılmaz. Upstream eşdeğer yüzey sağladığında yerel sözleşme düşürülür.

### G1b — ölçülmüş öncelik ve hedef bütçesi

R0 sonucu veya açıkça adlandırılmış `unsupported` profilleriyle aynı fixture'lar tekrar çalışır.
Karşılaştırma aynı cihazda baseline/adayı dönüşümlü koşar; varsayılan minimum üç tekrar ve tam
17 saniyelik timeline'dır. Statik fixture'da warm-up sonrası en az 300 frame alınır. Fark ölçüm
gürültüsünden ayrılmıyorsa karar `ertele`dir.

**Çıkış:** Onaylı hedef frame bütçesi, telemetri overhead bütçesi, cihaz matrisi ve her R maddesi
için beklenen p95 kazanç/byte azalması. Bu kayıt P1–P7 arasındaki sırayı belirler; belge sırası tek
başına öncelik değildir.

### G2 — ortak doğruluk ve backend corpus'u

- Gradient: tekrarlı/aynı konumlu stop, 0/1 sınırı, degenerate geometri, radial iki-daire ve conic
  açı corpus'u; Canvas2 CPU renk sözleşmesi ile GPUI `ColorSpace` eşlemesi yazılı olur.
- Composite: 11 Porter-Duff + 15 sanatsal mod; saydam/yarı saydam kaynak-hedef ve coverage dışı
  piksel vakaları.
- Image: nearest/linear sampling, crop, content mask, negatif determinant, singular transform ve
  şeffaf kenar.
- Shadow/filter: radius 0, büyük radius/bütçe sınırı, clip, offset ve alpha corpus'u.
- Runtime profilleri: macOS Metal, Windows D3D11, Linux wgpu-Vulkan, Linux wgpu-GL, browser
  WebGPU ve browser WebGL2.

**Giriş:** G1b'de en az bir yetenek için `devam` kararı.

**Çıkış:** CPU referansıyla açık tolerans sözleşmesi ve fixture manifesti. Varsayılan kabul son
8-bit çıktıda bayt eşitliğidir; bunun matematiksel olarak mümkün olmadığı özellikte ortak,
sayısallaştırılmış kanal toleransı değişiklikten önce kaydedilir. Platforma göre golden ayrımı
varsayılan değildir; gerekiyorsa farkın nedeni ve kapsamı ayrıca onaylanır.

### P1 — R1: bounded gradient boyası

Önce veri taşıma spike'ı yapılır. `Background` her quad/path verisine gömülü `repr(C)` bir tip
olduğu için doğrudan büyük sabit stop dizisi eklemek ile `offset + count` kullanan bounded stop
tablosu karşılaştırılır. WebGL2 storage buffer taşımadığından stop tablosunun instance uint texture
veya eşdeğer taşınabilir yolu kanıtlanır.

Asgari ürün:

- additive radial/conic constructor ve açık maksimum stop sayısı;
- sıralama, duplicate offset, taşma ve degenerate davranışı;
- Metal/HLSL/WGSL storage ve WebGL2 shader eşdeğerliği;
- primitive boyutu, upload byte'ı ve pipeline maliyeti ölçümü.

**Giriş:** G1b radial/conic/multi-stop hazırlama, raster veya upload maliyetini hedef bütçeyi aşan
ayrı bir darboğaz olarak gösterir; mevcut bounded `Background` rotası ya da consumer raster/cache
aynı bütçeyi karşılamaz. G2 gradient corpus'u ve renk uzayı kararı hazırdır.

**Hedef yüzey:** Additive public constructor/type; `color.rs`/`scene.rs` primitive verisi; Metal,
HLSL, native WGSL ve WebGL WGSL veri okuma yolu; renderer upload/binding ve layout testleri. Spike,
WebGL2 dahil tek bounded `MAX_STOPS` ile stop tablosunun taşıma biçimini ve primitive başına byte
bütçesini koddan önce seçer.

**Çıkış:** Canvas2 Aşama 2 gradient fixture'ları CPU referansıyla kabul edilir; sınır aşımı
yapılandırılmış hata verir, sessiz stop düşürmez. Baseline'a göre p95 frame maliyeti, primitive ve
frame upload byte'ı ile pipeline/cache etkisi raporlanır.

**Durdurma/bırakma:** WebGL2'de bounded taşıma ABI veya onaylı bellek bütçesi içinde kurulamazsa
`reddet`; consumer raster/cache bütçeyi karşılıyorsa `consumer'da kapat`; ölçüm gürültüsünden üstün
kazanç yoksa `ertele`. Upstream eşdeğer stop/gradient sözleşmesi sağladığında sapma düşürülür.

### P2 — R2a: coverage dışını değiştirmeyen Porter-Duff modları

İlk küçük dilim yalnız `SourceAtop`, `DestinationOver`, `DestinationOut`, `Lighter` ve `Xor`u
açar. `SourceOver` mevcut davranıştır. Pipeline varyant sayısı ve cache anahtarı bounded tutulur;
her primitive ailesinin straight/premultiplied çıkış sözleşmesi ayrı doğrulanır.
`wgpu::BlendState` çekirdek tiptir; Metal, D3D11 ve wgpu renderer'larında birden çok amaç-özel iç
blend state zaten vardır. Bağımlılık halkası tamdır; eksik olan consumer'ın kompozit modunu
primitive başına seçebildiği GPUI sözleşmesi ve bounded pipeline anahtarıdır.

**Giriş:** G1b kullanım dağılımında bu beş moddan en az biri gerçek fixture'da kullanılır ve CPU
kompozit ya da readback/upload yolu hedef bütçeyi aşar; G2 composite corpus'u hazırdır. Kullanılmayan
mod yalnız API tamlığı amacıyla açılmaz.

**Hedef yüzey:** Tek additive bounded mode enum'u; ilgili primitive verisi ve pipeline/cache
anahtarı; Metal `MTLBlendDescriptor`, D3D11 blend descriptor ve wgpu `BlendState` eşlemeleri;
straight/premultiplied alpha ile coverage testleri. Varsayılan değer mevcut `SourceOver` davranışını
ve ABI'yi mümkün olan en dar biçimde korur.

**Çıkış:** Beş yeni mod bütün runtime backend'lerinde CPU referansıyla eşleşir. Diğer beş mod bu
dilimde başarı diye raporlanmaz. Pipeline varyantı, cache hit oranı ve p95 kazanç baseline'la
karşılaştırılır.

**Durdurma/bırakma:** Consumer'ın mevcut offscreen/CPU yolu hedefi karşılıyorsa `consumer'da
kapat`; pipeline patlaması veya cache churn onaylı bütçeyi aşıyorsa tasarım küçültülür, küçülemiyorsa
`reddet`; kazanç gürültü sınırındaysa `ertele`. Upstream eşdeğer primitive kompozit seçimi
sağladığında sapma kaldırılır.

### P3 — R3: dönüşümlü polychrome image

Mevcut `paint_image` değiştirilmeden additive `paint_transformed_image` benzeri bir yüzey açılır.
Transformun uzayı/pivotu, source crop, sampling, content-mask koordinatı, corner-radii davranışı,
singular transform ve şeffaf kenar sözleşmesi önce sabitlenir. `PolychromeSprite` shader ABI'si
Metal, HLSL, native WGSL ve WebGL WGSL yollarında güncellenir.

**Giriş:** G1b, 17 saniyelik kamera timeline'ında axis-aligned `paint_image` yüzünden yeniden
rasterlanan layer sayısı/upload byte'ı veya CPU süresinin bütçeyi aştığını; immutable tile-grid ve
mevcut image cache alternatiflerinin karşılamadığını gösterir. G2 image corpus'u hazırdır.

**Hedef yüzey:** `Window::paint_image` davranışını değiştirmeyen additive çağrı; `PolychromeSprite`
için dönüşüm verisi; Metal/HLSL/native WGSL/WebGL WGSL vertex ve clip matematiği; sampling/crop ve
layout testleri. Monochrome sprite'taki dönüşüm emsal alınır, fakat polychrome corner-radius ve
content-mask semantiğinin kendiliğinden aynı olduğu varsayılmaz.

**Çıkış:** 17 saniyelik camera timeline'da layer yeniden raster sayısı ve upload byte'ı ölçülür;
görsel corpus CPU yolu ile eşleşir. Ölçülmüş kazanç bakım maliyetini aşmıyorsa sapma korunmaz.

**Durdurma/bırakma:** Public tile/cache yolu hedefi karşılıyorsa `consumer'da kapat`; negatif veya
singular dönüşüm, crop ya da şeffaf kenar sözleşmesi altı profilde ortaklaştırılamıyorsa `reddet`;
kazanç gürültü sınırındaysa `ertele`. Upstream polychrome affine image yüzeyi sağladığında sapma
düşürülür.

### P4 — R4: composable offscreen grup

Bu orta büyüklükteki public genelleştirme flat `Scene` yürütmesini çok geçişli hâle getirir. Üç
renderer da path rasterizasyonu için kendi ara texture/makinesini zaten taşır; iş sıfırdan
render-to-texture kurmak değil, bu tek-amaçlı yaşam döngüsünü consumer grupları, nesting ve bütçe
sözleşmesi için güvenle genelleştirip açmaktır:

- bounds, format/color-space, sample count ve bütçesi açık descriptor;
- aynı karede üretilen sonucu texture kaynağı olarak kullanma; normal karede CPU readback yok;
- nested depth, toplam piksel/byte, geçici surface pool ve yaşam süresi sınırı;
- content mask, group opacity, draw order ve device-loss davranışı;
- render-pass bağımlılıklarının cycle ve use-after-free doğrulaması.

Mevcut Metal/Windows/wgpu intermediate'ları yapım riskini düşürür fakat public offscreen kanıtı
sayılmaz; aynı scene sözleşmesinin bütün runtime backend'lerde çalışması gerekir.

**Giriş:** G1b'de coverage dışı kompozit, grup opacity, filtre veya shadow senaryolarından en az biri
ölçülmüş limit üretir; consumer CPU offscreen/readback yolu ve mevcut public scene yüzeyi bütçeyi
karşılamaz. G2 ilgili corpus'u hazırdır. İlk spike mevcut path intermediate yaşam döngüsünü
genelleştirme ile ayrı bounded surface pool'unu state karmaşıklığı, VRAM ve nesting açısından
karşılaştırır.

**Hedef yüzey:** Flat scene'i genel render graph'a çevirmeyen bounded `begin/end group` veya eşdeğer
scope; scene kayıtları; Metal/D3D11/wgpu target, SRV/texture view, pass ve pool yönetimi; açık format,
sample count, max nesting, max piksel/byte ve device-loss sözleşmesi. Grup sonucu yalnız aynı frame
ve tanımlı yaşam süresi içinde tüketilir.

**Çıkış:** İçi solid/path/image primitive'leri olan nested grup aynı karede tekrar composite edilir;
CPU readback byte'ı sıfırdır ve bütçe aşımı yapılandırılmış hatadır. Surface allocation/reuse,
geçici VRAM üst sınırı, pass sayısı ve p95 maliyet raporlanır.

**Durdurma/bırakma:** Gerekli public yüzey bounded grup olmaktan çıkıp genel kaynak/pass grafiğine
dönüşüyorsa `reddet` ve R7'ye taşınmaz; nesting/VRAM/pass bütçesi taşınamıyorsa `ertele/reddet`;
consumer yolu hedefi karşılıyorsa `consumer'da kapat`. Upstream composable bounded offscreen
sözleşmesi sağladığında sapma kaldırılır.

### P5 — R2b ve sanatsal blend modları

P4 üstünde önce coverage dışını değiştiren `SourceIn`, `SourceOut`, `DestinationIn`,
`DestinationAtop`, `Copy`; sonra 15 sanatsal blend modu uygulanır. Full-clip saydam kaynak ve
offscreen hedef okuması Canvas semantiğini korur.

**Giriş:** P4 kabul edilmiştir; G1b kullanım dağılımında ilgili mod gerçek fixture'da vardır ve P4
fallback ölçümü hedef bütçeye sığar. Beş coverage-dışı mod ile 15 sanatsal mod ayrı alt dilimlerdir;
birinin başarısı diğerine yetki vermez.

**Hedef yüzey:** P2'deki tek mode enum'unun bounded genişlemesi; full-clip kaynak/hedef offscreen
geçişi; premultiplied alpha ve renk uzayı açık composite shader'ı; P4 surface pool ve pass yaşam
döngüsü. Her alt dilim yalnız gerçekten kullanılan mode kümesini açabilir.

**Çıkış:** 26 modun tamamı CPU referans corpus'unda çalışır; P2 ile P5'in sonuçları aynı mode
enum/sözleşmesini paylaşır. Her alt dilim readback=0, pass/VRAM ve p95 ölçümleriyle ayrı kapanır.

**Durdurma/bırakma:** Modlar gerçek workload'da kullanılmıyorsa açılmaz; P4 fallback CPU yolundan
daha iyi değilse `ertele/reddet`; ortak alpha/renk semantiği G2 toleransı içinde kurulamazsa
`reddet`. Upstream eşdeğer composite sözleşmesi sağladığında ilgili alt dilim düşürülür.

### P6 — R5: keyfî path shadow ve asgari filtreler

Rounded-rect shadow shader'ı genellenmiş gibi gösterilmez. P4 üstünde path/glyph alpha maskesi,
bounded blur, offset ve colorize geçişleri kurulur; mümkünse aynı çekirdek FilterGraph blur/offset
node'larını da besler.

**Giriş:** P4 kabul edilmiştir; G1b shadow/filter halo alanı ve CPU band-margin maliyetinin hedef
bütçeyi aştığını gösterir. Gerçek workload'daki radius, offset ve clip dağılımı max blur yarıçapı ile
surface bütçesini belirler; gelecekteki genel filtre isteği bunları büyütmez.

**Hedef yüzey:** Path/glyph alpha-mask üretimi; bounded yatay/dikey blur veya kanıtlanmış eşdeğeri;
offset/colorize composite; P4 surface pool; radius=0, clip ve bütçe aşımı davranışı. Mevcut
rounded-rect SDF shadow shader'ı yalnız karşılaştırma/emsal, genel blur girdisi değildir.

**Çıkış:** Metin, harita sembolü ve keyfî path shadow corpus'u clip/bütçe sınırlarıyla eşleşir;
Canvas2 band-margin CPU maliyeti ve GPU geçiş maliyeti birlikte raporlanır.

**Durdurma/bırakma:** CPU band-margin yolu hedefi karşılıyorsa `consumer'da kapat`; gerekli radius
ve halo P4 bütçesini aşıyorsa tasarım küçültülür veya `reddet`; GPU geçişi net p95 kazanç sağlamazsa
`ertele`. Upstream bounded mask/blur yüzeyi sağladığında sapma düşürülür.

### P7 — R6: programlanabilir blend hızlı yolu

Yalnız P5 ölçümü offscreen yolunun hedef bütçeyi bozduğunu gösterirse açılır. WGPU 29.0.4
ve 30 framebuffer fetch/advanced blend yüzeyi sağlamaz; dual-source blending hedef rengini okumaz
ve bu modların genel çözümü değildir. D3D11 halkasında da programlanabilir blend yoktur. Bu yüzden
R6 genel backend yeteneği olarak dosyalanamaz: yalnız Metal'de additive hızlı yol, Windows/Linux/Web
için P4/P5 fallback ve iki yolun bayt eşitliği biçiminde değerlendirilebilir. Wgpu HAL'i bypass eden
Vulkan/WebGL özel yolları varsayılan çözüm değildir.

**Giriş:** P5 tüm profillerde doğruluk ve bütçe kapısını geçmiştir, fakat aynı Metal cihazlarında
offscreen pass maliyeti onaylı hedefi hâlâ ölçülebilir biçimde aşar. Kullanılacak Metal özelliğinin
donanım/OS tabanı ve capability sorgusu kayıtlıdır; fallback her zaman P4/P5'tir.

**Hedef yüzey:** Yalnız Metal renderer'da additive, capability-gated hızlı yol ve aynı public mode
sözleşmesi. D3D11 veya wgpu için özel bypass, farklı semantik ya da consumer görünür backend seçimi
eklenmez. Her hızlı-yol sample'ı fallback ile aynı fixture/frame kimliğinde karşılaştırılabilir.

**Çıkış:** Metal hızlı yolu ile P4/P5 fallback'i aynı corpus'ta bayt bayt eşleşir ve ölçülmüş net
kazanç sağlar. En zayıf halka doğrudan yolu taşıyana kadar genel hızlı yol `beklemede` kalır.

**Durdurma/bırakma:** Bayt eşitliği bozulursa, capability tabanı desteklenen cihaz kümesini fazla
daraltırsa veya net kazanç telemetri gürültüsünü aşmazsa hızlı yol açılmaz. Upstream eşdeğer hızlı
yolu sağladığında ya da P4/P5 hedef bütçeyi tek başına karşıladığında yerel yol kaldırılır.

### R7 — consumer shader/pass yüzeyi

Planlanmış uygulama değildir. En az iki bağımsız gerçek consumer, yukarıdaki bounded primitive
yüzeyleriyle çözülemeyen aynı sınırı kanıtlamadan açılmaz. Shader doğrulama, resource erişimi,
backend dil farkı, pipeline cache, device-loss ve güvenlik sözleşmesi ayrı bir tasarım programıdır.
Bu eşik oluşursa yeni tehdit modeli, kaynak sahipliği, quota ve backend dil sözleşmesiyle ayrı plan
ve ayrı yetki hazırlanır; bu belgedeki P4 veya wgpu altyapısı R7 için örtük onay sayılmaz.

### Uygulama dilimi sözleşmesi

Her P dilimi koddan önce tek sayfalık karar kaydı üretir. Kayıt; paket kimliği ve sahibi, bağlı
Canvas2 fixture/timeline hash'i, baseline ve aday revizyonlar, tüketici alternatifi sonucu, hedef
API/ABI, dokunulacak kaynak yüzeyi, kaynak/bellek/pipeline bütçesi, altı profilli kabul matrisi,
rollback ve upstream bırakma koşulunu içerir. Belirsiz semantik kararlar uygulama sırasında shader
koduna gömülmez; en az aşağıdaki kayıtlar giriş kapısında kapanır:

| Kayıt | Koddan önce verilecek karar |
|---|---|
| D-R0 | Frame kimliği, scope sınırları, query gecikmesi, unsupported/pending/drop semantiği ve telemetry overhead bütçesi |
| D-R1 | `MAX_STOPS`, stop taşıma biçimi, sıralama/duplicate davranışı, interpolation ve renk uzayı |
| D-R2 | Mode enum'u, alpha sözleşmesi, coverage kapsamı ve maksimum pipeline varyantı |
| D-R3 | Transform uzayı/pivotu, crop/sampling, mask/corner ve singular davranış |
| D-R4 | Surface formatı, sample count, max nesting/piksel/byte, pool yaşam süresi ve device loss |
| D-R5 | Mask formatı, max radius/halo, blur kerneli ve clip davranışı |
| D-R6 | Metal capability tabanı, fallback seçimi, byte-equality ve asgari net kazanç |

Aynı anda en fazla bir GPUI runtime sapması uygulanır (`WIP=1`). Bir dilim; karar kaydı,
`SAPMALAR.md`, kaynak, test/corpus adaptasyonu ve extraction/yetenek dokümantasyonuyla tek inceleme
birimidir. Bağımsız baseline/sync/araç-zinciri değişiklikleri ayrı commit'te tutulur; bu plan ayrıca
commit yetkisi verilmedikçe uygulama commit'lerine alınmaz. Consumer benchmark sonucu GPUI
commit'ine gömülmez; Canvas2'de append-only kanıt olarak fixture hash'i ve GPUI commit'iyle saklanır.

## 5. wgpu 30 değerlendirmesi ve koşullu geçiş paketi

### 5.1 Denetlenen baseline ve karar

12 Ağustos 2026 tarihinde yerel `../wgpu` checkout'u temiz `trunk` üzerinde
`422b1c3a2c08feb39e63fdb7ca2798b26803d427` revizyonundaydı. Manifest sürümü `30.0.0`, checkout
ise `v30.0.0` etiketinin 160 commit ilerisindeydi. Kayıtlı Zed ve bu standalone extraction
`wgpu`/`naga 29.0.4` kullanıyor.

**Karar:** wgpu 30, R1–R7'nin önkoşulu değildir ve bugün ölçülmüş bir GPUI sapmasını haklı
çıkarmaz. Geçiş ana yürütme sırasına eklenmez. Zed wgpu 30'a geçtiğinde normal upstream sync ile
alınır; bundan önce yalnız Linux/web runtime ölçümü 29'a göre somut doğruluk, kararlılık veya frame
pacing kazanımı gösterirse aşağıdaki W30 kapısı açılır.

Tam zincir denetiminde R0–R5 ile R7 bağımlılık düzeyinde taşınabilir; tek kırık istek R6'dır.
wgpu 30 bu kırığı kapatmaz. R6'nın dosyalanabilir tek biçimi Metal hızlı yol + R4/P5 fallback +
bayt eşitliğidir; bu sonuç wgpu 30'a erken geçiş gerekçesi oluşturmaz.

Platform kapsamı önemlidir: `gpui_wgpu` Linux'ta Vulkan/GL, web'de Browser WebGPU/WebGL2 yoludur.
macOS ana renderer'ı doğrudan Metal, Windows ana renderer'ı D3D11 kullandığı için wgpu 30'un
Metal/DX12 değişiklikleri bu iki çalışan GPUI backend'ini kendiliğinden iyileştirmez.

| İstenen yüzey | wgpu 30 sonucu | Karar gerekçesi |
|---|---|---|
| R1 gradient | Açmaz | Gradient türü, stop depolaması ve shader ABI'si GPUI işidir. |
| R2 composite | Açmaz | Dual-source blending zaten 29'da vardır. 30'da framebuffer fetch, advanced blend equation veya destination-color shader okuması yoktur. |
| R3 affine image | Açmaz | `PolychromeSprite` transformu ve sampling sözleşmesi GPUI scene/backend değişikliğidir. |
| R4 offscreen grup | Yeni temel sağlamaz | 29 zaten `RENDER_ATTACHMENT` ve `TEXTURE_BINDING` kullanımlı texture ile çoklu render pass sağlar; mevcut path intermediate bunun kanıtıdır. 30'daki `TRANSIENT_ATTACHMENT` sampled/stored grup sonucu için kullanılamaz. |
| R5 path shadow/filter | Açmaz | Mask üretimi, blur pass'leri ve bütçe sözleşmesi GPUI'de kurulmalıdır. |
| R6 programlanabilir blend | Açmaz; zincir kırık kalır | Hedef rengi genel shader girdisi yapan taşınabilir yeni yüzey yoktur; D3D11 de taşımaz. Yalnız Metal hızlı yol + R4/P5 fallback dosyalanabilir. |
| R7 consumer shader/pass | Açmaz | Wgpu'nun shader kabul etmesi GPUI'nin güvenli public resource/pass sözleşmesinin yerine geçmez. |

wgpu 30'un ilgili fakat **koşullu** kazanımları vardır:

- `wgpu-core` queue submission ile başka thread'deki bloklayan device poll arasındaki kilitlemeyi
  gevşetir. GPUI bugün yalnız resize sırasında aynı renderer akışında bloklayan `device.poll`
  çağırdığı için mevcut kodda doğrudan kazanç beklenmez; önce eşzamanlı contention kanıtlanmalıdır.
- Vulkan acquire/fence düzeltmeleri belirli NVIDIA/uzun-kare senaryolarındaki frame spike'larını
  azaltabilir. Bu ancak Linux'ta aynı 29/30 timeline ve driver ile ölçülürse geçiş gerekçesidir.
- Surface color-space/HDR seçimi yeni bir yetenektir, fakat R1–R7 değildir. `Auto` eski davranışı
  korur; Display P3/HDR açmak GPUI renk uzayı, blending ve golden sözleşmesi için ayrı plan ister.
- WebGPU handle ve Linux DMA-BUF import yüzeyleri backend'e özel zero-copy imkânlarıdır. Bütün
  runtime matrisinde çalışan GPUI scene texture sözleşmesi olmadan Canvas2 çözümü sayılmaz.
- `StagingBelt::finish_and_recall_on_submit`, 16-bit shader türleri ve transient attachment gibi
  ekler mevcut GPUI yükleme/shader yolunda otomatik performans kazanımı üretmez; WebGL2 ve Browser
  WebGPU kapsamları ayrıca sınırlıdır.

### 5.2 Doğrulanmış geçiş spike'ı

Ana çalışma ağacına kod taşımadan temiz geçici GPUI kopyası yerel wgpu/naga 30'a bağlandı. İlk
native `gpui_wgpu --all-targets` check'i yalnız şu güncellemeleri istedi:

1. `Cargo.toml`: workspace `wgpu = "30.0.0"` ve `crates/gpui_wgpu/Cargo.toml`:
   `naga = "30.0.0"`; gerçek geçişte wgpu ailesi, `naga-types`, `wgpu-naga-bridge`, `glow` ve
   ilişkili transitif değişiklikler lockfile'a normal Cargo çözümlemesiyle girer.
2. `crates/gpui_wgpu/src/wgpu_context.rs`: web adapter isteğine
   `apply_limit_buckets: false`; probe surface config'ine `color_space: SurfaceColorSpace::Auto`.
3. `crates/gpui_wgpu/src/wgpu_atlas.rs`: test adapter isteğine
   `apply_limit_buckets: false`.
4. `crates/gpui_wgpu/src/wgpu_renderer.rs`: `is_srgb()` yerine `has_srgb_suffix()`, surface
   config'e `color_space: SurfaceColorSpace::Auto`, `frame.present()` yerine
   `queue.present(frame)`.

Bu uyarlamalarla geçici kopyada:

- `cargo check -p gpui_wgpu --all-targets` geçti;
- `cargo check --workspace --all-targets` geçti;
- `cargo test -p gpui_wgpu -- --test-threads=1` sonucu **39/39 geçti**; iki WGSL doğrulama ve iki
  WebGL-özel shader/record-layout testi de buna dahildir.

Bu yalnız macOS host compile/unit kanıtıdır; uygulama runtime'ı wgpu renderer kullanmadığı için
Linux veya browser performans kanıtı değildir. WASM check, hostta `wasm32-unknown-unknown` target ve
Rust 1.97.1 `rust-src` bileşeni bulunmadığından kaynak derlemesine ulaşmadan durdu; gerçek geçişte
iki web yapılandırması zorunlu kapı olarak kalır. Yerel wgpu trunk ayrıca kendi
`unfulfilled_lint_expectations` uyarısını üretmiştir; yayımlanmış/pinlenmiş aday bu uyarıyla ayrıca
denetlenir.

### 5.3 W30 açılırsa uygulanacak sıra

1. Önce güncel Zed taranır. Zed wgpu 30 kullanıyorsa değişiklikler verbatim normal sync ile alınır.
2. Zed hâlâ 29'daysa aynı Linux Vulkan/GL ve browser WebGPU/WebGL2 fixture'ı 29 ile 30'da ölçülür.
   Encode/submit CPU süresi, p50/p95 present aralığı, GPU completion, device-loss, atlas upload ve
   VRAM ayrı raporlanır. macOS `gpui_wgpu` unit testi bu gate'in yerine geçmez.
3. Yalnız ölçülmüş limit varsa `SAPMALAR.md` girdisi koddan önce eklenir. Hareketli `trunk` dalı
   izlenmez; yayımlanmış sürüm veya gerekçeli tek commit hash'i pinlenir ve bırakma koşulu Zed'in
   aynı ya da yeni sürümü benimsemesidir.
4. Yukarıdaki dört kaynak uyarlaması yapılır; Cargo.lock'taki wgpu/naga ailesi incelenir. Yeni HDR,
   limit bucketing, external texture veya 16-bit feature'ları varsayılan olarak açılmaz.
5. `cargo fmt`, locked native workspace check, `scripts/verify-sapmalar.sh`, seri workspace test,
   Linux Vulkan/GL runtime, Windows/macOS regresyonu ve iki WASM yapılandırması çalıştırılır.
6. 29 baseline'ına karşı görsel corpus eşitliği ve hedeflenen ölçülmüş kazanım sağlanmazsa geçiş
   geri alınır; sırf daha yeni major sürüm olduğu için yerel sapma taşınmaz.

## 6. Önerilen yürütme sırası

1. **G0:** Baseline/parite doğrulanır.
2. **G1a:** Canvas2 public alternatifleri ve mevcut telemetriyle limitler ölçülür.
3. **D0 — R0 kararı:** Mevcut kanıt darboğazı ayırmaya yetiyorsa P0 açılmadan G1b'ye geçilir;
   yetmiyorsa yalnız attribution açığını kapatan P0 tamamlanır ve G1a ölçümü tekrarlanır.
4. **G1b:** Hedef bütçe, cihaz matrisi ve ölçülmüş öncelik kaydedilir.
5. **G2:** Ortak CPU referansı, tolerans ve altı profilli corpus sabitlenir.
6. **İlk runtime dilimi:** P1, P2, P3 veya P4 arasından belge sırasına göre değil, G1b'deki en büyük
   doğrulanmış p95/byte kazanımına göre yalnız biri açılır.
7. **Bağımlı dilimler:** P4 kabulünden sonra P5 veya P6; P5 kabulünden sonra yalnız koşulları oluşursa
   P7 açılabilir.

Bağımlılık özeti:

```text
G0 -> G1a -> D0 ---- yeterli ölçüm ----------------> G1b -> G2 -> {P1 | P2 | P3 | P4}
                  \- attribution eksik -> P0 -> G1a -/                 P4 -> {P5 | P6}
                                                                           P5 -> koşullu P7
```

P1/P2/P3/P4 G2 sonrasında teknik olarak bağımsızdır; `WIP=1` yüzünden paralel uygulama yetkisi
vermez. R7 yol haritası dışındadır. W30 bu zincirin parçası değildir; yalnız Bölüm 5.3'teki
ölçüm/parite kapısı açılırsa bağımsız altyapı dilimi olarak yürütülür ve aktif runtime paketiyle
aynı commit'te karıştırılmaz.

### 6.1 Başlangıç durum panosu

| Paket | Bugünkü durum | Açan kanıt |
|---|---|---|
| G0 | Başlangıç baseline'ı mevcut; runtime öncesi yeniden doğrulanacak | Seçilmiş güncel Zed revizyonu |
| G1a | **İlk yürütülecek iş** | Immutable Canvas2 fixture/timeline manifesti |
| P0/R0 | Karar bekliyor | G1a'da attribution açığı |
| G1b/G2 | G1a/P0 sonucunu bekliyor | Ölçülebilir limit ve hedef bütçe |
| P1–P4 | Yetkisiz/beklemede | İlgili G1b `devam` kararı + G2 |
| P5/P6 | Bağımlı/beklemede | P4 kabulü ve ayrı kullanım/performans kanıtı |
| P7 | Koşullu/beklemede | P5 kabulü + Metal'de ölçülmüş fallback bütçe aşımı |
| R7 | Plan dışı | İki bağımsız consumer + ayrı program yetkisi |
| W30 | Kapalı | Zed sync'i veya 29→30 Linux/Web runtime kazanımı |

İlk somut çıktı GPUI kodu değil, G1a karar matrisi olmalıdır. Böylece taşınabilir olduğu bilinen ama
gerçek workload'da gerekmeyen API'ler açılmaz.

### 6.2 Kanıt artefaktı sözleşmesi

Canvas2 tarafında her ölçüm koşusu immutable bir manifest ve makine-okunur sonuç üretir. Manifest;
fixture/timeline hash'i, build profili, feature'lar, GPUI/Canvas2/Zed revizyonları ve cihaz profilini;
sonuç ise scope semantiği, warm-up/örnek sayısı, p50/p95/p99, CPU/RSS, upload/readback ve varsa
gerçek GPU süresini taşır. Baseline ve aday aynı schema'yı kullanır. Özet rapor ham dosyaya işaret
eder; elle kopyalanmış tek sayı kabul kanıtı değildir. Hatalı koşu silinmez, `invalid` nedeni ile
işaretlenir; yeni koşu yeni kimlik alır.

## 7. Risk kaydı

| Risk | Erken sinyal | Koruma/karar |
|---|---|---|
| Primitive/ABI büyümesi | Instance ve frame upload byte'ı artar | P1/P2/P3 spike'ında byte bütçesi; WebGL layout testi; sınır aşımında tasarımı küçült |
| Pipeline varyant patlaması | Cache hit düşer, creation spike oluşur | Bounded mode/key; varyant sayısı ve p95 creation telemetrisi |
| Offscreen VRAM/lifetime hatası | Pool büyür, use-after-free/device-loss sorunu | P4 max byte/nesting, frame-scoped handle, allocation/reuse ve device-loss corpus'u |
| Renk/alpha backend sapması | Platform golden'ları ayrışır | Tek CPU sözleşmesi; premultiplied/ColorSpace kararı; ortak tolerans koddan önce |
| R0 query overhead/stall | Diagnostics açıkken p95 bozulur | Async query, dropped/pending sonucu, kapalıyken sıfır query; önceden tanımlı overhead bütçesi |
| WebGL2 en zayıf halka | Shader/layout veya limit taşınamaz | G2 WebGL2 ilk spike kapısı; işlevde sessiz fallback/özellik kaybı yok |
| Upstream sync taşıma maliyeti | Sürekli conflict veya aynı özellik upstream'de belirir | Additive küçük dilim, dosya listeli `SAPMALAR.md`, her sync'te bırakma denetimi |
| Benchmark yanlılığı | Tek koşu/tek cihaz kazancı | Aynı cihazda dönüşümlü baseline-aday, en az üç tekrar, provenance ve gürültü eşiği |

## 8. Doğrulama kapıları

Her GPUI diliminde en az:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
scripts/verify-sapmalar.sh
cargo test --workspace --locked -- --test-threads=1
```

Buna ek olarak ilgili feature, shader parser/layout testleri, Windows ve Linux cross-target check ile
iki WASM yapılandırması çalışır. `.github/workflows/ci.yml` invocation'ları derleme için kaynak
otoritedir. Derleme kapısından sonra gerçek runtime kabul matrisi ayrıdır:

| Profil | Zorunlu runtime kanıtı | Özel not |
|---|---|---|
| macOS Metal | G2 corpus + performans + device-loss/resize regresyonu | P7 hızlı yol ve fallback ayrı ölçülür |
| Windows D3D11 | G2 corpus + performans + device-loss/resize regresyonu | D3D12 sonucu yerine geçmez |
| Linux wgpu-Vulkan | G2 corpus + performans + surface/device-loss regresyonu | W30 varsa 29 baseline'ıyla aynı cihaz/sürücü |
| Linux wgpu-GL | G2 corpus + performans + surface/device-loss regresyonu | Vulkan sonucu yerine geçmez |
| Browser WebGPU | G2 corpus + performans + adapter capability kaydı | R0 timestamp yoksa `unsupported` kabul edilebilir |
| Browser WebGL2 | G2 corpus + performans + context-loss regresyonu | Yalnız R0 GPU timestamp'i `unsupported` olabilir |

R0 dışında desteklenen ürün yeteneği bir profilde `unsupported` denilerek kapatılamaz; ilgili paket
fallback'iyle aynı public semantiği sağlamalıdır. macOS host testi Windows/Linux runtime kanıtı,
cross-check GPU/present kanıtı ve unit test golden/checksum yeniden-baseline kanıtı sayılmaz. Eksik
runtime profili paket sonucunu `ertele` yapar; “tümü geçti” diye raporlanmaz.

## 9. Tamamlama tanımı

Bir madde ancak şu koşulların tümüyle kapanır:

- consumer sınırı ve kazanç aynı fixture'da ölçülmüş;
- consumer public alternatifi uygulanmış veya orantısız maliyeti ölçülmüş;
- güncel upstream/parite kararı ve yerel fark varsa koddan önce açılmış `SAPMALAR.md` kaydı mevcut;
- API/ABI, alpha/renk, capability, limit, hata, bellek ve device-loss sözleşmesi dokümante;
- CPU referans ile altı runtime profili kabul edilmiş; eksik profil açıkça `ertele`dir;
- hedef p95/byte/RSS kazanımı onaylı bütçeyi karşılıyor ve ölçüm gürültüsünden ayrılıyor;
- normal karede beklenmeyen readback yok; beklenen readback adı/byte'ı telemetride görünür;
- R0 ise diagnostics kapalı maliyeti bütçe içinde ve GPU/present adları gerçek semantiğe dayanıyor;
- ilgili feature, shader/layout, corpus ve regresyon testleri geçiyor;
- `yetenek.md`, `EXTRACTION.md`, karar kaydı ve gerekiyorsa `SAPMALAR.md` güncel;
- GPUI uygulama commit'i tek dilim; unrelated baseline/W30/plan değişikliği içermiyor;
- bırakma koşulu sonraki upstream sync'te otomatik ya da açık doğrulama adımıyla yeniden denetlenebilir;
- Canvas2 entegrasyonu aynı pinlenmiş GPUI commit'iyle yeniden ölçülmüş ve fallback'i doğrulanmış.

Bu koşullardan biri eksikse sonuç “kısmen tamamlandı” değil, ilgili kapı sonucu olan `ertele`,
`consumer'da kapat` veya `reddet`tir.

## 10. Olgun planın ilk icra kararı

Plan uygulamaya hazır, fakat runtime/public API değişikliği henüz yetkili değildir. İlk icra adımı
Canvas2'de G1a fixture manifestini ve karar matrisini üretmektir. Yalnız bu ölçüm mevcut GPUI
diagnostics'in attribution için yetersiz kaldığını gösterirse P0/R0 tasarımı açılır. Ardından G1b ve
G2 tamamlanır; ilk GPUI yetenek dilimi ölçümle seçilir. W30 ve R7 bu kritik yolun dışında kalır.
