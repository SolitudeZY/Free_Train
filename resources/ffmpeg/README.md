# FFmpeg runtime

The base repository does not commit FFmpeg binaries. Run the workspace script from PowerShell before packaging:

```powershell
.\scripts\fetch-ffmpeg-lgpl.ps1
```

The script downloads the BtbN Windows x64 LGPL shared build, validates that GPL mode is not enabled, and stages only:

- `ffmpeg.exe`
- `ffprobe.exe`
- required shared DLLs
- upstream `LICENSE.txt`

The staged runtime is ignored by version control. M0 measurements on 2026-07-22:

- Extracted runtime set: 127.63 MiB
- ZIP-compressed runtime set: 55.25 MiB
- Free-Train release executable without FFmpeg: 5.19 MiB

The projected standard installer remains approximately 60-65 MiB before installer metadata.

