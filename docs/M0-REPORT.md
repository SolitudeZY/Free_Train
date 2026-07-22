# M0 技术验证报告

## 状态

- 日期：2026-07-22
- 结论：核心技术路线通过；NSIS 安装器工具缓存仍受外部下载超时影响
- 下一阶段：M1 项目、素材与基础预览

## 1. 已完成

### 1.1 工具链

- Rust `1.97.1`，MSVC 工具链。
- Node.js `24.14.1`。
- pnpm `10.14.0`，通过 Corepack 使用。
- WebView2 `146.0.3856.97`。
- Tauri `2.11.x`。
- Svelte 5 / SvelteKit / Vite。

Rust 官方分发站点在当前网络下长时间无响应，最终通过 `rsproxy.cn` 安装；该镜像只用于开发机工具链安装，不写入项目依赖配置。

### 1.2 Workspace

已建立：

- `apps/desktop`：Tauri 2 + Svelte 桌面应用。
- `crates/domain`：ROI、切片配置和任务状态领域类型。
- `crates/application`：应用服务 DTO 与健康状态。
- `crates/storage`：SQLite 初始迁移和探针。
- `crates/media`：FFmpeg/ffprobe 版本和媒体 JSON 适配器。
- `crates/image-pipeline`：切片规划、DCT pHash、全局 SSIM 和确定性亮度增强探针。
- `crates/job-engine`：任务状态机。

### 1.3 桌面工作台

- 现代三栏工作台空壳。
- 素材、处理、审核、导出和任务导航。
- 视频画布、时间轴和缩略图区稳定占位。
- 浅色/深色主题切换。
- Tauri 后端状态命令，显示 SQLite、FFmpeg、图片流水线、任务状态机和 WebView2 状态。
- 未实现业务命令保持禁用，不伪造处理结果。

### 1.4 数据与算法探针

- SQLite 内存数据库迁移成功。
- 100,000 条候选记录插入和尾页查询测试通过，测试耗时约 0.09 秒。
- ROI 和贴边切片坐标测试通过。
- DCT pHash 对相同图片产生相同哈希。
- 全局 SSIM 对相同图片结果为 1.0。
- 固定随机种子增强参数可复现。
- 任务合法路径和非法跳转测试通过。

### 1.5 媒体探针

生成并保存 `tests/fixtures/m0-sample.mp4`：

- 640×360。
- 30 FPS。
- 2 秒。
- H.264 High / YUV420P 视频流，可由 WebView2 HTML5 播放器直接播放。

Rust ffprobe 适配器正确读取视频尺寸、编码、帧率和时长。FFmpeg 在 1 秒位置成功导出 PNG 帧。

### 1.6 大列表探针

隐藏开发路由 `/m0/virtual-grid` 使用算术窗口化展示 100,000 个候选项，仅渲染当前视口和 4 行预取内容，不创建 100,000 个 DOM 节点。

## 2. 验证结果

- Rust 单元测试：11 个通过。
- Rust Clippy：通过，`-D warnings`。
- Rustfmt：通过。
- Svelte Check：0 错误，0 警告。
- 前端生产构建：通过。
- 前端静态资源：约 0.14 MiB。
- Windows release EXE：5.19 MiB。
- Release 进程启动：成功，窗口标题 `Free-Train`，响应正常。
- 启动后工作集：约 32.72 MiB。

本地浏览器自动化受到 Browser URL 安全策略限制，无法访问 `127.0.0.1` 完成截图检查；没有使用其他自动化表面规避该限制。Tauri 原生窗口已成功启动，但本轮没有自动截图证据。

## 3. FFmpeg 发行体积

开发机原有 Gyan full/GPL 静态构建：

- `ffmpeg.exe`：216.41 MiB。
- `ffprobe.exe`：216.21 MiB。

该构建不可用于 Free-Train 发行。

BtbN Windows x64 LGPL shared 候选：

- 完整下载 ZIP：63.95 MiB。
- 仅运行所需 EXE、DLL 和许可证：解压后 127.63 MiB。
- 仅运行集重新 ZIP：55.25 MiB。
- 加 Free-Train release EXE：约 60.44 MiB，符合 40-70 MiB 标准安装包目标。

该候选已完成实际媒体探测和抽帧验证。项目提供 `scripts/fetch-ffmpeg-lgpl.ps1`，二进制不提交到源码仓库。

## 4. 未完成与阻塞

### 4.1 NSIS 安装包

Tauri `2.11.4` bundler 固定下载 `nsis-3.11.zip`。当前网络在 Tauri 内部 120 秒全局超时前无法完成 GitHub 下载。系统已安装 NSIS `3.12`，但 Tauri 仍坚持自己的固定工具缓存，因此未生成最终 NSIS 文件。

该问题不影响：

- 前端构建。
- Rust 编译和测试。
- release EXE。
- Tauri 原生启动。
- 预估应用和 FFmpeg 总体积。

后续处理方式：在构建机预缓存 Tauri 指定的 NSIS 工具包，或在网络条件稳定的 CI 环境生成安装器。

## 5. M0 退出条件评估

| 条件 | 结果 |
|---|---|
| Windows 可启动桌面空壳 | 通过 |
| 不携带 Python、Qt、OpenCV、Node.js、Chromium | 通过 |
| 代表性视频探测和抽帧 | 通过 |
| Rust ROI、切片、pHash、SSIM、增强探针 | 通过 |
| SQLite 100,000 条分页 | 通过 |
| Svelte 100,000 项虚拟网格 | 代码与构建通过，浏览器自动截图受策略限制 |
| 40-70 MiB 发行体积可行 | 通过，估算约 60-65 MiB |
| NSIS 安装文件实际生成 | 外部下载阻塞 |

核心技术路线已达到进入 M1 的条件。安装器缓存问题作为构建环境任务跟踪，不阻塞项目与素材功能开发。
