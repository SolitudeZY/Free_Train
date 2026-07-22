# Free-Train

Free-Train 是一个面向计算机视觉数据准备的 Windows 本地离线预处理工具。它处理未标注的视频和图片，提供视频抽帧、人工筛查、ROI 与固定尺寸切图、客观质量筛选、视觉近重复审核、可选数据增强、自由命名模板和可追溯导出。

当前状态：M2 已实现视频人工筛查、有效片段和自动抽帧纵向流程。详见 [M2 实现报告](./docs/M2-REPORT.md)。

## 文档索引

- [领域语言](./CONTEXT.md)
- [产品需求文档](./docs/PRD.md)
- [软件架构设计](./docs/ARCHITECTURE.md)
- [界面与交互设计](./docs/UI-DESIGN.md)
- [实施计划](./docs/IMPLEMENTATION-PLAN.md)
- [测试计划](./docs/TEST-PLAN.md)
- [M0 技术验证报告](./docs/M0-REPORT.md)
- [M1 实现报告](./docs/M1-REPORT.md)
- [M2 实现报告](./docs/M2-REPORT.md)
- [MCP 接口预留设计](./docs/MCP-DESIGN.md)
- [架构决策记录](./docs/adr/)

## 已确认边界

- Windows 10/11 本地桌面工具，单用户、内部使用。
- 只处理标注前素材，不负责标注、数据划分、模型训练或推理。
- 源素材默认原地引用且永不被修改或删除。
- 快速处理运行到人工审核阶段，最终导出必须确认。
- 桌面 MVP 不开放 MCP Server，但从首版保留独立应用服务边界。

## 开发命令

本机需要 Rust stable、Node.js、Corepack 和 WebView2。pnpm 通过 Corepack 调用，不要求全局安装垫片。

```powershell
corepack pnpm install
corepack pnpm check
cargo test --workspace
corepack pnpm --dir apps/desktop tauri dev
```

M1 使用方式：

1. 从顶部项目菜单新建或打开一个 `.ftproj` 项目目录。
2. 点击“导入”选择文件/目录，或把视频、图片和目录拖入窗口。
3. 在左侧来源树选择素材，中间预览，右侧查看来源、媒体与指纹信息。
4. 素材移动或被替换后点击左侧刷新按钮；离线素材可通过“重新定位”恢复引用。

M2 视频筛查：

1. 选择在线视频，等待实际帧时间轴加载完成。
2. 使用底部播放、逐帧、速度和时间跳转控件定位目标。
3. 点击相机按钮或按 `C` 保存当前帧；人工候选默认锁定。
4. 按 `I` 设置入点、按 `O` 设置出点，创建一个或多个有效片段。
5. 在右侧“抽帧”页选择固定时间、固定帧间隔、目标数量、有效片段或画面变化模式。
6. 先执行预估，再执行抽帧；候选图片显示在底部缩略图条。

生成 release EXE：

```powershell
corepack pnpm --dir apps/desktop tauri build --no-bundle --ci
```

准备 LGPL FFmpeg 运行时：

```powershell
.\scripts\fetch-ffmpeg-lgpl.ps1
```
