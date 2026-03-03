```
● All 8 files are in place. Here's a summary of what was created:

  windows/ crate — files created

  ┌────────────────────┬──────────┬──────────────────────────────────────────────────────────────────────────┐
  │        File        │  Status  │                           Key changes vs Linux                           │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ Cargo.toml         │ New      │ No libc, no x11 feature, added windows = "0.58"                          │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ src/adb.rs         │ Copied   │ Identical                                                                │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ src/discovery.rs   │ Copied   │ Identical                                                                │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ src/engine.rs      │ Copied   │ Identical                                                                │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ src/config.rs      │ Modified │ APPDATA path, no v4l2_device field                                       │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ src/main.rs        │ Modified │ mod virtual_cam instead of mod v4l2                                      │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ src/ffmpeg.rs      │ Modified │ VirtualCamWriter, taskkill, yuv420p_to_nv12() conversion, COM init guard │
  ├────────────────────┼──────────┼──────────────────────────────────────────────────────────────────────────┤
  │ src/virtual_cam.rs │ New      │ Full IMFVirtualCamera implementation                                     │
  └────────────────────┴──────────┴──────────────────────────────────────────────────────────────────────────┘

  Key implementation details in virtual_cam.rs

  - VirtualCamWriter::new() — calls MFStartup, creates AndroidCamSource (implements IMFMediaSource), creates the virtual camera with
  MFCreateVirtualCamera(Software, Session, CurrentUser, "AndroidCam"), attaches the source, and starts it.
  - write_frame(nv12) — if a RequestSample() token is pending, delivers the frame immediately as a MEMediaSample event; otherwise stores it
   for the next poll.
  - AndroidCamStream::RequestSample() — if a frame is available, fires MEMediaSample; if not, enqueues the token for write_frame() to
  satisfy later.
  - Format — NV12 1280×720 @30fps, announced via MFVideoFormat_NV12 media type.

  Build (on Windows)

  cargo build --release --manifest-path windows/Cargo.toml
  .\windows\target\release\campc.exe

✻ Sautéed for 13m 39s

❯  vale retorname una lista de todo lo neceasrio en windows 11 para que este programa funcione                                             
                                                                                                
● Requisitos para ejecutar campc.exe en Windows 11                                                                                          
                                                                                                                                            
  ---
  1. Sistema operativo                                                                                                                      
                                                             
  ┌─────────────────┬──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐   
  │    Requisito    │                                                     Detalle                                                      │   
  ├─────────────────┼──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
  │ Windows 11      │ Build 22621 o superior. IMFVirtualCamera no existe en versiones anteriores. El usuario tiene 25H2 (26200.x) —    │
  │ 22H2+           │ compatible.                                                                                                      │
  └─────────────────┴──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

  ---
  2. Para compilar (solo una vez)

  ┌───────────────────┬────────────────────────────────────────────────┬───────────────────────────────────────────────────────────────┐
  │    Herramienta    │                    Descarga                    │                             Notas                             │
  ├───────────────────┼────────────────────────────────────────────────┼───────────────────────────────────────────────────────────────┤
  │ Rust + MSVC       │ https://rustup.rs                              │ Instalar con target x86_64-pc-windows-msvc. Durante la        │
  │ toolchain         │                                                │ instalación de rustup, elegir "MSVC" (no GNU).                │
  ├───────────────────┼────────────────────────────────────────────────┼───────────────────────────────────────────────────────────────┤
  │ Visual Studio     │ https://aka.ms/vs/17/release/vs_BuildTools.exe │ Marcar: Desktop development with C++ → incluye MSVC compiler  │
  │ Build Tools       │                                                │ + Windows SDK. Alternativa: Visual Studio Community.          │
  └───────────────────┴────────────────────────────────────────────────┴───────────────────────────────────────────────────────────────┘

  Si ya tienes Visual Studio 2019/2022 instalado con C++, los Build Tools no hacen falta por separado.

  ---
  3. En tiempo de ejecución (en el PC)

  ┌─────────────────┬──────────────────────────────────────────────────────────────────────────────┬──────────────────────────────────┐
  │   Herramienta   │                                   Descarga                                   │          Necesario para          │
  ├─────────────────┼──────────────────────────────────────────────────────────────────────────────┼──────────────────────────────────┤
  │ FFmpeg          │ https://ffmpeg.org/download.html → "Windows builds by BtbN" →                │ Decodificar MJPEG y escalar a    │
  │ (ffmpeg.exe)    │ ffmpeg-master-latest-win64-gpl.zip                                           │ 720p. Debe estar en el PATH.     │
  ├─────────────────┼──────────────────────────────────────────────────────────────────────────────┼──────────────────────────────────┤
  │ ADB (Platform   │ https://developer.android.com/tools/releases/platform-tools                  │ Solo modo USB. En WiFi no hace   │
  │ Tools)          │                                                                              │ falta.                           │
  └─────────────────┴──────────────────────────────────────────────────────────────────────────────┴──────────────────────────────────┘

  Añadir FFmpeg al PATH

  1. Extraer el zip, copiar la carpeta a C:\ffmpeg\
  2. Configuración → Sistema → Variables de entorno → Path → Nuevo → C:\ffmpeg\bin
  3. Verificar: abrir PowerShell, escribir ffmpeg -version

  Verificar ADB (si usas USB)

  adb version
  adb devices   # con el móvil conectado y depuración USB activada

  ---
  4. En el móvil Android

  ┌─────────────────────────────┬─────────────────────────────────────────────────────────────────────┐
  │          Requisito          │                                Cómo                                 │
  ├─────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
  │ App CamPC instalada         │ adb install app-debug.apk o build desde Android Studio              │
  ├─────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
  │ Permiso de cámara concedido │ La app lo pide al primer arranque                                   │
  ├─────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
  │ Depuración USB activada     │ Solo modo USB: Ajustes → Opciones de desarrollador → Depuración USB │
  ├─────────────────────────────┼─────────────────────────────────────────────────────────────────────┤
  │ Misma red WiFi que el PC    │ Solo modo WiFi                                                      │
  └─────────────────────────────┴─────────────────────────────────────────────────────────────────────┘

  ---
  5. Firewall de Windows

  Windows Defender Firewall puede bloquear el beacon UDP de auto-descubrimiento. La primera vez que arranques campc.exe, Windows preguntará
   — dar permiso. Si no aparece el diálogo:

  # UDP :5001 (beacon auto-discovery)
  netsh advfirewall firewall add rule name="CamPC UDP discovery" protocol=UDP dir=in localport=5001 action=allow

  # TCP :5000 (stream WiFi directo — solo si el PC se conecta al móvil)
  netsh advfirewall firewall add rule name="CamPC TCP stream" protocol=TCP dir=in localport=5000 action=allow

  ---
  6. La cámara virtual "AndroidCam"

  No se necesita ningún driver de terceros. IMFVirtualCamera es nativa en Windows 11 22H2+.

  Verificar que aparece después de arrancar campc.exe y pulsar ▶ Iniciar:

  - Configuración → Privacidad y seguridad → Cámara → debe listar "AndroidCam"
  - En Zoom/Teams: Configuración → Vídeo → Cámara → seleccionar AndroidCam

  ---
  Resumen de pasos de instalación (orden recomendado)

  1. Instalar Visual Studio Build Tools (Desktop C++)
  2. Instalar Rust: https://rustup.rs (elegir MSVC)
  3. Instalar FFmpeg y añadir al PATH
  4. [Opcional USB] Instalar ADB Platform Tools y añadir al PATH
  5. Compilar: cargo build --release --manifest-path windows/Cargo.toml
  6. Ejecutar: .\windows\target\release\campc.exe
  7. En el móvil: abrir CamPC app → iniciar streaming
  8. En campc: ▶ Iniciar

  ---
  Nada de esto hace falta

  ┌─────────────────────────────────┬──────────────────────────────┐
  │              Cosa               │          Por qué no          │
  ├─────────────────────────────────┼──────────────────────────────┤
  │ OBS Virtual Camera              │ IMFVirtualCamera es nativa   │
  ├─────────────────────────────────┼──────────────────────────────┤
  │ VirtualCam / ManyCam / SplitCam │ Ídem                         │
  ├─────────────────────────────────┼──────────────────────────────┤
  │ v4l2loopback                    │ Solo Linux                   │
  ├─────────────────────────────────┼──────────────────────────────┤
  │ WSL                             │ No se usa                    │
  ├─────────────────────────────────┼──────────────────────────────┤
  │ Driver de cámara de terceros    │ No necesario en Win 11 22H2+ │
  └─────────────────────────────────┴────────────────────────────────┘
```
