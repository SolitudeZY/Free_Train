# 采用 Tauri、Svelte、Rust 和精简 FFmpeg

Free-Train 使用 Tauri 2 和 Svelte/TypeScript 构建由 Windows WebView2 渲染的现代桌面界面，Rust 负责应用服务、任务执行、元数据和图片处理，并随安装包分发精简的 FFmpeg/ffprobe。该方案替代 PySide6，因为同时打包 Python、Qt、OpenCV 和 FFmpeg 会让小型预处理工具达到约 150-250 MB；当前方案在保持现代界面和后续 MCP 复用边界的同时，将标准安装包目标控制在约 40-70 MB。

