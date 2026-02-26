# cam-mobile-pc

Usa la cámara trasera de tu Android como webcam virtual en Linux (Fedora) via USB.
Funciona con Zoom, Google Meet, Teams, Discord y cualquier app compatible con V4L2.

---

## Cómo funciona (flujo completo)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  ANDROID                                                                     │
│                                                                              │
│  CameraX (rear camera, 1280×720 @ 30fps)                                    │
│       │ YUV_420_888 frames                                                   │
│       ▼                                                                      │
│  CameraStreamer.kt                                                           │
│       │ convierte YUV → NV21 → JPEG (calidad 75)                            │
│       ▼                                                                      │
│  TcpServer.kt                                                                │
│       │ envuelve cada JPEG en un frame MIME multipart (MJPEG)               │
│       │ escucha en TCP :5000                                                 │
└───────┼──────────────────────────────────────────────────────────────────────┘
        │  USB (cable)
        │  adb forward tcp:5000 tcp:5000
        ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│  LINUX PC                                                                    │
│                                                                              │
│  ffmpeg                                                                      │
│       │ lee stream MJPEG desde tcp://localhost:5000                          │
│       │ convierte a yuyv422 (formato universal V4L2)                         │
│       ▼                                                                      │
│  /dev/video10  (v4l2loopback, label: "AndroidCam", exclusive_caps=1)        │
│       │                                                                      │
│       ▼                                                                      │
│  Zoom / Meet / Teams / Discord / Cheese → ven "AndroidCam" como webcam      │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Componentes Android

| Archivo | Rol |
|---|---|
| `MainActivity.kt` | UI, solicitud de permisos (CAMERA, POST_NOTIFICATIONS), arranca/para el servicio |
| `CameraStreamingService.kt` | ForegroundService tipo `camera`; orquesta TcpServer + CameraStreamer; muestra notificación persistente con botón Stop |
| `CameraStreamer.kt` | Vincula CameraX al ciclo de vida; recibe frames YUV_420_888; los convierte a NV21 y luego a JPEG; llama a `TcpServer.sendFrame()` |
| `TcpServer.kt` | Servidor TCP en puerto 5000; acepta un cliente a la vez; envuelve cada JPEG en un frame MIME multipart (`--frame\r\nContent-Type: image/jpeg\r\n...`) |

### Conversión de color en detalle

CameraX entrega frames en `YUV_420_888`. Para comprimirlos con `YuvImage` de Android se necesita `NV21`:

1. **Plano Y** se copia directo (luminancia completa).
2. **Plano UV** — hay dos casos:
   - `vPlane.pixelStride == 2` → el hardware ya entregó VU intercalado (semi-planar), se copia directo (**fast path**).
   - `pixelStride == 1` → se intercalan manualmente los bytes V y U (**slow path**).
3. `YuvImage.compressToJpeg()` comprime a calidad 75 (~3–8 Mbps según contenido).
4. Cada JPEG se envía como un frame del stream MJPEG sobre TCP.

### Transporte MJPEG sobre TCP

El protocolo es `multipart/x-mixed-replace`, el mismo que usan las IP cameras:

```
--frame\r\n
Content-Type: image/jpeg\r\n
Content-Length: <bytes>\r\n
\r\n
<jpeg data>
\r\n
--frame\r\n
...
```

ffmpeg lo lee con `-f mpjpeg` y lo vuelca directo en `/dev/video10`.

### Lado Linux

- **v4l2loopback** crea `/dev/video10` con `exclusive_caps=1`, lo que hace que el device se anuncie como cámara de captura (no de salida), igual a una webcam real.
- **ADB forward** tuneliza el puerto 5000 del PC al 5000 del celular por USB, sin necesidad de WiFi.
- **ffmpeg** actúa de bridge: desmultiplexa el MJPEG, convierte a `yuyv422` (formato compatible con todas las apps V4L2) y escribe en el device virtual.
- El script `start.sh` corre en loop: si ffmpeg muere (desconexión USB, app pausada), reestablece el ADB forward y reconecta automáticamente.

---

## Requisitos

- **Android:** 8.0+ (API 26), cámara trasera física
- **Linux:** Fedora (o cualquier distro con `dnf` / `akmod`); kernel con v4l2loopback
- **Cable USB** con datos (no solo carga)
- **Depuración USB** activada en el teléfono

---

## Setup (una sola vez)

### 1. Linux — instalar dependencias y crear el device virtual

```bash
cd linux
bash setup.sh
```

Instala `adb`, `ffmpeg`, `akmod-v4l2loopback` y persiste `/dev/video10` ("AndroidCam") en cada arranque.

### 2. Agregar tu usuario al grupo `video`

```bash
sudo usermod -aG video $USER
```

Luego cerrar sesión y volver a entrar (necesario para que el permiso tome efecto).

### 3. Android — compilar e instalar la app

```bash
cd android
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

O abrí `android/` en Android Studio y ejecutá directamente.

---

## Uso diario

**Orden correcto (importante):**

1. Conectar el teléfono por USB
2. En el PC, ejecutar:
   ```bash
   cd linux
   sh start.sh
   ```
3. En el teléfono: abrir **CamPC** → tocar **Start Streaming**
4. Abrir Zoom / Meet / Teams → Configuración → Video → Cámara → seleccionar **AndroidCam**

> Abrí la app de videoconferencia **después** de que ffmpeg esté conectado y recibiendo frames.
> Si ya la tenías abierta, reiniciala para que detecte el device.

---

## Estructura del proyecto

```
cam-mobile-pc/
├── android/
│   └── app/src/main/
│       ├── java/com/campc/
│       │   ├── MainActivity.kt            # UI + permisos + control del servicio
│       │   ├── CameraStreamingService.kt  # ForegroundService (tipo camera)
│       │   ├── CameraStreamer.kt          # CameraX + conversión YUV→NV21→JPEG
│       │   └── TcpServer.kt              # Servidor TCP + framing MJPEG
│       ├── res/layout/activity_main.xml
│       └── AndroidManifest.xml
├── linux/
│   ├── setup.sh    # Instala deps, carga v4l2loopback, persiste en boot
│   └── start.sh    # ADB forward + loop ffmpeg (auto-restart)
└── README.md
```

---

## Troubleshooting

| Síntoma | Causa probable | Solución |
|---|---|---|
| Cheese / app dice "No device found" | Device no existe o sin permisos | Verificar que el usuario está en el grupo `video`; reiniciar la app después de que ffmpeg conecte |
| ffmpeg: `Option reconnect not found` | Las opciones `-reconnect*` son solo para HTTP | Ya corregido en `start.sh`; el loop externo maneja la reconexión |
| Imagen verde / artefactos de color | Formato de pixel incompatible | Cambiar `yuyv422` → `rgb24` en `start.sh` |
| Alta latencia | ffmpeg con buffering grande | Las opciones `-probesize 32 -analyzeduration 0` ya están incluidas |
| "Device busy" en `/dev/video10` | Otro proceso lo tiene abierto | `fuser /dev/video10` para identificarlo y matarlo |
| La cámara Android falla silenciosamente | Falta `foregroundServiceType="camera"` | Verificar `AndroidManifest.xml` — ya está configurado |
| ADB forward falla al reconectar USB | Evento de desconexión USB | `start.sh` reintenta automáticamente; revisar cable y modo depuración USB |
| El module no carga al arrancar | Config de modprobe incorrecta | Re-ejecutar `setup.sh`; verificar `/etc/modprobe.d/v4l2loopback.conf` |

---

## Verificación rápida

```bash
# Verificar que el módulo está cargado y el device existe
lsmod | grep v4l2loopback
v4l2-ctl --list-devices

# Probar el device sin el celular (fuente sintética)
ffmpeg -f lavfi -i testsrc=size=1280x720:rate=30 -pix_fmt yuyv422 -f v4l2 /dev/video10 &
cheese   # debería ver el patrón de prueba

# Ver el stream MJPEG crudo desde el celular
nc localhost 5000 | head -c 300

# Ver qué proceso usa el device
fuser /dev/video10
```

---

## Decisiones de diseño

| Decisión | Elección | Por qué |
|---|---|---|
| Protocolo de video | MJPEG (MIME multipart) | Cada frame es un JPEG independiente; demuxer `mpjpeg` de ffmpeg built-in; tolerante a pérdida |
| Transporte | ADB forward por USB | Sin configuración de red; latencia baja y estable; funciona en cualquier entorno |
| Device virtual | v4l2loopback con `exclusive_caps=1` | Aparece como webcam real para Zoom, Meet, Teams (que filtran devices sin esta flag) |
| Encoding Android | CameraX + YuvImage | Más simple que MediaCodec; `STRATEGY_KEEP_ONLY_LATEST` previene acumulación de frames y OOM |
| Resolución / FPS | 1280×720 @ 30fps, calidad JPEG 75 | ~3–8 Mbps; entra cómodo en USB 2.0; suficiente para videoconferencias |
| Pixel format Linux | `yuyv422` | Formato más compatible con apps V4L2 (Zoom, Meet, OBS, etc.) |

---

## Mejoras futuras

- **H.264 sobre MPEG-TS**: reemplazar `CameraStreamer.kt` con `MediaCodec` H.264 + empaquetado MPEG-TS. En Linux cambiar `-f mpjpeg` → `-f mpegts`. Reducción de ~10× en ancho de banda.
- **WiFi opcional**: exponer el servidor TCP por WiFi además de USB para uso inalámbrico (mayor latencia).
- **Rotación automática**: detectar orientación del dispositivo y aplicar filtro `transpose` en ffmpeg.
