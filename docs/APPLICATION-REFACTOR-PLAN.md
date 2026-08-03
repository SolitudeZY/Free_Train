# Application 模块拆分计划

## 状态

- 日期：2026-07-24
- 范围：`crates/application`
- 目标：提高领域实现的局部性和测试可定位性，不改变产品行为或 Tauri 调用接口
- 当前基线：`src/lib.rs` 接近 4,000 行，公开约 35 个函数和 35 个类型，包含项目、源素材、候选图片、审核、ROI、导出和抽帧等多组领域行为

## 1. 约束

1. 保持 `application` crate 作为桌面应用和未来 MCP Adapter 共用的应用层 seam。
2. 保持现有公开函数名、序列化字段、错误语义和 Tauri 命令参数不变。
3. 使用 `pub use` 维持当前扁平外部接口，调用方不因文件拆分而修改业务代码。
4. 不新增只有 SQLite 一个 Adapter 的 Repository trait；存储 seam 继续由 `storage::ProjectStore` 提供。
5. 不在同一阶段混入算法调整、数据库迁移或界面行为变化。
6. 每个阶段独立通过 `cargo fmt`、`cargo test --workspace`、前端检查和生产构建。

## 2. 目标结构

```text
crates/application/src/
├─ lib.rs          # 模块声明、稳定重导出和少量共享类型
├─ project.rs      # 项目生命周期、会话、清单和最近项目
├─ sources.rs      # 源素材导入、状态、重新定位、删除和指纹
├─ sampling.rs     # 人工抽帧、有效片段、候选图片和自动抽帧
├─ review.rs       # 质量评估、相似组、代表图和审核历史
├─ roi.rs          # ROI 配置和切片预览
├─ export.rs       # 导出计划、命名、执行和来源追溯记录
└─ error.rs        # 跨模块应用错误
```

模块文件可以拥有私有内部 seam，但不会向调用方暴露实现细节。领域类型优先与其行为放在同一模块；只有确实被多个模块共享的类型才保留在 `lib.rs` 或移入共享错误模块。

## 3. 分阶段实施

### 阶段 A：审核模块

- 移动质量评估、视觉近重复图、相似组、代表图、人工决定、锁定、撤销和重做实现到 `review.rs`。
- 将审核专属私有类型和辅助函数一起移动，避免形成只有转发函数的浅模块。
- 保留 `run_review_analysis`、`list_review_workspace`、`update_review_items`、`undo_review_action` 和 `redo_review_action` 的外部接口。
- 将审核测试放到审核模块附近，或保留跨领域导出测试在 crate 级测试中。

退出条件：删除 `review.rs` 后，审核复杂度不会散落回其他模块；审核调用方只依赖现有稳定接口。

### 阶段 B：ROI 与导出模块

- 将 ROI 配置和切片预览移动到 `roi.rs`。
- 将导出计划、命名模板、冲突处理、原子写入和来源追溯记录移动到 `export.rs`。
- ROI 与导出通过明确的领域类型协作，不相互读取私有实现。

退出条件：预览和正式导出继续共享同一切片规划实现，所有 M3 测试通过。

### 阶段 C：抽帧模块

- 将人工抽帧、有效片段、候选图片、抽帧规划、来源组抽帧和画面变化触发移动到 `sampling.rs`。
- 抽帧性能约束属于模块接口：长视频不展开完整帧时间轴，单帧使用快速 seek，计划最多 100,000 张。

退出条件：M2 真实 FFmpeg 测试、长视频规划回归测试和候选删除测试通过。

### 阶段 D：源素材与项目模块

- 将项目创建、打开、锁定、摘要和最近项目移动到 `project.rs`。
- 将源素材导入、离线检测、重新定位、删除、缩略图和指纹移动到 `sources.rs`。
- 集中 `ProjectStore::open` 和摘要刷新时机，减少跨领域重复协调。

退出条件：项目打开、导入、重新导入、离线素材和批量删除测试通过。

### 阶段 E：共享接口收敛

- 将跨模块错误移动到 `error.rs`。
- 删除无用重导出和已经没有调用方的辅助函数。
- 检查 Tauri Adapter 的 import 列表，确认它只依赖应用层公开接口。
- 更新架构、实现报告和测试计划中的文件导航说明。

## 4. 验证矩阵

每个阶段执行：

```powershell
cargo fmt --all
cargo test --workspace
corepack pnpm --filter @freetrain/desktop check
corepack pnpm --filter @freetrain/desktop build
git diff --check
```

额外检查：

- 源素材文件内容不得发生变化。
- 数据库 Schema 和迁移版本不得变化。
- Tauri 命令名称和前端 `invoke` 参数不得变化。
- 现有项目可以直接打开，不要求重新导入。
- 拆分提交只包含代码移动、可见性调整和必要 import 修改。

## 5. 回滚策略

- 每个阶段保持可独立提交。
- 若某阶段出现行为变化，回滚该阶段，不影响之前已验证的模块。
- 在完成阶段 E 前，`lib.rs` 通过 `pub use` 保持兼容，调用方无需同步迁移。
