# GPUI Yetenek Manifestosu — AI Ajanı İçin

```yaml
belge_turu: agent_capability_manifest
hedef_tuketici: kod_ureten_ve_depo_duzenleyen_ai_ajani
dil: tr
kutuphane: GPUI
surum: 0.2.3
rust_edition: 2024
rust_toolchain: 1.95.0
upstream_commit_upstream_md: 259297035a3fd64be4fb36042c229f59f074e38b
upstream_commit_notice: 259297035a3fd64be4fb36042c229f59f074e38b
provenance_status: consistent_repository_metadata
upstream_sync_date: 2026-07-30
durum: pre_1_0_unofficial_standalone_extraction
ana_crate: gpui
platform_giris_crate: gpui_platform
dogrulama_hedefi: cargo_check_workspace
son_dogrulama_platformu: x86_64_linux
son_dogrulama: cargo_fmt_check_locked_workspace_check_test_gpui_wgpu_bench_ve_wasm_multithread_ve_no_default_check
parite_kurali: AGENTS.md
```

## 0. Ajan yürütme sözleşmesi

Bu belgeyi GPUI tabanlı görevlerde karar ve uygulama girdisi olarak kullan.

Normatif anahtar sözcükler:

- `ZORUNLU`: ihlal etme.
- `ÖNERİLEN`: aksi için somut teknik gerekçe yoksa uygula.
- `KAÇIN`: yalnızca zorunlu olduğunda kullan.
- `DESTEKLENMEZ`: bu çalışma alanının sağladığı yetenek olarak varsayma.

Temel karar kuralları:

1. Uygulama başlatmak için doğrudan platform crate'i seçmek yerine `gpui_platform::application()` kullan.
2. Durum taşıyan ve tekrar render edilen UI için `Entity<T> + Render + Context<T>` kullan.
3. Durumsuz, tek kullanımlık UI parçası için `RenderOnce` veya doğrudan element ağacı kullan.
4. Genel yerleşim ve etkileşim için önce `div()` seç; özel çizim gerektiğinde `canvas`, `PathBuilder` veya düşük seviye `Element` uygula.
5. Uzun veri kümelerinde bütün çocukları üretme; değişken yükseklik için `list`, sabit/tekdüze öğeler için `uniform_list` kullan.
6. Entity mutasyonunu yalnızca GPUI context'leri üzerinden yap. `Entity<T>` iç verisini bağımsız sahiplenilen normal Rust verisi gibi ele alma.
7. Render sonucunu değiştiren mutasyondan sonra `cx.notify()` çağır.
8. Event/observation aboneliğini canlı tutmak için dönen `Subscription` değerini bir owner alanında sakla.
9. Foreground GPUI görevlerinde `cx.spawn`/`window.spawn`, `Send` arka plan işi için `cx.background_spawn` veya `BackgroundExecutor`, Tokio gerektiren iş için `gpui_tokio` kullan.
10. Platforma özel kod yazmadan önce `gpui_platform` ve `App`/`Window` üzerindeki soyutlanmış servisi ara.
11. Klavye davranışını ham key event ile sabitlemek yerine `Action + KeyBinding + key_context + on_action` hattını kullan.
12. Test edilebilir davranış üret; uygun görevlerde `#[gpui::test]`, `TestAppContext` ve sentetik input yeteneklerini kullan.
13. Bu depo Zed UI kitini, Zed temalarını veya editör bileşenlerini içermez. Bunların varlığını varsayma.
14. API pre-1.0'dır. Bellekten imza üretmek yerine yerel kaynakta sembol ve imzayı doğrula.
15. Bu depoda GPUI özelliği, davranışı, API'si, platform düzeltmesi veya optimizasyonu üretme.
16. Bir GPUI kod değişikliği kayıtlı Zed revizyonunda yoksa burada uygulama; önce Zed'e ekle, sonra
    upstream senkronizasyonuyla birebir aktar.

## 1. Sistem modeli

```text
gpui_platform::application()
  -> Application::run(|cx: &mut App|)
    -> App::open_window(WindowOptions, builder)
      -> Result<WindowHandle<ViewModel>>
        -> Entity<ViewModel> (window root view)
        -> Render::render(&mut self, &mut Window, &mut Context<Self>)
          -> element ağacı
            -> layout
            -> prepaint/hitbox
            -> GPU scene/paint
```

GPUI üç seviyeyi birlikte sunar:

```yaml
durum_katmani:
  primitive: Entity<T>
  erisim: App | Context<T> | AsyncApp | AsyncWindowContext
  amac: sahiplik, mutasyon, observation, event, global durum

deklaratif_ui:
  primitive: Render
  sonuc: IntoElement
  amac: view modelden her frame element ağacı üretmek

dusuk_seviye_ui:
  primitive: Element
  fazlar: request_layout -> prepaint -> paint
  amac: özel layout, hit testing, sanallaştırma, doğrudan çizim
```

Render modeli hibrittir:

- View/entity durumu retained'dır.
- `render` çağrısında element ağacı deklaratif olarak yeniden kurulur.
- Elementler layout/prepaint/paint yaşam döngüsünü yönetir.
- GPU sahnesi quad, gölge, underline, glyph/sprite, yüzey ve path primitive'lerinden oluşur.

## 2. Minimum uygulama üretim kalıbı

```rust
use gpui::{
    App, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

struct RootView {
    label: SharedString,
}

impl Render for RootView {
    fn render(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(rgb(0x20242b))
            .text_color(rgb(0xffffff))
            .child(self.label.clone())
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(480.), px(240.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| RootView { label: "GPUI".into() }),
        )
        .expect("window creation failed");
        cx.activate(true);
    });
}
```

Cargo bağımlılık seçimi:

```toml
[dependencies]
gpui = { path = "crates/gpui" }

[dependencies.gpui_platform]
path = "crates/gpui_platform"
features = ["font-kit", "wayland", "x11"]
```

Yukarıdaki `path = "crates/..."` biçimi bu deponun dışındaki/altındaki bağımsız consumer manifesti
için örnektir. Bu deponun sanal workspace kök manifestindeki `[workspace.dependencies]` kayıtlarını
kullanan yeni bir workspace member'ında doğru biçim:

```toml
[dependencies]
gpui.workspace = true
gpui_platform = { workspace = true, features = ["font-kit", "wayland", "x11"] }
```

`gpui_platform` default feature kümesi boştur. Yukarıdaki çapraz-platform feature'ları özellikle
belirtilmelidir; aksi durumda macOS backend'i `NoopTextSystem` seçer ve metin layout edilse bile
glyph çizilmez.

Tek platform için feature azaltma:

```yaml
macos:
  renderer: Metal
  gpui_platform_features: [font-kit]
  not: gpui_platform/font-kit yoksa NoopTextSystem glyph çizmez
linux_freebsd:
  renderer: WGPU
  gpui_platform_features: [wayland, x11] # en az biri
windows:
  renderer: DirectX
  text: DirectWrite
  gpui_platform_features: []
web_wasm:
  platform: gpui_web
  init: gpui_platform::web_init
  optional_entry: gpui_platform::single_threaded_web
  default_feature: gpui_web/multithreaded
  derleme: atomics target feature'ları ve wasm_thread nightly özelliği gerekir
```

`single_threaded_web()` constructor'ının varlığı Cargo feature'ını kapatmaz. Kayıtlı upstream
revizyonda hem varsayılan `multithreaded` hem de `--no-default-features` yolu derlenir; önceki
snapshot'ta bulunan `shared_memory_supported()` gate hatası upstream'de düzeltilmiştir. Varsayılan
konfigürasyonun derlenmesi için atomics hedef feature'ları ve bootstrap gerekir, çünkü `wasm_thread`
nightly `stdarch_wasm_atomic_wait` özelliğini kullanır. Atomics açıkken `parking_lot_core`'un
nightly gate'i de açılır; `--no-default-features` yolunda ikisi de kapanır ve stable kanal yeter.

`gpui_platform` wasm hedefinde `gpui_web`'i varsayılan feature'larla çeker, yani `multithreaded`
o crate üzerinden gelen tüketicide kapatılamaz. Tek iş parçacıklı yolu isteyen tüketici
`gpui_platform` yerine doğrudan `gpui_web`'e bağlanabilir: `single_threaded_web()` ve `web_init()`
karşılıkları `WebPlatform::new(false)`, `fetch_http_client()` ve `init_logging()` üzerine ince bir
sarmalayıcıdır. Bu, depoda değişiklik gerektirmez.

Buradaki eksikler için varsayılan yol düzeltmeyi önce Zed'e sokmaktır. Tüketici tarafında
çözülemeyen veya oradaki maliyeti orantısız kalan durumlarda `AGENTS.md`'deki bilinçli sapma
süreci işletilir ve sapma `SAPMALAR.md`'ye kaydedilir.

Web'de hem `application()` hem `single_threaded_web()` platformun Fetch tabanlı HTTP client'ını
kurar; ayrıca `with_http_client` çağırman gerekmez.

## 3. Ana yetenek kataloğu

### 3.1 Uygulama ve yaşam döngüsü

```yaml
giris:
  - gpui_platform::application()
  - gpui_platform::headless()
  - gpui_platform::background_executor()
  - gpui_platform::current_platform(headless)
ana_tipler:
  - Application
  - ApplicationHandle
  - App
  - AppLifecyclePhase
  - QuitMode
yetenekler:
  - event loop başlatma
  - uygulamayı aktive/deaktive etme
  - pencere açma ve yönetme
  - quit/relaunch davranışı
  - URL açma
  - dosya/dizin prompt'ları
  - clipboard
  - menü ve platform aksiyonları
  - global durum ve servis kaydı
  - foreground/background executor erişimi
  - sistem görünümünü anlık okuma; pencere appearance değişimini Window üzerinden izleme
  - sistem bildirimi
```

`Application::run` callback'i ana `App` erişim noktasıdır. UI state ve GPUI servisleri ana thread/context disiplini içinde yönetilmelidir.

### 3.2 Entity, context, sahiplik ve reaktivite

Ana tipler:

- `Entity<T>`: `App` tarafından sahiplenilen typed state handle.
- `WeakEntity<T>`: sahipliği uzatmayan handle; async callback ve döngü kırmak için kullan.
- `AnyEntity`, `AnyView`, `AnyWeakView`: type-erased entegrasyon.
- `EntityId`: identity.
- `Reservation<T>`: entity yaratılmadan ID ayırma ve daha sonra insert etme.
- `Context<T>`: entity read/update sırasında entity-özel yetenekler sağlar ve `App`'e deref eder.
- `App`: global root context.
- `AsyncApp`, `AsyncWindowContext`: `await` boyunca tutulabilen fallible context.
- `Window`: pencere durumu ve frame/paint/input servisi.
- `WindowHandle<V>`, `AnyWindowHandle`: pencereye dışarıdan typed/type-erased erişim.
- `Global`: tipe göre tekil uygulama durumu.

Entity işlem yetenekleri:

```yaml
create:
  - cx.new(|cx| T)
  - cx.reserve_entity()
  - cx.insert_entity(reservation, builder)
read:
  - entity.read(cx)
  - cx.read_entity(&entity, callback)
update:
  - entity.update(cx, callback)
  - cx.update_entity(&entity, callback)
  - cx.as_mut(&entity)
reactivity:
  - cx.notify()
  - cx.observe(&entity, callback)
  - cx.observe_new::<T>(callback)
  - cx.on_release(callback)
events:
  - impl EventEmitter<E> for T
  - cx.emit(event)
  - cx.subscribe(&entity, callback)
global:
  - cx.global::<G>()
  - cx.try_global::<G>()
  - cx.global_mut::<G>()
  - cx.set_global(value)
  - cx.read_global(...)
  - cx.update_global(...)
  - cx.update_default_global(...)
  - cx.observe_global::<G>(callback)
```

`global`/`global_mut` değer yoksa panic eder; opsiyonel okuma için `try_global` kullan.
`observe_global` bir `Subscription` döndürür ve owner üzerinde saklanmalıdır.

`observe_new` belirli türde yeni entity yaratılmasını, `on_release` entity yaşam döngüsü sonunu
gözler. Mevcut effect döngüsünün sonuna iş bırakmak için `App::defer`; çizim döngüsüne bağlı iş için
`Window::request_animation_frame` kullan.

Entity sahibi callback kalıbı:

```rust
div().on_action(cx.listener(
    |this: &mut Model, action: &MoveUp, window: &mut Window, cx: &mut Context<Model>| {
        this.apply(action, window, cx);
        cx.notify();
    },
))
```

Element callback'leri doğrudan `&mut Model` veya `Context<Model>` vermez. `on_action` ham
listener imzası `Fn(&A, &mut Window, &mut App)` biçimindedir. Bir `Context<T>` içindeyken
`cx.listener(...)`, callback'i entity update ile sarar ve kanonik `&mut T` erişimini sağlar.

Kendi view/entity tipini tanımlamadan frame'ler arası yerel element durumu:

- `Window::use_state`: çağrı konumuna bağlı yerel state.
- `Window::use_keyed_state`: anahtara bağlı yerel state.
- Her ikisi de arka planda `Entity<S>` oluşturup döndürür; entity sistemini atlamaz.
- Birden çok aynı tür child/view render ediliyorsa ID scope çakışmalarını önlemek için benzersiz
  element/view kimliği kullan.

Seçim:

```yaml
domain_ve_paylasilan_ui_state: Entity<T>
uygulama_geneli_servis_state: Global
elemente_ozel_gecici_state: Window::use_state veya Window::use_keyed_state
```

Sahiplik kuralları:

- Entity verisini `Arc<Mutex<T>>` ile ikinci bir state sistemi haline getirmekten KAÇIN; GPUI entity/context modelini kullan.
- Callback içinde güçlü `Entity` yakalamak yaşam döngüsü döngüsü yaratıyorsa `downgrade()` kullan.
- Async context/entity/window erişiminin başarısız olabileceğini `Result`/`Option` olarak işle.
- Aynı entity üzerinde iç içe çakışan lease/borrow oluşturma.

### 3.3 View ve render

```yaml
traits:
  Render:
    girdi: "&mut self, &mut Window, &mut Context<Self>"
    cikti: impl IntoElement
    kullanim: stateful view
  RenderOnce:
    girdi: "self, &mut Window, &mut App"
    kullanim: tek kullanımlık değer/komponent
  IntoElement:
    kullanim: element ağacına dönüşüm
  ParentElement:
    kullanim: child/children ekleme
  Element:
    kullanim: özel request_layout/prepaint/paint
  View:
    kullanim: render edilebilir entity abstraction
derive_macrolari:
  - gpui::Render
  - gpui::IntoElement
```

View state'i değiştiğinde repaint için `cx.notify()` gereklidir. Yalnızca dış servis değiştiren ve render çıktısını etkilemeyen işlemlerde gereksiz notify üretme.

### 3.4 Elementler ve layout

Hazır element seçim tablosu:

| Hedef | Primitive | Seçim koşulu |
|---|---|---|
| Genel container/layout | `div()` / `Div` | Varsayılan seçim |
| Metin | `SharedString`, `Text`, `StyledText`, `InteractiveText` | Stil, highlight veya metin etkileşimi |
| Raster görüntü | `img(source)` / `Img` | asset, path/URI veya render image |
| SVG | `svg()` / `Svg` | vektör asset |
| Özel çizim | `canvas(prepaint, paint)` | imperative paint callback |
| Path | `PathBuilder` + scene path | fill/stroke/tessellation |
| Değişken boyutlu sanal liste | `list(...)` | çok sayıda farklı yükseklikli satır |
| Tekdüze sanal liste | `uniform_list(...)` | sabit/tahmin edilebilir öğe ölçüsü |
| Pencereye ankrajlı içerik | `anchored()` | popover/menu/tooltip konumlama |
| Parent ölçüsüne bağlı dal | `container_query(...)` | responsive alt ağaç |
| Gecikmeli paint sırası | `deferred(child)` | overlay/z-order ihtiyacı |
| Native/external surface | `surface(source)` | Yalnızca macOS `CVPixelBuffer` compositing |
| Animasyon | `with_animation` / `AnimationExt` | zaman tabanlı element dönüşümü |
| Görüntü cache'i | `image_cache(...)`, `retain_all(...)` | async/tekrarlı image yükleme |

`Div`/`Styled` yetenek aileleri:

```yaml
layout:
  display: [flex, grid, block, hidden]
  sizing: [width, height, size, min, max, full, auto]
  spacing: [margin, padding, gap]
  flex: [direction, wrap, grow, shrink, basis]
  alignment: [items, content, self, justify]
  grid: [grid_cols, grid_rows, row, column, placement, grid_cols_min_content, grid_cols_max_content, grid_rows_min_content, grid_rows_max_content]
  positioning: [relative, absolute, top, right, bottom, left]
  overflow: [hidden, overflow_x_hidden, overflow_y_hidden]
visual:
  - background/fill/gradient/pattern
  - border/radius
  - shadow
  - opacity
  - visibility
  - cursor
text:
  - family
  - size
  - weight
  - style
  - color
  - alignment
  - white-space
  - overflow/truncation
  - underline/strikethrough/highlight
interaction_states:
  - hover
  - active
  - focus
  - focus_visible
  - drag_over
  - group
```

`active` pointer/press state stilidir. `focus`, `in_focus` ve `focus_visible` stillerinin çalışması
için elementin bir `FocusHandle` izlemesi veya `focusable()` olması gerekir.

GPUI grid API'si serbest CSS track listeleri, `fr` veya genel `minmax(...)` builder'ı sağlamaz;
`grid_cols(u16)`/`grid_rows(u16)`, placement ve hem kolon hem satır için
`grid_cols_min_content`/`grid_cols_max_content` ile
`grid_rows_min_content`/`grid_rows_max_content` primitive'leriyle sınırlıdır. Bu min/max sizing
seçeneklerinin taşıyıcı tipi `GridTemplateMinSize`'dır; önceki `TemplateColumnMinSize` adı
kaldırılmıştır. `overflow_visible()` metodu yoktur; visible varsayılan overflow değeridir.

`.id(...)`, `Stateful<Div>` üretir. Yalnızca bu aşamadan sonra açılan başlıca yetenekler:
`on_click`, `on_drag`, `on_hover`, tooltip, `overflow_scroll`/`track_scroll`, `focusable`,
`active` ve ARIA. Buna karşılık focus stilleri (`focus`, `in_focus`, `focus_visible`) ile
`on_drop`, `on_drag_move` ve `drag_over`
`InteractiveElement` üzerinde düz `Div` ile de kullanılabilir:

```rust
div()
    .id("results")
    .overflow_scroll()
    .on_click(cx.listener(|this, event, window, cx| {
        this.handle_click(event, window, cx);
    }))
```

`ElementId` on varyanta sahiptir: `View`, `Integer`, `Name`, `Uuid`, `FocusHandle`,
`NamedInteger`, `Path`, `CodeLocation`, `NamedChild`, `OpaqueId`. Yeniden sıralanabilir listelerde
kararlı kimlik kullan.

Koşullu fluent element üretimi için prelude'den gelen `FluentBuilder` kullan:

```rust
div()
    .when(is_selected, |element| element.bg(selected_color))
    .when_some(subtitle, |element, subtitle| element.child(subtitle))
    .map(|element| decorate(element))
```

Bu yöntem builder tipini korur; gereksiz `if`/`match` dallarıyla farklı opaque element tipleri
üretmekten kaçınır. Dışarıdan geçerli import yolu yalnızca
`gpui::prelude::FluentBuilder`'dır; `gpui::util` private modüldür ve `gpui_util` farklı bir
crate'tir.

Ölçü ve geometri primitive'leri:

- `px`, `Pixels`, `DevicePixels`, `ScaledPixels`
- `rems`, `Rems`
- `relative(f32)` → `DefiniteLength::Fraction` (yüzdesel/parent-relative uzunluk)
- `percentage(f32)`, `Percentage` (rotasyon/transform oranı; layout yüzdesi değildir)
- `Length`, `AbsoluteLength`, `DefiniteLength`
- `Point<T>`, `Size<T>`, `Bounds<T>`, `Edges<T>`, `Corners<T>`
- `point`, `size`, `bounds`
- `Axis`, `Anchor`, `Radians`
- `AvailableSpace`, `LayoutId`

Layout Taffy tabanlıdır. Stil API'si utility/Tailwind benzeri fluent zincir sunar; CSS DOM değildir.

Liste API ayrımı:

```yaml
list:
  init: ListState::new(item_count, ListAlignment, overdraw)
  imza_kavrami: list(state: ListState, render_item)
  state: ListState
  yetenekler:
    - splice
    - scroll_to
    - scroll_to_reveal_item
    - reset
    - visible range ve scroll event
uniform_list:
  imza_kavrami: uniform_list(id, item_count, render_range)
  state: UniformListScrollHandle
  scroll: scroll_to_item(ix, ScrollStrategy)
  not: ListState kabul etmez
```

### 3.5 Etkileşim, focus ve input

`InteractiveElement` ve `StatefulInteractiveElement` ile:

```yaml
mouse:
  - on_mouse_down
  - on_mouse_up
  - on_click
  - on_mouse_move
  - on_hover(|&bool, ...|)
  - on_mouse_exit
  - on_scroll_wheel
keyboard:
  - on_key_down
  - on_key_up
  - on_action
  - key_context
focus:
  - track_focus
  - focusable
  - Focusable::focus_handle
  - App::focus_handle
drag_drop:
  - on_drag
  - on_drop
  - on_drag_move
  - external_drag_payload
  - file drop events
touch_gesture:
  - touch event tipleri
  - pinch
  - platform gesture abstraction
state:
  - hover style
  - active style
  - group style
scroll:
  - ScrollHandle
  - ScrollAnchor
```

Input event tipleri mouse, keyboard, touch, pressure, scroll, pinch, file drop ve platform inputlarını
kapsar. Touch tipleri ile `LongPressEvent` tanımlıdır fakat çekirdek touch/tap/long-press kayıt ve
dispatch hattı henüz uygulanmamıştır. `ClickEvent::Touch` üretimi yoktur; backend'ler
`Platform::gestures()` override etmez ve yalnızca `NullPlatformGestures` implementasyonu bulunur.
Pinch uçtan uca uygulanmıştır fakat `PinchEvent: MouseEvent` olduğundan trackpad/mouse dispatch
hattından akar.

Event dispatch:

- `DispatchPhase::{Capture, Bubble}` sırasını ayırt et.
- Element düzeyinde `capture_*` listener varyantlarını capture gereksiniminde kullan.
- `cx.stop_propagation()` (`App`) sonraki propagation'ı keser.
- `cx.propagate()` (`App`) açıkça devam ettirir.
- `window.prevent_default()` varsayılan davranışı engeller.
- `on_mouse_event` bir element builder metodu değil, `Window` seviyesinde düşük seviye listener'dır.

Metin girişleri/IME için:

- `EntityInputHandler`
- `ElementInputHandler<V>`
- `InputHandler`
- `PlatformInputHandler`
- UTF-16 selection dönüşümleri
- composition marked text
- selection bounds ve replace işlemleri

Kendi text field/editor bileşenini yazarken yalnızca key events kullanma; IME/input handler sözleşmesini uygula.

Dosya sürükle-bırak kabulünde `ExternalPaths` değerini kullan; platformdan gelen bir veya daha
fazla `PathBuf` için `SmallVec<[PathBuf; 2]>` taşır; koleksiyon boş da olabilir.

Pencere dışına dosya sürüklemek için `on_drag` çağrısından sonra `external_drag_payload(...)`
kaydet; resolver `ExternalDragPayload::Files(FileDragPaths::new(...))` döndürür ve `FileDragPaths`
her yolu dizin olup olmadığı bilgisiyle eşler. Resolver bir sürükleme jesti başına en fazla bir kez,
işaretçi viewport'tan çıktığında çağrılır; `on_drag` ile aynı sürüklenen değer tipini kullanmak
zorunludur. Platform tarafı yalnızca macOS'ta uygulanmıştır (`PlatformWindow::can_start_external_drag`
diğer backend'lerde `false` döner), bu yüzden bunu taşınabilir bir yetenek olarak varsayma.

`ClickEvent`, `Keyboard | Mouse | Touch` varyantlı bir enum'dur; ortak alanları yoktur. Doğrudan
`event.position` yazma. Ortak erişim için `position()`, `modifiers()`, `click_count()`,
`mouse_position()`, `is_right_click()`, `is_secondary()`, `standard_click()` ve `is_keyboard()`
metotlarını kullan. Mevcut runtime'da `Touch` varyantının üretilmediği sınırlamasını koru.

Focus gözlemi:

- `Context::on_focus`
- `Context::on_focus_in`
- `Context::on_blur`
- `Context::on_focus_out`
- `Context::on_focus_lost`

Bu metodların döndürdüğü `Subscription` değerlerini owner üzerinde sakla.

### 3.6 Action, keymap ve klavye dispatch

Action tanımı:

```rust
gpui::actions!(menu, [MoveUp, MoveDown]);

// veya alan taşıyan action:
#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
struct Move {
    direction: Direction,
    select: bool,
}
```

Bağlama hattı:

```rust
div()
    .key_context("menu")
    .on_action(cx.listener(
        |this: &mut Model, action: &MoveUp, window: &mut Window, cx: &mut Context<Model>| {
            this.move_up(action, window, cx);
            cx.notify();
        },
    ))
```

`KeyBinding` değerlerini uygulama başlangıcında kaydet. Context predicate'leriyle aynı tuşu farklı
UI bölgelerinde farklı action'a bağla. Action kimliği `namespace::Name` string'idir; namespace
verilmezse yalnızca struct adıdır. Bu kimliği Rust'ın fully-qualified type adı olarak varsayma.
Unit olmayan action'lar `serde::Deserialize` ile birlikte `schemars::JsonSchema` gerektirir.
Bu tip uygulama crate'inde tanımlanıyorsa `schemars` bağımlılığını da manifestte erişilebilir yap.
JSON ile oluşturulmayacak action için `#[action(no_json)]` kaçışını değerlendir. `Debug`, action
derive'ının zorunlu koşulu değildir.
`NoAction` ve `Unbind` keymap override semantiği sağlar.

Key binding kayıt kalıbı:

```rust
cx.bind_keys([
    KeyBinding::new("up", MoveUp, Some("menu")),
    KeyBinding::new("down", MoveDown, Some("menu")),
]);
```

- Kayıt çağrısının sahibi `App::bind_keys`'dir (`cx: &mut App` üzerinde çağrılır).
- `KeyBinding::new(keystrokes, action, context)` parse hatasında panic eder.
- Fallible/dinamik yükleme için `KeyBinding::load(...)` kullan; keyboard mapper, context predicate,
  key-equivalent ve action input parametrelerini alır.

Action keşfi ve komut paleti:

- `Window::available_actions`
- `Window::bindings_for_action`
- `Window::bindings_for_action_in_context`
- registered action introspection
- focused dispatch path için action availability sorguları

Karar:

```yaml
yeniden_eslenebilir_komut: Action
metin_girisi: InputHandler
ham_fiziksel_event_gerekiyorsa: KeyDownEvent/KeyUpEvent
platform_menu_komutu: action + app menu
```

### 3.7 Pencere yönetimi

Yetenekler:

- Typed root view ile window açma/değiştirme.
- `WindowOptions`, `WindowBounds`, `TitlebarOptions`, `WindowKind`.
- Pencere boyutu, konumu, display ve scale factor.
- `minimize_window`, `zoom_window`, `toggle_fullscreen`, activate/bring-to-front.
- Maximized açılış için `WindowBounds::Maximized`; sorgu için `Window::is_maximized`.
- Decorations, titlebar, traffic-light/window controls.
- Focus yönetimi ve focus zinciri.
- Cursor style: `Window::set_cursor_style(style, &Hitbox)`.
- Cursor hide mode: `App::set_cursor_hide_mode`.
- Hitbox, tooltip, prompt.
- Input dispatch ve action dispatch.
- Frame isteme, refresh, animation.
- Odaksız pencere enerji tasarrufu için yaklaşık 30 FPS'e sınırlanır; yüksek hızlı input
  (ör. pointer altındaki odaksız pencereyi kaydırma) algılanırken sınırlama kaldırılır ve
  presentation sürdürülür.
- Drag/drop.
- Clipboard işlemleri `App` üzerindedir; `Window` API'si değildir.
- Window appearance/background appearance; appearance değişimini
  `Window::observe_window_appearance` ile izleme.
- Text rendering mode setter: `App::set_text_rendering_mode`.
- Pop-up ve Linux layer-shell abstraction.
- Raw window/display handle ile renderer entegrasyonu.

`Window` değerini uzun süre saklama. Dışarı taşınan referans yerine `WindowHandle` sakla ve context ile `update` et.

Ek pencere/UI servisleri:

- Tooltip: stateful element üzerinde `.tooltip(...)` ve `hoverable_tooltip(...)`.
- Prompt: `PromptLevel`, `PromptButton`, `Window::prompt`; özel UI için
  `App::set_prompt_builder`/`reset_prompt_builder`.
- Tipli menü: `Menu` ve `MenuItem`.
- Menü kaydı: `App::set_menus(iter_of_menu)`.
- Tab navigasyonu: `FocusHandle::tab_stop`/`tab_index` ve
  `Window::focus_next`/`Window::focus_prev`.
- Keystroke gözlemi: `App::observe_keystrokes`, `App::intercept_keystrokes`.
- macOS sistem window tabbing: `SystemWindowTab`, tab listeleme/seçme/ayırma işlemleri.
- Client-side decoration/hit testing: `WindowControlArea`.

### 3.8 Metin sistemi

Yetenekler:

```yaml
font:
  - aile ve fallback çözümleme
  - FontWeight
  - FontStyle
  - font features
  - font metrics
layout:
  - TextRun
  - line layout
  - line wrapping
  - truncation
  - glyph positioning
  - Unicode BiDi paragraf ayırıcılarında paragraf başına güvenli shaping
render:
  - glyph rasterization
  - subpixel/monochrome rendering
  - platform text backend
```

İlgili tipler: `TextSystem`, `WindowTextSystem`, `Font`, `FontId`, `FontFamilyId`, `FontMetrics`, `TextRun`, `LineLayout`, `LineWrapper`, `TruncateFrom`, `RenderGlyphParams`.

macOS'ta görünür glyph için `font-kit` feature'ını koru. Web/SVG fallback için depoda IBM Plex Sans ve Lilex varlıkları bulunur.

WGPU/cosmic-text hattı, aynı layout satırındaki farklı yönlere sahip Unicode BiDi paragraflarını
paragraf ayırıcılarında bölerek ayrı şekillendirir ve glyph byte indekslerini/konumlarını tekrar
birleştirir. `LF`, `CR`, `U+001C`–`U+001E`, `U+0085` ve `U+2029` bu kapsamdadır. Consumer kodunun
çökmeyi önlemek için bu karakterleri önceden silmesi veya tek yöne zorlaması gerekmez.

### 3.9 Renk, görüntü, SVG ve GPU sahnesi

Renk:

- `rgb(0xRRGGBB)`, `rgba(0xRRGGBBAA)`
- `Rgba`, `Hsla`, `hsla`, `opaque_grey`
- `Background`
- solid, linear gradient, slash pattern, checkerboard
- color-space ve alpha dönüşümleri
- varsayılan `Colors`/`GlobalColors`

`hsla` ve `opaque_grey` `const fn` olduğundan sabit renk tablolarında ve diğer const bağlamlarda
doğrudan kullanılabilir; girdiler yine `[0, 1]` aralığına clamp edilir.

Asset/görüntü:

- `AssetSource`: uygulama asset sağlayıcısı.
- `Application::with_assets(asset_source)`: asset source'u uygulamaya bağlar; `svg()`/`img()` ile
  uygulama asset'i kullanmadan önce çağır.
- `Asset` trait: yükleme kaynağı ve çıktı tipini tanımlar.
- `App::fetch_asset` / `App::remove_asset`: asset cache erişimi ve invalidation.
- `Window::use_asset`: render akışında asset kullanımı.
- `ImageSource`: path/URI/bytes/render image kaynakları.
- BMP, DDS, EXR, Farbfeld, GIF, HDR, ICO, JPEG, PNG, PNM, QOI, TGA, TIFF ve WebP decoder feature'ları derlemede mevcuttur.
- EXIF orientation işleme kodu bulunur.
- `ImageCache`/`ImageCacheProvider` özel cache stratejisine izin verir.
- `ImageSource::Custom` uygulamaya özel görüntü kaynağı sağlar.

Decoder format listesi `img()` resource yükleme hattına aittir. `ImageFormat` enum'u aynı listeyi
temsil etmez ve dokuz formatla sınırlıdır.

SVG:

- `Svg`, `SvgRenderer`, `RenderSvgParams`, `SvgSize`
- resvg/usvg tabanlı render
- transformation ve recolor/stil seçenekleri

Düşük seviye sahne:

- `Scene`
- `Primitive`, `PrimitiveBatch`
- `Quad`, `Shadow`, `Underline`
- monochrome/subpixel/polychrome sprite
- external `PaintSurface`
- `Path`, `PathVertex`, `PathBuilder`
- fill/stroke options, fill rule, transform
- draw order, content mask ve transformation matrix

`Window` doğrudan paint ailesi:

- `paint_layer`
- `paint_drop_shadows` / `paint_inset_shadows`
- `paint_quad`
- `paint_path`
- `paint_underline` / `paint_strikethrough`
- `paint_glyph` / `paint_emoji`
- `paint_svg`
- `paint_image`
- macOS'ta `paint_surface`

Özel grafik için seçim:

```yaml
basit_dikdortgen_border_shadow: div veya PaintQuad
dinamik_2d_cizim: canvas
vektor_geometri: PathBuilder
ikon: svg
raster: img
renderer_platform_entegre_yuzey: surface
```

`surface()` bu snapshot'ta yalnızca macOS'ta derlenir ve kaynağı `CVPixelBuffer` ile sınırlıdır.

Animasyon easing kataloğu `ease_in_out`, `bounce` ve `pulsating_between` yardımcılarını da içerir.
Kullanımdan önce tam easing imzasını `elements/animation.rs` içinde doğrula.

### 3.10 Async, task ve zamanlama

```yaml
foreground:
  api:
    - cx.spawn(async move |cx| ...)
    - window.spawn(cx, async move |cx| ...)
    - ForegroundExecutor::spawn_when_idle(timeout, future)
    - ForegroundExecutor::idle_time_remaining()
  ozellik: GPUI context erişimi
background:
  api:
    - cx.background_spawn(future)
    - BackgroundExecutor::spawn
  kosul: "Future + Send + 'static"
timers:
  api:
    - executor.timer(duration)
    - Task
task_control:
  - await
  - detach
  - Task drop edildiğinde örtük iptal
  - TaskExt
priority:
  - scheduler::Priority
```

Ana thread'de boşta zaman işi için `ForegroundExecutor::spawn_when_idle(timeout, future)` kullan.
`timeout` verilmezse platform meşgul kaldığı sürece poll süresiz ertelenebilir; verilirse o süreyi
aşan poll sıradan ana thread işi olarak koşar. Her poll tek bir idle diliminin parçasını harcar, bu
yüzden uzun senkron bölümleri `idle_time_remaining()` ile sınırla ve aralarda yield et. Metrelenmiş
idle desteği yalnızca web backend'inde vardır; diğer platformlarda çağrı düşük öncelikli spawn'a
düşer, `timeout` yok sayılır ve `idle_time_remaining()` `None` döner.

Tokio adaptörü:

```rust
gpui_tokio::init(cx);
let task = gpui_tokio::Tokio::spawn(cx, async move {
    // Tokio I/O
});
```

- Var olan runtime için `init_from_handle`.
- `Tokio::spawn` dönen GPUI task drop edilirse Tokio task iptal edilir.
- `Tokio::spawn_result` anyhow sonucu köprüler.

Async güvenlik:

- UI thread'e dönmeden entity state'ini doğrudan değiştirme.
- Uzun işte `WeakEntity` yakala; tamamlanınca upgrade/update başarısızlığını işle.
- `Task` drop semantiğini kontrol et; fire-and-forget isteniyorsa açıkça `.detach()`.
- `cancel_on_drop` adında bir API yoktur; iptal `Task` drop semantiğidir.
- Bloklayan I/O'yu foreground executor üzerinde çalıştırma.
- Birden çok child future'ı yapılandırılmış concurrency ile yürütmek için
  `BackgroundExecutor::scoped` yeteneğini değerlendir.

### 3.11 Platform servisleri

Soyutlanmış servisler:

- macOS, Linux/FreeBSD, Windows ve WASM platform seçimi.
- Display enumerasyonu ve display metadata.
- Native window oluşturma.
- Clipboard: text, image ve `ExternalPaths` (dosya yolu koleksiyonu).
- URL/path açma.
- Dosya/dizin seçici prompt.
- Uygulama menüsü.
- Credentials depolama/okuma.
- Sistem bildirimi ve notification action response.
- Appearance, thermal state ve lifecycle callback'leri.
- Keyboard layout/mapping.
- Screen capture source/stream (`screen-capture` feature).
- Linux Wayland/X11 seçimi, portal entegrasyonu, layer shell ve popup.
- Headless platform/test dispatcher.

`AppLifecyclePhase` public bir tiptir fakat mevcut backend'ler bunu etkin bir lifecycle akışına
bağlamaz; `Platform::on_app_lifecycle` varsayılanı no-op'tur. Uygulama mantığını bu callback'in
çalışacağı varsayımına bağlama.

Platform backend matrisi:

| Hedef | Pencere | Render | Metin | Erişilebilirlik |
|---|---|---|---|---|
| macOS | AppKit/Cocoa | Metal | font-kit/CoreText | AccessKit macOS |
| Linux/FreeBSD | Wayland ve/veya X11 | WGPU | platform/WGPU text | AccessKit Unix |
| Windows | Win32 | DirectX | DirectWrite | AccessKit Windows |
| WASM | Web platform | web backend | web text backend | backend sınırlarına bağlı |

Bu extraction içinde yalnızca macOS build/test doğrulanmıştır. Diğer hedeflerin kaynak backend'leri vardır fakat bu snapshot için cross-compile doğrulaması yapılmamıştır.

### 3.12 Erişilebilirlik

GPUI AccessKit tiplerini re-export eder:

- `accesskit`
- `AccessibleAction`
- `Role`
- `Orientation`
- `Toggled`

Pencere a11y ağacı, node identity, focus ve accessible action callback altyapısı içerir.
Stateful element (`.id(...)` sonrası) üzerinde ARIA builder API'sini kullan:

```rust
div()
    .id("save")
    .role(Role::Button)
    .aria_label("Save")
    .on_a11y_action(AccessibleAction::Click, |data, window, cx| {
        // data: Option<&accesskit::ActionData>
    })
```

Mevcut builder ailesi role, label/description, value, selected/expanded ve
`aria_toggled(Toggled::{True, False, Mixed})` gibi semantik özellikleri ve accessible action
callback'ini kapsar. `aria_checked` metodu veya ayrı checked alanı yoktur. Etkileşimli özel
elementte focus, tab stop ve action eşlemesini birlikte sağla. Yalnızca görsel/click davranışı
erişilebilir bileşen sayılmaz.

### 3.13 Test, property test, benchmark ve gözlemleme

Feature'lar:

```yaml
test-support:
  saglar:
    - TestAppContext
    - public TestDispatcher ve TestScreenCaptureSource/TestScreenCaptureStream
    - sentetik input
    - clock ilerletme
    - run_until_parked
    - proptest re-export
bench:
  saglar:
    - test-support
    - Criterion entegrasyonu
    - BenchAppContext
    - BenchWindowContext
    - BenchReport
```

Makrolar:

- `#[gpui::test]`
- `#[gpui::property_test]`
- `#[gpui::bench]`
- `gpui::bench_group!`
- `gpui::bench_main!`

Test yetenekleri:

- Entity oluşturma/read/update.
- Observer ve emitted event doğrulama.
- Key/mouse/dispatch simülasyonu.
- Focus ve window davranışı.
- Deterministik scheduler clock.
- Background/foreground task sürme.
- `Observation<T>` ve `observe`.
- Headless/test platform.
- `Window::render_to_image` ile render edilmiş sahneyi RGBA image olarak alma.
- macOS visual/offscreen test context; AppKit ana thread sınırlamalarını dikkate al.

`gpui_wgpu` içindeki `layout_line` Criterion benchmark'ı hem paragraf ayırıcısı içermeyen yaygın
hızlı yolu hem de BiDi paragraf ayırıcılı karışık yönlü şekillendirme yolunu ölçer:

```sh
cargo bench -p gpui_wgpu --bench layout_line
```

Başlıca `TestAppContext` çağrıları:

- `add_window(builder)` veya boyut kontrollü `open_window(size, builder)`
- `add_window_view`
- `simulate_keystrokes(window, "cmd-p escape")`
- `simulate_input(window, "text")`
- `dispatch_keystroke`
- `run_until_parked`

`VisualTestContext` window handle'ı içinde tuttuğu için `simulate_keystrokes("...")` biçiminde daha
kısa bir overload sağlar. Test-only platform/window/display implementasyonlarının çoğu
`pub(crate)`'tir; public API olarak yalnızca export edilen test tiplerini varsay.

Değişiklik sonrası minimum doğrulama:

```sh
cargo check --workspace
cargo test --workspace
cargo check -p gpui-hello-world
```

GUI açılması otomatik test oracle'ı olarak tek başına kullanılmamalıdır.

### 3.14 Inspector ve profiler

Feature-gated yetenekler:

```yaml
inspector:
  feature: inspector
  amac: element/style reflection ve UI inceleme
  not: debug build'de inspector feature olmadan da etkinleştirilen cfg yolları vardır
profiler:
  feature: profiler
  amac: task, frame ve thread performans ölçümü
input_latency:
  feature: input-latency-histogram
  amac: input latency histogram/snapshot
  api: Window::input_latency_snapshot() -> InputLatencySnapshot
  snapshot:
    clone: true
    alanlar:
      - latency_histogram
      - events_per_frame_histogram
      - mid_draw_events_dropped
leak_detection:
  feature: leak-detection
  amac: entity yaşam döngüsü/backtrace desteği
screen_capture:
  feature: screen-capture
runtime_shaders:
  feature_owner: gpui_platform -> gpui_macos
  amac: macOS Metal shader'larını runtime'da derleme/yükleme
```

İlgili derive/makrolar `gpui_macros` içindedir: `AppContext`, `VisualContext`, `Render`, `IntoElement`, `Action`, inspector reflection, `styles`, test/property-test/bench.

### 3.15 İleri yetenek ve sembol dizini

Bu bölüm, kategori haritasından adı tahmin edilemeyen fakat özel bileşen veya platform entegrasyonu
yazarken gereken, bu snapshot'ın yerel kaynağında public tanımı doğrulanmış yüzeyi indeksler.
Kullanım sırasında tam imzaları yine Bölüm 9 rotalarından doğrula.

#### Özel `Element` uygulama araç seti

```yaml
kalici_element_state:
  - Window::with_element_state
  - Window::with_optional_element_state
identity:
  - GlobalElementId
hit_testing:
  - Window::insert_hitbox
  - Hitbox
  - HitboxBehavior
layout:
  - Window::request_measured_layout
  - AnyElement::layout_as_root
  - AnyElement::prepaint_at
paint_context:
  - Window::defer_draw
  - Window::with_content_mask
  - Window::with_element_offset
```

Özel element `request_layout -> prepaint -> paint` fazlarında state'i `GlobalElementId` ile
anahtarla, interaktif alan için hitbox üret, ölçüme bağlı layout'u measured-layout callback'iyle
çöz ve parent clip/offset bağlamını koru.

#### Input handler bağlama

`Window::handle_input(focus_handle, EntityInputHandler, cx)`, IME/text input handler'ını aktif
pencere ve focus handle'a bağlayan çağrıdır. Yalnızca `EntityInputHandler` implement etmek yeterli
değildir; elementin paint/prepaint akışında handler'ı pencereye kaydet.

#### Uygulama ve pencere yaşam döngüsü

```yaml
app:
  - Context::on_app_quit
  - Context::on_app_restart
  - App::on_window_closed
window:
  - Window::on_window_should_close
application:
  - Application::on_open_urls
  - Application::on_reopen
  - Application::on_system_wake
  - Application::run_embedded
```

Çıkışta state kaydetme, restart öncesi cleanup ve window close veto/confirmation için bu callback
ailesini kullan. `on_app_quit` future'larının tamamlanması için sınırlı bir kapanış süresi vardır.

#### Subscription, entity ve boş view yardımcıları

- `Subscription::detach`: aboneliği owner alanında saklamadan kalıcılaştırır; otomatik unsubscribe
  isteniyorsa kullanma.
- `Entity::read_with`, `Entity::update_in`, `Entity::write`.
- `WeakEntity::read_with`, `WeakEntity::update`.
- `Entity::cached` ve `AnyView::cached`: view render sonucunu style refinement ile cache'leme.
- `Empty` ve `EmptyView`: içeriksiz element/root view.

#### Görüntü durumları

`StyledImage` prelude trait'i şu builder'ları sağlar:

- `with_loading`: async yükleme placeholder'ı.
- `with_fallback`: yükleme hatası fallback'i.
- `object_fit(ObjectFit::{Contain, Cover, Fill, ScaleDown, None})`.
- `grayscale(bool)`.

#### Zengin ve etkileşimli metin

- `StyledText::with_highlights` / `with_default_highlights`.
- `StyledText::with_runs`.
- `HighlightStyle` ve `combine_highlights`.
- `TextLayout::index_for_position`.
- `TextLayout::position_for_index`.
- `InteractiveText::on_click` / `on_hover`.

Pointer koordinatı ile metin byte/index konumu arasında dönüşüm için `TextLayout` kullan; font
ölçüsünü elle tahmin etme.

#### Anchored, deferred ve animasyon

```yaml
anchored:
  - snap_to_window
  - snap_to_window_with_margin
  - AnchoredFitMode
  - AnchoredPositionMode
deferred:
  - Deferred::with_priority
animation:
  - Animation::new
  - Animation::repeat
  - Animation::with_easing
  - AnimationExt::with_animation
  - AnimationExt::with_animations
```

Dekoratif animasyondan önce `App::reduce_motion()` kontrol et. Kullanıcı/OS tercihini test etmek
veya uygulamak için `App::set_reduce_motion`.

#### Liste ve scroll davranışları

```yaml
list_state:
  - set_follow_mode
  - scroll_to_end
  - ListSizingBehavior
  - ListHorizontalSizingBehavior
  - scrollbar drag API'leri
uniform_list:
  - with_decoration
  - y_flipped
  - with_width_from_item
scroll_handle:
  - offset
  - set_offset
  - scroll_to_item
  - logical_scroll_top
```

Log/chat tail-follow için `ListState::set_follow_mode` ve `scroll_to_end`; ters akış için
`UniformList::y_flipped` kullan.

#### Kısayol gösterimi ve key dispatch introspection

- `Window::keystroke_text_for`: action için platforma uygun görünen kısayol etiketi.
- `Modifiers::secondary()`: macOS Command / diğer platformlarda Control semantiği.
- `Keystroke::parse` ve `Keystroke::unparse`.
- `Window::has_pending_keystrokes`.
- `Window::possible_bindings_for_input`.
- `Window::context_stack`.
- `KeyContext::set`: typed/context property kaydı.

Key context predicate söz dizimi ve dispatch modeli için `crates/gpui/docs/key_dispatch.md` ile
`crates/gpui/docs/contexts.md` dosyalarını birlikte oku.

#### Client-side decoration ve window chrome

```yaml
csd:
  - Window::request_decorations
  - Window::window_controls
  - Window::start_window_move
  - Window::start_window_resize
  - Window::set_client_inset
  - Window::set_input_region
  - ResizeEdge
  - Tiling
  - WindowInsets
metadata_attention:
  - set_window_title
  - set_window_edited
  - set_document_path
  - request_attention
  - play_system_bell
```

#### Overlay, dış tıklama ve drag/drop

- `on_mouse_down_out` / `on_mouse_up_out`: popover dışı dismiss.
- `occlude` / `block_mouse_except_scroll`: alttaki hit target'ları engelleme.
- `on_modifiers_changed`.
- `on_aux_click`.
- `can_drop`.
- `DragMoveEvent<T>`.

#### Rem, text-style ve pixel snapping

- `Window::rem_size`, `set_rem_size`, `with_rem_size`.
- `Window::with_text_style`.
- Pixel hizalama/snap yardımcıları: GPU ölçeğinde keskin sınır ve glyph/quad konumlama.

#### Async ve executor ekleri

```yaml
senkron_kopru:
  - gpui::block_on
  - ForegroundExecutor::block_on
  - ForegroundExecutor::block_with_timeout
timeout:
  - FutureExt::with_timeout
  - Timeout
spawn:
  - spawn_in
  - spawn_with_priority
  - Window::defer
  - Context::defer_in
structured_concurrency:
  - Scope
  - BackgroundExecutor::scoped
  - BackgroundExecutor::scoped_priority
```

#### Genişletilmiş test yüzeyi

```yaml
test_app_context:
  - condition
  - next_event
  - next_notification
  - simulate_prompt_answer
  - expect_restart
visual:
  - VisualTestAppContext::capture_screenshot
  - open_offscreen_window
executor_determinism:
  - advance_clock
  - forbid_parking
  - simulate_random_delay
contexts:
  - TestApp
  - HeadlessAppContext
```

#### Masaüstü ve macOS entegrasyon uçları

- Dock/recent/jump-list: `set_dock_menu`, `perform_dock_menu_action`, `add_recent_document`,
  `update_jump_list`.
- Ek panolar: `read_from_primary`, find pasteboard API'leri.
- Uygulama kimliği/yolları: `register_url_scheme`, `set_app_identity`, `app_path`,
  `path_for_auxiliary_executable`.
- Headless renderer seçimi: `gpui_platform::current_headless_renderer` (`test-support`).

## 4. Çalışma alanındaki yardımcı crate'ler

### `gpui_platform`

Platform koşullu crate seçimini merkezileştirir. Uygulama, headless uygulama, background executor ve current platform constructor sağlar.

### `gpui_macos`

AppKit pencere/event loop, Metal renderer, clipboard, display link, macOS media ve sistem servisleri.

### `gpui_linux`

Wayland, X11 ve headless client; WGPU renderer; clipboard, cursor, XIM/keyboard, portal, layer shell, popup ve sistem bildirimi.

### `gpui_windows`

Win32 window/event, DirectX renderer/atlas/shader, DirectWrite, clipboard, direct manipulation, destination list, system settings/notifications ve vsync.

### `gpui_web`

WASM/web platform, browser window/display/events, keyboard, dispatcher, HTTP client ve logging.
Default feature `multithreaded`'dır ve atomics hedef ayarlarını gerektirir. Fetch tabanlı HTTP
client arka plan worker'larından da çalışır ve platform tarafından varsayılan olarak kurulur.
Dispatcher, ana thread için idle zamanlaması (`spawn_when_idle`) sağlayan tek backend'dir.

### `gpui_wgpu`

WGPU context, renderer, atlas, WGSL shader'lar ve cosmic text sistemi. Özellikle Linux/web render
altyapısının parçasıdır. Cosmic text adapter'ı, farklı yönlerdeki birden çok Unicode BiDi
paragrafını tek `LineLayout` içinde güvenli biçimde şekillendirir.

### `gpui_tokio`

GPUI task cancellation semantiğiyle Tokio runtime/spawn köprüsü.

### `gpui_shared_string`

UI metinlerinde ucuz clone ve paylaşım için `SharedString`. Dinamik label ve child text için varsayılan owned string tipi olarak değerlendir.

### `gpui_util`

`ArcCow`, deferred cleanup ve genel yardımcı altyapı. `FluentBuilder` burada değildir; public
consumer import yolu `gpui::prelude::FluentBuilder`'dır.

### `collections`

Standart koleksiyon re-export'ları ve sıralı/küçük koleksiyon optimizasyonları; `VecMap` dahil.

### `scheduler`

GPUI foreground/background task yürütmesinin çekirdeği:

- `Task`, `FallibleTask`
- local executor
- background executor
- priority
- clock/timer
- test scheduler

### `sum_tree`

Büyük sıralı veri üzerinde özet bilgili dengeli ağaç:

- `SumTree`
- `Item`, `Summary`, `Dimension`
- cursor/seek
- edit/append
- `TreeMap`

Editör buffer'ı, satır index'i, sanal görünüm veya prefix-summary gerektiren yüksek hacimli veride kullan. Basit küçük liste için kullanma.

### `refineable` ve `derive_refineable`

Struct değerlerini kısmi override/refinement modeliyle birleştirme. Stil/konfigürasyon katmanları ve default + override kompozisyonu için uygundur.

### `http_client`

Async HTTP soyutlaması:

- `HttpClient` trait
- async body
- request/response işlemleri
- proxy ve redirect davranışları
- GitHub yardımcı kodları

Bu extraction'da Zed'e özel `github-download` entegrasyonu çıkarılmıştır.

### `media`

Platform media binding'leri ve media erişim yardımcıları. Uygulama UI primitive'i değildir; platform backend bağımlılık closure'ının parçasıdır.

### `gpui_macros`

Proc macro katmanı:

- render/element derive
- action kayıt ve derive
- app/visual context derive
- style metod üretimi
- inspector reflection
- GPUI test/property test/benchmark

## 5. Feature matrisi

`gpui` feature'ları:

| Feature | Etki |
|---|---|
| `default` | `font-kit`, `wayland`, `x11`, `windows-manifest` |
| `test-support` | test context, leak detection, collections ve HTTP client test support, Wayland/X11, proptest |
| `bench` | test support + Criterion + histogram |
| `inspector` | inspector macro/reflection; debug cfg yolları ayrıca feature'sız etkin olabilir |
| `leak-detection` | backtrace ile leak teşhisi |
| `wayland` | `gpui` cfg yollarını/public API'leri açar; gerçek backend bağımlılıkları `gpui_linux/wayland` üzerinden gelir |
| `x11` | `gpui` cfg yollarını ve `scap?/x11` hattını açar; gerçek backend `gpui_linux/x11` üzerinden gelir |
| `screen-capture` | `scap` tabanlı ekran yakalama |
| `windows-manifest` | Windows resource manifest embed |
| `input-latency-histogram` | latency histogram |
| `profiler` | profiling altyapısı |

`wayland`/`x11` salt etiketten ibaret değildir; örneğin feature-gated `Window::set_exclusive_edge`
gibi public yolları da etkiler. `gpui` manifestindeki örtük `font-kit` feature'ı bu extraction'da
platform font backend'ini etkinleştirmez. macOS için gerekli gerçek forwarding `gpui_platform/font-kit ->
gpui_macos/font-kit` hattıdır. `gpui_platform` ayrıca `runtime_shaders` feature'ını
`gpui_macos/runtime_shaders` hedefine forward eder. Feature seçerken `gpui`,
`gpui_platform` ve hedef backend manifestlerindeki forwarding zincirini birlikte doğrula.
Cargo'nun opsiyonel bağımlılıklardan ürettiği örtük feature'lar `font-kit` ile sınırlı değildir;
`backtrace`, `scap` ve `proptest` gibi adları manifestten doğrula.

## 6. Görevden primitive'e karar tablosu

```yaml
stateful_component:
  sec: "struct + Entity<T> + impl Render"
stateless_component:
  sec: "RenderOnce veya IntoElement"
shared_application_service:
  sec: "impl Global + App global API"
parent_child_state_notification:
  sec: "observe + notify"
typed_domain_event:
  sec: "EventEmitter<E> + emit + subscribe"
keyboard_command:
  sec: "Action + KeyBinding + key_context + on_action"
text_editor_or_input:
  sec: "EntityInputHandler + IME composition"
small_layout:
  sec: "div + Styled"
huge_variable_rows:
  sec: "list + ListState"
huge_uniform_rows:
  sec: "uniform_list + UniformListScrollHandle"
popup_or_context_menu:
  sec: "anchored/deferred + focus + dismiss event"
custom_chart:
  sec: "canvas veya PathBuilder"
icon:
  sec: "svg"
photo_or_bitmap:
  sec: "img + ImageCache"
async_network:
  sec: "http_client + background task; sonucu entity update ile UI'a taşı"
tokio_library_integration:
  sec: "gpui_tokio"
deterministic_ui_test:
  sec: "#[gpui::test] + TestAppContext"
large_prefix_summarized_data:
  sec: "sum_tree"
theme_override_layers:
  sec: "Refineable"
platform_independent_entry:
  sec: "gpui_platform::application"
```

## 7. Önerilen uygulama mimarisi

```text
Application
├── App globals
│   ├── tema/ayar servisi
│   ├── HTTP veya domain servis handle'ları
│   └── opsiyonel Tokio runtime
├── WindowHandle<RootView>
│   └── Entity<RootView>
│       ├── child Entity<ModelA>
│       ├── child Entity<ModelB>
│       ├── Subscription alanları
│       ├── FocusHandle / ScrollHandle
│       └── Task alanları (owner yaşam döngüsüne bağlıysa)
└── Action/KeyBinding kaydı
```

Render fonksiyonunu mümkün olduğunca:

- deterministik,
- bloklamayan,
- yan etkisiz,
- eldeki state'ten element üreten

bir dönüşüm olarak tut. I/O ve uzun hesaplamayı task'a çıkar. Callback'lerde state mutate et, notify et ve gerekiyorsa event emit et.

## 8. Hata önleme kuralları

```yaml
yanlis:
  - "Entity<T>'yi T sanıp doğrudan alanlarına erişmek"
  - "Subscription sonucunu drop etmek"
  - "Task'ı yanlışlıkla drop ederek iptal etmek"
  - "mutasyondan sonra notify çağırmamak"
  - "render içinde bloklayan I/O yapmak"
  - "uzun listeyi children(iterator) ile tamamen materialize etmek"
  - "text input'u yalnızca keydown ile uygulamak"
  - "Window referansını callback ömrünün dışına taşımak"
  - "platform crate'lerine gereksiz cfg dallarıyla doğrudan bağlanmak"
  - "Zed'e ait UI/theme crate'lerinin bu workspace'te olduğunu varsaymak"
  - "Zed'de bulunmayan yerel GPUI davranışı, API'si veya feature'ı eklemek"
  - "FreeBSD/Windows/macOS backend'lerinin bu sync'te cross-compile edildiğini iddia etmek"
  - "WASM compile-check sonucunu browser runtime testi gibi sunmak"
  - "pre-1.0 API imzalarını kaynağı kontrol etmeden üretmek"
dogru:
  - "Entity update/read API"
  - "owner struct içinde Subscription/Task"
  - "notify/emit ayrımını bilinçli kullanmak"
  - "list/uniform_list sanallaştırması"
  - "EntityInputHandler ve IME"
  - "WindowHandle + context update"
  - "gpui_platform constructor"
  - "rg ile sembol doğrulama + cargo check"
```

## 9. Kaynak doğrulama rotaları

Bir API belirsizse aşağıdaki sırayı kullan:

```yaml
public_exports: crates/gpui/src/gpui.rs
prelude: crates/gpui/src/prelude.rs
application_entity_context: crates/gpui/src/app.rs ve crates/gpui/src/app/
render_contract: crates/gpui/src/element.rs
ownership_guide: crates/gpui/src/_ownership_and_data_flow.rs
accessibility_guide: crates/gpui/src/_accessibility.rs
context_guide: crates/gpui/docs/contexts.md
elements: crates/gpui/src/elements/
style_api: crates/gpui/src/style.rs ve crates/gpui/src/styled.rs
interaction: crates/gpui/src/interactive.rs ve crates/gpui/src/elements/div.rs
key_dispatch: crates/gpui/src/keymap.rs, crates/gpui/src/key_dispatch.rs, crates/gpui/docs/key_dispatch.md
window_platform: crates/gpui/src/window.rs ve crates/gpui/src/platform.rs
text: crates/gpui/src/text_system.rs ve crates/gpui/src/text_system/
async: crates/gpui/src/executor.rs ve crates/gpui_tokio/src/gpui_tokio.rs
tests: crates/gpui/src/test.rs ve crates/gpui/src/app/test_context.rs
platform_selection: crates/gpui_platform/src/gpui_platform.rs
features: crates/gpui/Cargo.toml ve crates/gpui_platform/Cargo.toml
provenance: UPSTREAM.md, EXTRACTION.md ve NOTICE birlikte
extraction_limits: EXTRACTION.md
working_example: examples/hello-world/src/main.rs
```

Sembol arama kalıbı:

```sh
rg 'pub (struct|enum|trait|fn) SYMBOL|impl .*SYMBOL' crates/
```

Derleme geri bildirimini API keşfinin bir parçası olarak kullan:

```sh
cargo check -p <degistirilen-paket>
cargo test -p <degistirilen-paket>
cargo check --workspace
```

## 10. Kapsam ve kesin sınırlar

Bu çalışma alanı şunları sağlar:

- Bağımsız derlenebilir GPUI çekirdeği.
- Dört platform ailesine ait kaynak backend'leri.
- GPU render, text, input, window, a11y ve async altyapısı.
- Tek bir bağımlılık-minimal hello-world örneği.
- GPUI'nin runtime/build dependency closure'ındaki yardımcı crate'ler.

Bu çalışma alanı şunları sağlamaz:

- Zed editörü.
- Zed'in hazır UI component kütüphanesi.
- Zed theme sistemi ve uygulama asset'leri.
- Cloud, collaboration, telemetry, project/workspace/language katmanları.
- Stabil API garantisi.
- Zed Industries desteği veya resmi standalone dağıtım statüsü.
- Bu sync için FreeBSD, Windows ve macOS cross-compile doğrulaması.
- WASM için browser runtime doğrulaması.
- Radial gradient primitive'i.
- Lottie renderer/oynatıcı.
- `SemanticVersion` adlı bir GPUI primitive'i.

Provenance:

```yaml
kaynak: https://github.com/zed-industries/zed
otorite: upstream Zed repository
bu_depo: unofficial extracted snapshot
commit_tutarliligi:
  UPSTREAM_md_EXTRACTION_md_ve_NOTICE: 259297035a3fd64be4fb36042c229f59f074e38b
  senkronizasyon_tarihi: 2026-07-30
lisans_ana: Apache-2.0
lisans_notu:
  - gpui_shared_string ve gpui_util upstream manifestlerinde lisans beyanı taşımıyor
  - yeniden dağıtım öncesi NOTICE ve EXTRACTION.md incele
```

## 11. Ajan tamamlama kontrol listesi

GPUI kod görevi bitmeden aşağıdakileri değerlendir:

```yaml
architecture:
  - state uygun Entity/Global içinde mi
  - Render/RenderOnce seçimi doğru mu
  - büyük listede sanallaştırma var mı
reactivity:
  - gereken mutasyonlar notify ediyor mu
  - event gereken yerde EventEmitter kullanılıyor mu
  - Subscription yaşam süresi korunuyor mu
async:
  - foreground/background ayrımı doğru mu
  - task drop/cancel davranışı bilinçli mi
  - weak handle gereken yerde kullanıldı mı
input:
  - keyboard command action üzerinden mi
  - text input IME uyumlu mu
  - focus ve accessibility tanımlı mı
platform:
  - gpui_platform abstraction kullanıldı mı
  - feature/target varsayımları doğrulandı mı
verification:
  - sembol imzaları yerel kaynaktan doğrulandı mı
  - ilgili paket cargo check geçti mi
  - davranış testi eklendi/çalıştırıldı mı
scope:
  - GPUI kod değişikliği kayıtlı Zed revizyonunda birebir var mı
  - yalnız standalone extraction uyarlamasıysa runtime/API/feature semantiğini değiştirmedi mi
  - Zed'e ait çıkarılmamış crate varsayımı var mı
  - extraction sınırları sonuçta doğru ifade edildi mi
```
