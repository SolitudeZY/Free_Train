<script lang="ts">
  import { convertFileSrc, invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { confirm as confirmDialog, open } from "@tauri-apps/plugin-dialog";
  import {
    Activity,
    AlertCircle,
    Camera,
    Check,
    ChevronDown,
    ChevronRight,
    CircleGauge,
    Clock3,
    Crop,
    Download,
    FileImage,
    FileVideo,
    Film,
    Flag,
    FolderInput,
    FolderOpen,
    FolderOutput,
    FolderPlus,
    FolderTree,
    HardDrive,
    Hash,
    Image as ImageIcon,
    Images,
    LayoutGrid,
    Link2,
    Lock,
    ListX,
    ListChecks,
    LoaderCircle,
    Maximize2,
    Moon,
    MoreHorizontal,
    Pause,
    Pin,
    Play,
    Plus,
    RefreshCw,
    RotateCcw,
    ScanLine,
    Search,
    Save,
    Settings2,
    SkipBack,
    SkipForward,
    SlidersHorizontal,
    SquareCheckBig,
    Sun,
    Timer,
    Trash2,
    Unlock,
    Video,
    X,
  } from "lucide-svelte";
  import { onMount } from "svelte";

  type SourceKind = "image" | "video";
  type SourceStatus = "online" | "offline" | "error";
  type ProjectSummary = {
    id: string;
    name: string;
    path: string;
    createdAt: string;
    sourceCount: number;
    offlineCount: number;
    candidateCount: number;
  };
  type SourceAsset = {
    id: string;
    absolutePath: string;
    fileName: string;
    relativeFolder: string;
    sourceGroup: string;
    sourceIdentifier: string;
    kind: SourceKind;
    status: SourceStatus;
    sizeBytes: number;
    modifiedUnixMs: number;
    quickFingerprint: string;
    sha256: string | null;
    width: number | null;
    height: number | null;
    durationMs: number | null;
    codec: string | null;
    frameRate: string | null;
    captureTime: string | null;
    captureTimeSource: string | null;
    orientation: number | null;
    thumbnailPath: string | null;
    error: string | null;
    importedAt: string;
    lastCheckedAt: string;
  };
  type ImportResult = {
    discovered: number;
    imported: number;
    updated: number;
    unsupported: number;
    failures: { path: string; error: string }[];
  };
  type SourceDeletionResult = { deleted: number; candidateDeleted: number; failures: { path: string; error: string }[] };
  type SourceDeletionProgress = { completed: number; total: number; deleted: number; candidateDeleted: number };
  type VideoSelection = { id: string; sourceId: string; startMs: number; endMs: number; label: string; protected: boolean; createdAt: string };
  type CandidateImage = {
    id: string; sourceId: string; videoOffsetMs: number; sourceFrameNumber: number | null;
    selectionMethod: string; parametersJson: string; imagePath: string; thumbnailPath: string;
    width: number; height: number; pinned: boolean; createdAt: string;
  };
  type SamplingMode = "fixed_interval" | "frame_interval" | "target_count" | "valid_ranges" | "change_triggered";
  type SamplingConfig = {
    mode: SamplingMode; intervalMs: number; frameInterval: number; targetCount: number;
    rangeIds: string[]; customTimestampsMs: number[]; pinResults: boolean;
  };
  type SamplingEstimate = { timestampsMs: number[]; estimatedCount: number };
  type GroupSamplingEstimate = { sourceCount: number; estimatedCount: number };
  type SamplingExecutionResult = { planned: number; created: number; existing: number; failures: { path: string; error: string }[] };
  type CandidateDeletionResult = { deleted: number; failures: { path: string; error: string }[] };
  type ChangePoint = { timestampMs: number; score: number };
  type ChangeAnalysis = { points: ChangePoint[]; suggestedTimestampsMs: number[] };
  type EdgeStrategy = "discard" | "pad" | "shift_to_edge";
  type PaddingMode = "constant" | "edge" | "reflect";
  type ResizeMode = "stretch" | "fit" | "fill" | "long_side";
  type ExportFormat = "jpeg" | "png" | "webp";
  type ExportContent = "frames" | "tiles";
  type ExportSourceScope = "current" | "source_group";
  type ExportCandidateScope = "all" | "selected";
  type ConflictStrategy = "append_sequence" | "append_hash" | "skip" | "fail";
  type RoiProfile = {
    id: string; scope: "source_group" | "source"; scopeValue: string; name: string;
    roi: { x: number; y: number; width: number; height: number };
    renderConfig: {
      tile: { tile_width: number; tile_height: number; overlap_x: number; overlap_y: number; edge_strategy: EdgeStrategy };
      resize: ResizeMode; padding: PaddingMode; fill: [number, number, number, number];
    };
    inherited: boolean; updatedAt: string;
  };
  type TilePlacement = {
    row: number; column: number; sourceX: number; sourceY: number; sourceWidth: number; sourceHeight: number;
    outputWidth: number; outputHeight: number; padded: boolean;
  };
  type TilePreview = {
    sourceId: string; candidateId: string | null; roiProfileId: string; roiName: string;
    placement: TilePlacement; previewPath: string;
  };
  type ExcludedTile = { sourceId: string; candidateId: string | null; roiProfileId: string; row: number; column: number };
  type ExportPlan = {
    outputDir: string; items: { fileName: string; placement: TilePlacement }[];
    skipped: number; estimatedBytes: number;
  };
  type ExportResult = { exportId: string; written: number; skipped: number; manifestPath: string; failures: { path: string; error: string }[] };
  type SimilarityScope = "source" | "source_group" | "project";
  type ReviewAction = "keep" | "exclude" | "restore" | "lock" | "unlock" | "make_representative";
  type QualityMetrics = {
    width: number; height: number; aspectRatio: number; sharpness: number;
    underexposedRatio: number; overexposedRatio: number; entropy: number; lowInformation: number;
  };
  type ReviewItem = {
    assetKey: string; sourceId: string; candidateId: string | null; sourceGroup: string;
    sourceIdentifier: string; displayName: string; imagePath: string; thumbnailPath: string;
    videoOffsetMs: number | null; selectionMethod: string; pinned: boolean; metrics: QualityMetrics | null;
    automaticStatus: "keep" | "suggest_exclude" | "warning" | "error"; automaticReasons: string[];
    manualDecision: "keep" | "exclude" | null; locked: boolean; similarityGroupId: string | null;
    similarityScore: number | null; representative: boolean; lockedConflict: boolean; decodeError: string | null;
  };
  type ReviewSummary = {
    total: number; keep: number; suggestedExclude: number; manuallyExcluded: number;
    warning: number; failed: number; locked: number; similarityGroups: number;
  };
  type ReviewWorkspace = { items: ReviewItem[]; summary: ReviewSummary };

  const sections = [
    { id: "sources", label: "素材", icon: Images },
    { id: "process", label: "处理", icon: SlidersHorizontal },
    { id: "review", label: "审核", icon: ListChecks },
    { id: "export", label: "导出", icon: Download },
    { id: "jobs", label: "任务", icon: CircleGauge },
  ] as const;

  const mediaFilters = [{ name: "视频与图片", extensions: ["jpg", "jpeg", "png", "bmp", "tif", "tiff", "webp", "mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "m2ts"] }];
  const pageSize = 500;
  const enabledSections = new Set(["sources", "process", "review", "export"]);

  let activeSection = $state("sources");
  let theme = $state<"light" | "dark">("light");
  let project = $state<ProjectSummary | null>(null);
  let recentProjectPath = $state("");
  let sources = $state<SourceAsset[]>([]);
  let selectedSourceId = $state("");
  let checkedSourceIds = $state<Set<string>>(new Set());
  let collapsedSourceGroups = $state<Set<string>>(new Set());
  let search = $state("");
  let busyMessage = $state("");
  let message = $state("");
  let messageKind = $state<"info" | "error">("info");
  let projectMenuOpen = $state(false);
  let importMenuOpen = $state(false);
  let createDialogOpen = $state(false);
  let createParent = $state("");
  let createName = $state("");
  let sourceRemovalDialogOpen = $state(false);
  let pendingSourceRemovalIds = $state<string[]>([]);
  let sourceRemovalProgress = $state<SourceDeletionProgress | null>(null);
  let sourceRemovalCompletion = $state<{ deleted: number; candidateDeleted: number } | null>(null);
  let sourceRemovalCompletionTimer: number | undefined;
  let dragActive = $state(false);
  let visibleLimit = $state(pageSize);
  let verifiedSourceId = $state("");
  let previewChecking = $state(false);
  let sourceContextMenu = $state<{ x: number; y: number } | null>(null);
  let sourcePanel: HTMLElement;
  let videoElement = $state<HTMLVideoElement>();
  const checkingSourceIds = new Set<string>();
  let inspectorTab = $state<"info" | "sampling" | "roi" | "export">("info");
  let frameTimestamps = $state<number[]>([]);
  let currentTimeMs = $state(0);
  let isPlaying = $state(false);
  let playbackRate = $state(1);
  let jumpTime = $state("00:00:00.000");
  let markInMs = $state<number | null>(null);
  let protectNewRange = $state(false);
  let videoSelections = $state<VideoSelection[]>([]);
  let candidates = $state<CandidateImage[]>([]);
  let selectedCandidateId = $state("");
  let checkedCandidateIds = $state<Set<string>>(new Set());
  let videoBusy = $state("");
  let samplingMode = $state<SamplingMode>("fixed_interval");
  let intervalMs = $state(1_000);
  let frameInterval = $state(30);
  let targetCount = $state(10);
  let pinBatchResults = $state(false);
  let applySourceGroup = $state(false);
  let estimatedSourceCount = $state(1);
  let samplingEstimate = $state<SamplingEstimate | null>(null);
  let changeAnalysis = $state<ChangeAnalysis | null>(null);
  let changeChartExpanded = $state(false);
  let analysisFps = $state(2);
  let changeThreshold = $state(0.08);
  let minChangeIntervalMs = $state(500);
  let maxChangeIntervalMs = $state(5_000);
  let estimatePulse = $state(false);
  let estimatePulseTimer: number | undefined;
  let roiProfiles = $state<RoiProfile[]>([]);
  let tilePreviews = $state<TilePreview[]>([]);
  let selectedTilePreview = $state<TilePreview | null>(null);
  let checkedTileKeys = $state<Set<string>>(new Set());
  let excludedTiles = $state<Map<string, ExcludedTile>>(new Map());
  let selectedRoiId = $state("");
  let roiBusy = $state("");
  let roiName = $state("主区域");
  let roiScope = $state<"source_group" | "source">("source_group");
  let roiX = $state(0);
  let roiY = $state(0);
  let roiWidth = $state(640);
  let roiHeight = $state(640);
  let tileWidth = $state(640);
  let tileHeight = $state(640);
  let overlapXPercent = $state(0);
  let overlapYPercent = $state(0);
  let edgeStrategy = $state<EdgeStrategy>("shift_to_edge");
  let paddingMode = $state<PaddingMode>("constant");
  let resizeMode = $state<ResizeMode>("stretch");
  let fillColor = $state("#000000");
  let exportDirectory = $state("");
  let namingTemplate = $state("{source}_{roi}_r{row}_c{col}_{index}");
  let exportFormat = $state<ExportFormat>("png");
  let exportContent = $state<ExportContent>("tiles");
  let exportSourceScope = $state<ExportSourceScope>("current");
  let exportCandidateScope = $state<ExportCandidateScope>("all");
  let conflictStrategy = $state<ConflictStrategy>("append_sequence");
  let exportPlan = $state<ExportPlan | null>(null);
  let exportBusy = $state("");
  let reviewWorkspace = $state<ReviewWorkspace | null>(null);
  let reviewBusy = $state("");
  let checkedReviewKeys = $state<Set<string>>(new Set());
  let selectedReviewKey = $state("");
  let reviewStatusFilter = $state("all");
  let reviewSourceFilter = $state("all");
  let reviewGroupFilter = $state("all");
  let reviewVisibleLimit = $state(400);
  let reviewMinWidth = $state(320);
  let reviewMinHeight = $state(240);
  let reviewMinSharpness = $state(80);
  let reviewMaxUnderexposed = $state(35);
  let reviewMaxOverexposed = $state(35);
  let reviewMaxLowInformation = $state(72);
  let reviewPhashDistance = $state(8);
  let reviewSsimThreshold = $state(94);
  let reviewSimilarityScope = $state<SimilarityScope>("source");
  let reviewTimeWindowSeconds = $state(30);
  let roiDrag = $state<{
    mode: "create" | "move";
    startX: number;
    startY: number;
    originX: number;
    originY: number;
    originWidth: number;
    originHeight: number;
  } | null>(null);
  let roiAutoSaveTimer: number | undefined;

  const selectedSource = $derived(sources.find((source) => source.id === selectedSourceId) ?? null);
  const allSourcesChecked = $derived((project?.sourceCount ?? 0) > 0 && checkedSourceIds.size === project?.sourceCount);
  const filteredSources = $derived(
    search.trim()
      ? sources.filter((source) =>
          `${source.fileName} ${source.sourceGroup} ${source.sourceIdentifier}`.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase()),
        )
      : sources,
  );
  const visibleSources = $derived(filteredSources.slice(0, visibleLimit));
  const sourceGroups = $derived.by(() => {
    const groups = new Map<string, SourceAsset[]>();
    for (const source of visibleSources) {
      const items = groups.get(source.sourceGroup) ?? [];
      items.push(source);
      groups.set(source.sourceGroup, items);
    }
    return [...groups.entries()];
  });
  const previewUrl = $derived(selectedSource && verifiedSourceId === selectedSource.id ? convertFileSrc(selectedSource.absolutePath) : "");
  const isActiveVideo = $derived(selectedSource?.kind === "video" && selectedSource.status === "online" && verifiedSourceId === selectedSource.id);
  const changeChartMaxTimestamp = $derived(Math.max(changeAnalysis?.points.at(-1)?.timestampMs ?? selectedSource?.durationMs ?? 1, 1));
  const changeChartMaxScore = $derived(Math.max(changeThreshold, ...(changeAnalysis?.points.map((point) => point.score) ?? []), 0.001));
  const changeThresholdY = $derived(70 - (Math.min(changeThreshold, changeChartMaxScore) / changeChartMaxScore) * 60);
  const changePolyline = $derived.by(() => {
    if (!changeAnalysis?.points.length) return "";
    return changeAnalysis.points
      .map((point) => `${42 + (point.timestampMs / changeChartMaxTimestamp) * 266},${70 - (point.score / changeChartMaxScore) * 60}`)
      .join(" ");
  });
  const selectedRoi = $derived(roiProfiles.find((profile) => profile.id === selectedRoiId) ?? null);
  const activeTilePreviews = $derived(tilePreviews.filter((preview) => !selectedRoiId || preview.roiProfileId === selectedRoiId));
  const checkedCandidateCount = $derived(checkedCandidateIds.size);
  const checkedTileCount = $derived(checkedTileKeys.size);
  const tilePreviewTotal = $derived(tilePreviews.length + excludedTiles.size);
  const selectedSourceGroupCount = $derived(selectedSource ? sources.filter((source) => source.kind === selectedSource.kind && source.sourceGroup === selectedSource.sourceGroup).length : 0);
  const selectedReviewItem = $derived(reviewWorkspace?.items.find((item) => item.assetKey === selectedReviewKey) ?? null);
  const reviewSourceOptions = $derived([...new Set(reviewWorkspace?.items.map((item) => item.sourceIdentifier) ?? [])].sort());
  const reviewGroupOptions = $derived([...new Set((reviewWorkspace?.items.map((item) => item.similarityGroupId).filter(Boolean) as string[] | undefined) ?? [])].sort());
  const filteredReviewItems = $derived((reviewWorkspace?.items ?? []).filter((item) => {
    const status = reviewEffectiveStatus(item);
    return (reviewStatusFilter === "all" || status === reviewStatusFilter)
      && (reviewSourceFilter === "all" || item.sourceIdentifier === reviewSourceFilter)
      && (reviewGroupFilter === "all" || item.similarityGroupId === reviewGroupFilter);
  }));
  const visibleReviewItems = $derived(filteredReviewItems.slice(0, reviewVisibleLimit));
  const shortcutHints = $derived.by(() => {
    if (activeSection === "review") return [["K", "保留"], ["X", "排除"], ["R", "恢复"], ["L", "锁定/解锁"], ["Enter", "设为代表图"]];
    if (inspectorTab === "export") return [["Ctrl+S", "检查计划"], ["Enter", "确认导出"], ["Esc", "返回处理"]];
    if (inspectorTab === "roi") return [["R", "新建 ROI"], ["拖动", "移动 ROI"], ["方向键", "微调"], ["Ctrl+S", "保存"], ["P", "保存并预览"]];
    if (isActiveVideo) return [["Space", "播放/暂停"], ["←/→", "逐帧"], ["A/D", "候选切换"], ["C", "保存帧"], ["I/O", "片段入/出点"]];
    return [["Ctrl+O", "打开项目"], ["Ctrl+I", "导入素材"], ["E", "导出"]];
  });

  function setMessage(text: string, kind: "info" | "error" = "info") {
    message = text;
    messageKind = kind;
  }

  function errorText(error: unknown) {
    return error instanceof Error ? error.message : String(error);
  }

  function reviewEffectiveStatus(item: ReviewItem) {
    if (item.manualDecision === "exclude") return "excluded";
    if (item.manualDecision === "keep") return "keep";
    if (item.automaticStatus === "suggest_exclude") return "suggested";
    if (item.automaticStatus === "warning") return "warning";
    if (item.automaticStatus === "error") return "error";
    return "keep";
  }

  function reviewStatusLabel(item: ReviewItem) {
    const status = reviewEffectiveStatus(item);
    return status === "excluded" ? "人工排除" : status === "suggested" ? "建议排除" : status === "warning" ? "质量警告" : status === "error" ? "处理失败" : item.manualDecision === "keep" ? "人工保留" : "保留";
  }

  function reviewThumbnailUrl(item: ReviewItem) {
    return convertFileSrc(item.thumbnailPath || item.imagePath);
  }

  function toggleReviewChecked(assetKey: string, checked: boolean) {
    const next = new Set(checkedReviewKeys);
    if (checked) next.add(assetKey);
    else next.delete(assetKey);
    checkedReviewKeys = next;
  }

  function toggleAllVisibleReview(checked: boolean) {
    const next = new Set(checkedReviewKeys);
    for (const item of visibleReviewItems) {
      if (checked) next.add(item.assetKey);
      else next.delete(item.assetKey);
    }
    checkedReviewKeys = next;
  }

  function selectReviewItem(item: ReviewItem) {
    selectedReviewKey = item.assetKey;
  }

  async function loadReviewWorkspace(quiet = false) {
    if (!project) return;
    try {
      reviewWorkspace = await invoke<ReviewWorkspace>("get_review_workspace");
      if (reviewWorkspace.items.length && !reviewWorkspace.items.some((item) => item.assetKey === selectedReviewKey)) {
        selectedReviewKey = reviewWorkspace.items[0].assetKey;
      }
    } catch (error) {
      reviewWorkspace = null;
      if (!quiet) setMessage(errorText(error), "error");
    }
  }

  async function runReviewAnalysis() {
    if (!project || reviewBusy) return;
    reviewBusy = "正在测量质量并生成相似组";
    activeSection = "review";
    try {
      reviewWorkspace = await invoke<ReviewWorkspace>("run_review_analysis", {
        config: {
          minWidth: reviewMinWidth,
          minHeight: reviewMinHeight,
          minSharpness: reviewMinSharpness,
          maxUnderexposedRatio: reviewMaxUnderexposed / 100,
          maxOverexposedRatio: reviewMaxOverexposed / 100,
          maxLowInformation: reviewMaxLowInformation / 100,
          phashDistance: reviewPhashDistance,
          ssimThreshold: reviewSsimThreshold / 100,
          similarityScope: reviewSimilarityScope,
          videoTimeWindowMs: reviewTimeWindowSeconds * 1_000,
        },
      });
      checkedReviewKeys = new Set();
      selectedReviewKey = reviewWorkspace.items[0]?.assetKey ?? "";
      reviewVisibleLimit = 400;
      setMessage(`分析完成：${reviewWorkspace.summary.total} 张审核资产，${reviewWorkspace.summary.similarityGroups} 个相似组，${reviewWorkspace.summary.suggestedExclude} 张建议排除`);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      reviewBusy = "";
    }
  }

  async function applyReviewAction(action: ReviewAction, assetKeys?: string[]) {
    const keys = assetKeys ?? (checkedReviewKeys.size ? [...checkedReviewKeys] : selectedReviewKey ? [selectedReviewKey] : []);
    if (!keys.length || reviewBusy) return;
    reviewBusy = "正在保存审核决定";
    try {
      reviewWorkspace = await invoke<ReviewWorkspace>("update_review_items", { assetKeys: keys, action });
      if (action !== "lock" && action !== "unlock") checkedReviewKeys = new Set();
      setMessage(`已更新 ${keys.length} 个审核项`);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      reviewBusy = "";
    }
  }

  function thumbnailUrl(source: SourceAsset) {
    return source.thumbnailPath ? convertFileSrc(source.thumbnailPath) : "";
  }

  function toggleSourceChecked(sourceId: string, checked: boolean) {
    const next = new Set(checkedSourceIds);
    if (checked) next.add(sourceId);
    else next.delete(sourceId);
    checkedSourceIds = next;
  }

  function sourceGroupItems(group: string) {
    return sources.filter((source) => source.sourceGroup === group);
  }

  function sourceGroupChecked(group: string) {
    const items = sourceGroupItems(group);
    return items.length > 0 && items.every((source) => checkedSourceIds.has(source.id));
  }

  function toggleSourceGroupChecked(group: string, checked: boolean) {
    const items = sourceGroupItems(group);
    const next = new Set(checkedSourceIds);
    for (const source of items) {
      if (checked) next.add(source.id);
      else next.delete(source.id);
    }
    checkedSourceIds = next;
    setMessage(checked ? `已选中来源组“${group}”的 ${items.length} 个来源` : `已取消来源组“${group}”`);
  }

  function toggleSourceGroupCollapsed(group: string) {
    const next = new Set(collapsedSourceGroups);
    if (next.has(group)) next.delete(group);
    else next.add(group);
    collapsedSourceGroups = next;
  }

  async function toggleAllSources() {
    const clearing = allSourcesChecked;
    if (clearing) {
      checkedSourceIds = new Set();
      setMessage("已清除全部来源选择");
      return;
    }
    busyMessage = "正在选择项目全部来源";
    try {
      const sourceIds = await invoke<string[]>("list_all_source_ids");
      checkedSourceIds = new Set(sourceIds);
      setMessage(`已选中当前项目的全部 ${sourceIds.length} 个来源`);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      busyMessage = "";
    }
  }

  function selectOfflineSources() {
    checkedSourceIds = new Set(sources.filter((source) => source.status !== "online").map((source) => source.id));
    setMessage(`已选中 ${checkedSourceIds.size} 个离线或异常来源`);
  }

  function requestRemoveCheckedSources() {
    pendingSourceRemovalIds = [...checkedSourceIds];
    if (!pendingSourceRemovalIds.length) return;
    sourceRemovalDialogOpen = true;
  }

  function cancelRemoveCheckedSources() {
    sourceRemovalDialogOpen = false;
    pendingSourceRemovalIds = [];
  }

  async function confirmRemoveCheckedSources() {
    const sourceIds = [...pendingSourceRemovalIds];
    if (!sourceIds.length) return;
    sourceRemovalDialogOpen = false;
    pendingSourceRemovalIds = [];
    sourceRemovalCompletion = null;
    sourceRemovalProgress = { completed: 0, total: sourceIds.length, deleted: 0, candidateDeleted: 0 };
    busyMessage = `正在移除项目来源 0 / ${sourceIds.length}`;
    try {
      const result = await invoke<SourceDeletionResult>("remove_sources", { sourceIds });
      checkedSourceIds = new Set();
      await loadSources(true);
      sourceRemovalCompletion = { deleted: result.deleted, candidateDeleted: result.candidateDeleted };
      if (sourceRemovalCompletionTimer !== undefined) window.clearTimeout(sourceRemovalCompletionTimer);
      sourceRemovalCompletionTimer = window.setTimeout(() => (sourceRemovalCompletion = null), 4_500);
      setMessage(
        `已移除 ${result.deleted} 个来源和 ${result.candidateDeleted} 个候选${result.failures.length ? `，${result.failures.length} 个缓存文件未能清理` : ""}`,
        result.failures.length ? "error" : "info",
      );
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      sourceRemovalProgress = null;
      busyMessage = "";
    }
  }

  function formatBytes(bytes: number) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
    return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  }

  function formatDuration(duration: number | null) {
    if (duration === null) return "--";
    const seconds = Math.floor(duration / 1000);
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const rest = seconds % 60;
    return [hours, minutes, rest].map((value) => String(value).padStart(2, "0")).join(":");
  }

  function formatTimestamp(timestampMs: number) {
    const totalSeconds = Math.floor(timestampMs / 1_000);
    const hours = Math.floor(totalSeconds / 3_600);
    const minutes = Math.floor((totalSeconds % 3_600) / 60);
    const seconds = totalSeconds % 60;
    const milliseconds = Math.round(timestampMs % 1_000);
    return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}.${String(milliseconds).padStart(3, "0")}`;
  }

  function parseTimestamp(value: string) {
    if (/^\d+(\.\d+)?$/.test(value.trim())) return Math.round(Number(value) * 1_000);
    const match = value.trim().match(/^(\d+):(\d{1,2}):(\d{1,2})(?:\.(\d{1,3}))?$/);
    if (!match) return null;
    return (Number(match[1]) * 3_600 + Number(match[2]) * 60 + Number(match[3])) * 1_000 + Number((match[4] ?? "0").padEnd(3, "0"));
  }

  function candidateThumbnailUrl(candidate: CandidateImage) {
    return convertFileSrc(candidate.thumbnailPath);
  }

  function formatChartTime(timestampMs: number) {
    const totalSeconds = Math.round(timestampMs / 1_000);
    const hours = Math.floor(totalSeconds / 3_600);
    const minutes = Math.floor((totalSeconds % 3_600) / 60);
    const seconds = totalSeconds % 60;
    return hours > 0
      ? `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`
      : `${minutes}:${String(seconds).padStart(2, "0")}`;
  }

  function selectCandidate(candidate: CandidateImage) {
    selectedCandidateId = candidate.id;
    if (exportCandidateScope === "selected") exportPlan = null;
    seekTo(candidate.videoOffsetMs);
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(`[data-candidate-id="${candidate.id}"]`)?.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "center" });
    });
  }

  function toggleCandidateChecked(candidateId: string, checked: boolean) {
    const next = new Set(checkedCandidateIds);
    if (checked) next.add(candidateId);
    else next.delete(candidateId);
    checkedCandidateIds = next;
  }

  function tilePreviewKey(preview: TilePreview) {
    return [preview.sourceId, preview.candidateId ?? "source", preview.roiProfileId, preview.placement.row, preview.placement.column].join(":");
  }

  function toggleTileChecked(preview: TilePreview, checked: boolean) {
    const key = tilePreviewKey(preview);
    const next = new Set(checkedTileKeys);
    if (checked) next.add(key);
    else next.delete(key);
    checkedTileKeys = next;
  }

  async function clearTilePreviews(scope: "selected" | "all") {
    const targets = scope === "selected"
      ? tilePreviews.filter((preview) => checkedTileKeys.has(tilePreviewKey(preview)))
      : tilePreviews;
    if (!targets.length) return;
    const confirmed = await confirmDialog(
      `将从本次导出计划中排除 ${targets.length} 个切片。重新生成切片预览可恢复。`,
      { title: scope === "selected" ? "清空选中切片" : "清空全部切片", kind: "warning" },
    );
    if (!confirmed) return;
    const nextExcluded = new Map(excludedTiles);
    const removedKeys = new Set<string>();
    for (const preview of targets) {
      const key = tilePreviewKey(preview);
      removedKeys.add(key);
      nextExcluded.set(key, {
        sourceId: preview.sourceId,
        candidateId: preview.candidateId,
        roiProfileId: preview.roiProfileId,
        row: preview.placement.row,
        column: preview.placement.column,
      });
    }
    excludedTiles = nextExcluded;
    tilePreviews = tilePreviews.filter((preview) => !removedKeys.has(tilePreviewKey(preview)));
    checkedTileKeys = new Set();
    if (selectedTilePreview && removedKeys.has(tilePreviewKey(selectedTilePreview))) selectedTilePreview = tilePreviews[0] ?? null;
    exportPlan = null;
    setMessage(`已从本次导出计划排除 ${targets.length} 个切片`);
  }

  function navigateCandidate(direction: -1 | 1) {
    if (!candidates.length) return;
    let index = candidates.findIndex((candidate) => candidate.id === selectedCandidateId);
    if (index < 0) {
      index = candidates.reduce((closest, candidate, candidateIndex) =>
        Math.abs(candidate.videoOffsetMs - currentTimeMs) < Math.abs(candidates[closest].videoOffsetMs - currentTimeMs) ? candidateIndex : closest, 0);
    } else {
      index = Math.max(0, Math.min(candidates.length - 1, index + direction));
    }
    selectCandidate(candidates[index]);
  }

  function pulseEstimate() {
    estimatePulse = false;
    if (estimatePulseTimer !== undefined) window.clearTimeout(estimatePulseTimer);
    requestAnimationFrame(() => {
      estimatePulse = true;
      estimatePulseTimer = window.setTimeout(() => (estimatePulse = false), 1_300);
    });
  }

  function resetVideoWorkspace() {
    frameTimestamps = [];
    currentTimeMs = 0;
    isPlaying = false;
    markInMs = null;
    videoSelections = [];
    candidates = [];
    selectedCandidateId = "";
    checkedCandidateIds = new Set();
    samplingEstimate = null;
    changeAnalysis = null;
  }

  function hexToRgba(value: string): [number, number, number, number] {
    const normalized = value.replace("#", "").padEnd(6, "0").slice(0, 6);
    return [
      Number.parseInt(normalized.slice(0, 2), 16),
      Number.parseInt(normalized.slice(2, 4), 16),
      Number.parseInt(normalized.slice(4, 6), 16),
      255,
    ];
  }

  function rgbaToHex(value: [number, number, number, number]) {
    return "#" + value.slice(0, 3).map((channel) => Math.max(0, Math.min(255, channel)).toString(16).padStart(2, "0")).join("");
  }

  function roiStyle(profile: RoiProfile) {
    const width = Math.max(selectedSource?.width ?? 1, 1);
    const height = Math.max(selectedSource?.height ?? 1, 1);
    return "left:" + profile.roi.x / width * 100 + "%;top:" + profile.roi.y / height * 100
      + "%;width:" + profile.roi.width / width * 100 + "%;height:" + profile.roi.height / height * 100 + "%";
  }

  function draftRoiStyle() {
    const width = Math.max(selectedSource?.width ?? 1, 1);
    const height = Math.max(selectedSource?.height ?? 1, 1);
    return "left:" + roiX / width * 100 + "%;top:" + roiY / height * 100
      + "%;width:" + roiWidth / width * 100 + "%;height:" + roiHeight / height * 100 + "%";
  }

  function tileStyle(preview: TilePreview) {
    const width = Math.max(selectedSource?.width ?? 1, 1);
    const height = Math.max(selectedSource?.height ?? 1, 1);
    return "left:" + preview.placement.sourceX / width * 100 + "%;top:" + preview.placement.sourceY / height * 100
      + "%;width:" + preview.placement.sourceWidth / width * 100 + "%;height:" + preview.placement.sourceHeight / height * 100 + "%";
  }

  function roiPointerPosition(event: PointerEvent) {
    const element = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
    const stage = element?.closest<HTMLElement>(".preview-stage") ?? element;
    const bounds = stage?.getBoundingClientRect() ?? null;
    if (!bounds || !selectedSource) return null;
    return {
      x: Math.max(0, Math.min(selectedSource.width ?? 1, (event.clientX - bounds.left) / bounds.width * (selectedSource.width ?? 1))),
      y: Math.max(0, Math.min(selectedSource.height ?? 1, (event.clientY - bounds.top) / bounds.height * (selectedSource.height ?? 1))),
    };
  }

  function startRoiDrag(event: PointerEvent) {
    if (inspectorTab !== "roi" || !selectedSource || (event.target instanceof HTMLElement && event.target.closest(".roi-box"))) return;
    const position = roiPointerPosition(event);
    if (!position) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.currentTarget instanceof HTMLElement) event.currentTarget.setPointerCapture(event.pointerId);
    beginNewRoi();
    roiX = Math.round(position.x);
    roiY = Math.round(position.y);
    roiWidth = 1;
    roiHeight = 1;
    roiDrag = {
      mode: "create",
      startX: position.x,
      startY: position.y,
      originX: roiX,
      originY: roiY,
      originWidth: roiWidth,
      originHeight: roiHeight,
    };
  }

  function startRoiMove(event: PointerEvent, profile: RoiProfile) {
    if (inspectorTab !== "roi" || !selectedSource) return;
    const position = roiPointerPosition(event);
    if (!position) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.currentTarget instanceof HTMLElement) event.currentTarget.setPointerCapture(event.pointerId);
    editRoi(profile);
    roiDrag = {
      mode: "move",
      startX: position.x,
      startY: position.y,
      originX: profile.roi.x,
      originY: profile.roi.y,
      originWidth: profile.roi.width,
      originHeight: profile.roi.height,
    };
  }

  function moveRoiDrag(event: PointerEvent) {
    if (!roiDrag || !selectedSource) return;
    const position = roiPointerPosition(event);
    if (!position) return;
    event.preventDefault();
    if (roiDrag.mode === "create") {
      roiX = Math.round(Math.min(roiDrag.startX, position.x));
      roiY = Math.round(Math.min(roiDrag.startY, position.y));
      roiWidth = Math.max(1, Math.round(Math.abs(position.x - roiDrag.startX)));
      roiHeight = Math.max(1, Math.round(Math.abs(position.y - roiDrag.startY)));
    } else {
      const maxX = Math.max(0, (selectedSource.width ?? 1) - roiDrag.originWidth);
      const maxY = Math.max(0, (selectedSource.height ?? 1) - roiDrag.originHeight);
      roiX = Math.round(Math.max(0, Math.min(maxX, roiDrag.originX + position.x - roiDrag.startX)));
      roiY = Math.round(Math.max(0, Math.min(maxY, roiDrag.originY + position.y - roiDrag.startY)));
    }
  }

  async function endRoiDrag(event: PointerEvent) {
    if (!roiDrag) return;
    moveRoiDrag(event);
    const completedDrag = roiDrag;
    roiDrag = null;
    if (completedDrag.mode === "create" && (roiWidth < 2 || roiHeight < 2)) {
      setMessage("ROI 范围过小，请重新拖选", "error");
      return;
    }
    await saveRoi({ auto: true });
  }

  function beginNewRoi() {
    selectedRoiId = "";
    roiName = "ROI " + (roiProfiles.length + 1);
    roiScope = "source_group";
    roiX = 0;
    roiY = 0;
    roiWidth = selectedSource?.width ?? 640;
    roiHeight = selectedSource?.height ?? 640;
    paddingMode = "constant";
    fillColor = "#000000";
    tilePreviews = [];
    selectedTilePreview = null;
    checkedTileKeys = new Set();
    excludedTiles = new Map();
  }

  function editRoi(profile: RoiProfile) {
    selectedRoiId = profile.id;
    selectedTilePreview = null;
    roiName = profile.name;
    roiScope = profile.scope;
    roiX = profile.roi.x;
    roiY = profile.roi.y;
    roiWidth = profile.roi.width;
    roiHeight = profile.roi.height;
    tileWidth = profile.renderConfig.tile.tile_width;
    tileHeight = profile.renderConfig.tile.tile_height;
    overlapXPercent = Math.round(profile.renderConfig.tile.overlap_x / Math.max(tileWidth, 1) * 100);
    overlapYPercent = Math.round(profile.renderConfig.tile.overlap_y / Math.max(tileHeight, 1) * 100);
    edgeStrategy = profile.renderConfig.tile.edge_strategy;
    paddingMode = profile.renderConfig.padding;
    fillColor = rgbaToHex(profile.renderConfig.fill);
    resizeMode = profile.renderConfig.resize;
  }

  async function loadRoiWorkspace(sourceId: string) {
    try {
      roiProfiles = await invoke<RoiProfile[]>("get_roi_profiles", { sourceId });
      tilePreviews = [];
      selectedTilePreview = null;
      checkedTileKeys = new Set();
      excludedTiles = new Map();
      exportPlan = null;
      if (roiProfiles.length) editRoi(roiProfiles[0]);
      else beginNewRoi();
    } catch (error) {
      roiProfiles = [];
      setMessage(errorText(error), "error");
    }
  }

  async function saveRoi(options: { auto?: boolean; previewAfter?: boolean } = {}) {
    if (!selectedSource) return null;
    roiBusy = "正在保存 ROI";
    try {
      const overlapX = Math.min(Math.max(0, Math.round(tileWidth * overlapXPercent / 100)), Math.max(0, tileWidth - 1));
      const overlapY = Math.min(Math.max(0, Math.round(tileHeight * overlapYPercent / 100)), Math.max(0, tileHeight - 1));
      const saved = await invoke<RoiProfile>("save_roi_profile", {
        draft: {
          id: selectedRoiId || null,
          scope: roiScope,
          scopeValue: roiScope === "source" ? selectedSource.id : selectedSource.sourceGroup,
          name: roiName.trim(),
          roi: { x: Math.round(roiX), y: Math.round(roiY), width: Math.round(roiWidth), height: Math.round(roiHeight) },
          renderConfig: {
            tile: {
              tile_width: Math.round(tileWidth), tile_height: Math.round(tileHeight),
              overlap_x: overlapX, overlap_y: overlapY, edge_strategy: edgeStrategy,
            },
            resize: resizeMode, padding: paddingMode, fill: hexToRgba(fillColor),
          },
        },
      });
      await loadRoiWorkspace(selectedSource.id);
      selectedRoiId = saved.id;
      const effective = roiProfiles.find((profile) => profile.id === saved.id);
      if (effective) editRoi(effective);
      setMessage((options.auto ? "已自动保存 ROI“" : "已保存 ROI“") + saved.name + "”");
      if (options.previewAfter) await previewRoiTiles();
      return saved;
    } catch (error) {
      setMessage(errorText(error), "error");
      return null;
    } finally {
      roiBusy = "";
    }
  }

  async function removeRoi() {
    if (!selectedSource || !selectedRoiId) return;
    roiBusy = "正在删除 ROI";
    try {
      await invoke<boolean>("delete_roi_profile", { profileId: selectedRoiId });
      await loadRoiWorkspace(selectedSource.id);
      setMessage("ROI 已删除");
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      roiBusy = "";
    }
  }

  function nudgeRoi(deltaX: number, deltaY: number) {
    if (!selectedSource) return;
    roiX = Math.max(0, Math.min((selectedSource.width ?? 1) - roiWidth, roiX + deltaX));
    roiY = Math.max(0, Math.min((selectedSource.height ?? 1) - roiHeight, roiY + deltaY));
    if (roiAutoSaveTimer !== undefined) window.clearTimeout(roiAutoSaveTimer);
    roiAutoSaveTimer = window.setTimeout(() => void saveRoi({ auto: true }), 350);
  }

  async function previewRoiTiles() {
    if (!selectedSource) return;
    roiBusy = "正在生成切片预览";
    try {
      tilePreviews = await invoke<TilePreview[]>("preview_tiles", {
        sourceId: selectedSource.id,
        candidateId: selectedSource.kind === "video" ? selectedCandidateId || null : null,
        limit: 1_000,
      });
      checkedTileKeys = new Set();
      excludedTiles = new Map();
      selectedTilePreview = tilePreviews[0] ?? null;
      inspectorTab = "roi";
      setMessage("已生成 " + tilePreviews.length + " 个切片预览");
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      roiBusy = "";
    }
  }

  function selectTilePreview(preview: TilePreview) {
    selectedRoiId = preview.roiProfileId;
    selectedTilePreview = preview;
  }

  function currentExportRequest() {
    return {
      sourceId: selectedSource?.id ?? "",
      sourceScope: exportSourceScope,
      candidateId: selectedSource?.kind === "video" && exportSourceScope === "current" && exportCandidateScope === "selected" ? selectedCandidateId || null : null,
      outputDir: exportDirectory,
      namingTemplate,
      format: exportFormat,
      conflictStrategy,
      content: exportContent,
      excludedTiles: [...excludedTiles.values()],
    };
  }

  function setExportSourceScope(scope: ExportSourceScope) {
    exportSourceScope = scope;
    if (scope === "source_group") exportCandidateScope = "all";
    exportPlan = null;
  }

  function setExportContent(content: ExportContent) {
    const tileTemplate = "{source}_{roi}_r{row}_c{col}_{index}";
    const frameTemplate = "{source}_{timestamp_ms}_{index}";
    if (content === "frames" && namingTemplate === tileTemplate) namingTemplate = frameTemplate;
    if (content === "tiles" && namingTemplate === frameTemplate) namingTemplate = tileTemplate;
    exportContent = content;
    exportPlan = null;
  }

  function setExportCandidateScope(scope: ExportCandidateScope) {
    exportCandidateScope = scope;
    exportPlan = null;
  }

  async function chooseExportDirectory() {
    const path = await open({ title: "选择导出目录", directory: true });
    if (typeof path === "string") {
      exportDirectory = path;
      exportPlan = null;
    }
  }

  async function previewExportPlan() {
    if (!selectedSource || !exportDirectory) return;
    if (selectedSource.kind === "video" && exportSourceScope === "current" && exportCandidateScope === "selected" && !selectedCandidateId) {
      setMessage("请先在底部候选栏选择要导出的帧", "error");
      return;
    }
    exportBusy = "正在检查导出计划";
    try {
      exportPlan = await invoke<ExportPlan>("plan_export", { request: currentExportRequest() });
      setMessage("导出计划包含 " + exportPlan.items.length + " 张图片");
    } catch (error) {
      exportPlan = null;
      setMessage(errorText(error), "error");
    } finally {
      exportBusy = "";
    }
  }

  async function executeExport() {
    if (!selectedSource || !exportDirectory || !exportPlan) return;
    exportBusy = exportContent === "frames" ? "正在导出候选帧与来源清单" : "正在导出切片与来源清单";
    try {
      const result = await invoke<ExportResult>("run_export", { request: currentExportRequest() });
      setMessage("已导出 " + result.written + " 张，清单：" + result.manifestPath, result.failures.length ? "error" : "info");
      exportPlan = null;
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      exportBusy = "";
    }
  }

  async function loadVideoWorkspace(sourceId: string) {
    videoBusy = "正在读取帧时间轴";
    try {
      const [timestamps, selections, savedCandidates] = await Promise.all([
        invoke<number[]>("get_video_frame_timestamps", { sourceId }),
        invoke<VideoSelection[]>("get_video_selections", { sourceId }),
        invoke<CandidateImage[]>("get_candidates", { sourceId, offset: 0, limit: 10_000 }),
      ]);
      if (selectedSourceId !== sourceId) return;
      frameTimestamps = timestamps;
      videoSelections = selections;
      candidates = savedCandidates;
      selectedCandidateId = "";
      checkedCandidateIds = new Set();
      currentTimeMs = 0;
      jumpTime = formatTimestamp(0);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      videoBusy = "";
    }
  }

  function seekTo(timestampMs: number) {
    const duration = selectedSource?.durationMs ?? 0;
    const clamped = Math.max(0, Math.min(timestampMs, duration));
    currentTimeMs = clamped;
    jumpTime = formatTimestamp(clamped);
    if (videoElement) videoElement.currentTime = clamped / 1_000;
  }

  function stepFrame(direction: -1 | 1) {
    if (!frameTimestamps.length) return;
    const current = currentTimeMs;
    if (direction > 0) {
      seekTo(frameTimestamps.find((timestamp) => timestamp > current + 1) ?? frameTimestamps.at(-1) ?? current);
    } else {
      seekTo(frameTimestamps.findLast((timestamp) => timestamp < current - 1) ?? frameTimestamps[0]);
    }
  }

  async function togglePlayback() {
    if (!videoElement) return;
    if (videoElement.paused) await videoElement.play();
    else videoElement.pause();
  }

  function applyPlaybackRate(rate: number) {
    playbackRate = rate;
    if (videoElement) videoElement.playbackRate = rate;
  }

  function jumpToInput() {
    const parsed = parseTimestamp(jumpTime);
    if (parsed === null) setMessage("跳转时间格式无效", "error");
    else seekTo(parsed);
  }

  async function captureCurrentFrame() {
    if (!selectedSource || selectedSource.kind !== "video") return;
    videoBusy = "正在保存当前帧";
    try {
      const result = await invoke<{ candidate: CandidateImage; created: boolean }>("capture_video_frame", { sourceId: selectedSource.id, timestampMs: Math.round(currentTimeMs) });
      candidates = await invoke<CandidateImage[]>("get_candidates", { sourceId: selectedSource.id, offset: 0, limit: 10_000 });
      project = await invoke<ProjectSummary | null>("get_current_project");
      selectCandidate(result.candidate);
      setMessage(result.created ? `已锁定保存 ${formatTimestamp(result.candidate.videoOffsetMs)}` : "该时间点已有候选图片");
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      videoBusy = "";
    }
  }

  function setRangeIn() {
    markInMs = Math.round(currentTimeMs);
    setMessage(`入点 ${formatTimestamp(markInMs)}`);
  }

  async function setRangeOut() {
    if (!selectedSource || markInMs === null) {
      setMessage("请先设置有效片段入点", "error");
      return;
    }
    const endMs = Math.round(currentTimeMs);
    if (endMs <= markInMs) {
      setMessage("出点必须晚于入点", "error");
      return;
    }
    try {
      await invoke<VideoSelection>("add_video_selection", { sourceId: selectedSource.id, startMs: markInMs, endMs, protected: protectNewRange });
      videoSelections = await invoke<VideoSelection[]>("get_video_selections", { sourceId: selectedSource.id });
      setMessage(`已添加有效片段 ${formatTimestamp(markInMs)} - ${formatTimestamp(endMs)}`);
      markInMs = null;
    } catch (error) {
      setMessage(errorText(error), "error");
    }
  }

  async function removeSelection(selectionId: string) {
    await invoke<boolean>("remove_video_selection", { selectionId });
    if (selectedSource) videoSelections = await invoke<VideoSelection[]>("get_video_selections", { sourceId: selectedSource.id });
  }

  function currentSamplingConfig(): SamplingConfig {
    return {
      mode: samplingMode,
      intervalMs: Math.max(1, Math.round(intervalMs)),
      frameInterval: Math.max(1, Math.round(frameInterval)),
      targetCount: Math.max(1, Math.round(targetCount)),
      rangeIds: videoSelections.map((selection) => selection.id),
      customTimestampsMs: changeAnalysis?.suggestedTimestampsMs ?? [],
      pinResults: pinBatchResults,
    };
  }

  async function estimateVideoSampling() {
    if (!selectedSource || selectedSource.kind !== "video") return;
    videoBusy = "正在生成抽帧计划";
    try {
      if (applySourceGroup && !["valid_ranges", "change_triggered"].includes(samplingMode)) {
        const estimate = await invoke<GroupSamplingEstimate>("estimate_source_group_sampling", { sourceGroup: selectedSource.sourceGroup, config: currentSamplingConfig() });
        samplingEstimate = { timestampsMs: [], estimatedCount: estimate.estimatedCount };
        estimatedSourceCount = estimate.sourceCount;
      } else {
        samplingEstimate = await invoke<SamplingEstimate>("estimate_video_sampling", { sourceId: selectedSource.id, config: currentSamplingConfig() });
        estimatedSourceCount = 1;
      }
      inspectorTab = "sampling";
      pulseEstimate();
      setMessage(`预计从 ${estimatedSourceCount} 个视频生成 ${samplingEstimate.estimatedCount} 个候选图片`);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      videoBusy = "";
    }
  }

  async function runVideoSampling() {
    if (!selectedSource || selectedSource.kind !== "video") return;
    videoBusy = "正在执行视频抽帧";
    try {
      const result = applySourceGroup && !["valid_ranges", "change_triggered"].includes(samplingMode)
        ? await invoke<SamplingExecutionResult>("run_source_group_sampling", { sourceGroup: selectedSource.sourceGroup, config: currentSamplingConfig() })
        : await invoke<SamplingExecutionResult>("run_video_sampling", { sourceId: selectedSource.id, config: currentSamplingConfig() });
      candidates = await invoke<CandidateImage[]>("get_candidates", { sourceId: selectedSource.id, offset: 0, limit: 10_000 });
      checkedCandidateIds = new Set();
      project = await invoke<ProjectSummary | null>("get_current_project");
      setMessage(`新增 ${result.created} 个，已有 ${result.existing} 个${result.failures.length ? `，失败 ${result.failures.length} 个` : ""}`, result.failures.length ? "error" : "info");
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      videoBusy = "";
    }
  }

  async function clearCandidates(scope: "selected" | "all") {
    if (!selectedSource || selectedSource.kind !== "video") return;
    const candidateIds = scope === "selected" ? [...checkedCandidateIds] : null;
    const count = candidateIds?.length ?? candidates.length;
    if (count === 0) return;
    const confirmed = await confirmDialog(
      scope === "selected"
        ? `将清空当前视频中选中的 ${count} 个候选帧，包括其中的人工或锁定候选。源视频不会被修改。`
        : `将清空当前视频的全部 ${count} 个候选帧，包括人工和锁定候选。源视频不会被修改。`,
      { title: scope === "selected" ? "清空选中候选" : "清空全部候选", kind: "warning" },
    );
    if (!confirmed) return;
    videoBusy = scope === "selected" ? "正在清空选中候选" : "正在清空全部候选";
    try {
      const result = await invoke<CandidateDeletionResult>("remove_candidates", {
        sourceId: selectedSource.id,
        candidateIds,
      });
      candidates = await invoke<CandidateImage[]>("get_candidates", { sourceId: selectedSource.id, offset: 0, limit: 10_000 });
      project = await invoke<ProjectSummary | null>("get_current_project");
      checkedCandidateIds = new Set();
      if (!candidates.some((candidate) => candidate.id === selectedCandidateId)) selectedCandidateId = "";
      tilePreviews = [];
      selectedTilePreview = null;
      exportPlan = null;
      setMessage(
        `已清空 ${result.deleted} 个候选${result.failures.length ? `，${result.failures.length} 个缓存文件未能清理` : ""}`,
        result.failures.length ? "error" : "info",
      );
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      videoBusy = "";
    }
  }

  async function analyzeVideoChanges() {
    if (!selectedSource || selectedSource.kind !== "video") return;
    videoBusy = "正在分析画面变化";
    try {
      changeAnalysis = await invoke<ChangeAnalysis>("analyze_video_changes", {
        sourceId: selectedSource.id, analysisFps, threshold: changeThreshold,
        minIntervalMs: Math.round(minChangeIntervalMs), maxIntervalMs: Math.round(maxChangeIntervalMs),
      });
      samplingMode = "change_triggered";
      samplingEstimate = null;
      setMessage(`发现 ${changeAnalysis.suggestedTimestampsMs.length} 个建议时间点`);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      videoBusy = "";
    }
  }

  function activateSection(sectionId: string) {
    activeSection = sectionId;
    if (sectionId === "sources") inspectorTab = "info";
    if (sectionId === "process") inspectorTab = selectedSource?.kind === "video" ? "sampling" : "roi";
    if (sectionId === "export") inspectorTab = "export";
    if (sectionId === "review") void loadReviewWorkspace(true);
    if (sectionId === "sources" || inspectorTab === "sampling") selectedTilePreview = null;
  }

  function activateInspectorTab(tab: "info" | "sampling" | "roi" | "export") {
    inspectorTab = tab;
    if (tab === "info") activeSection = "sources";
    else if (tab === "export") activeSection = "export";
    else activeSection = "process";
    if (tab === "info" || tab === "sampling") selectedTilePreview = null;
  }

  async function loadSources(selectFirst = false) {
    if (!project) return;
    sources = await invoke<SourceAsset[]>("list_sources", { offset: 0, limit: 10_000 });
    visibleLimit = pageSize;
    let nextSourceId = selectedSourceId;
    if (selectFirst || !sources.some((source) => source.id === selectedSourceId)) {
      nextSourceId = sources[0]?.id ?? "";
    }
    project = await invoke<ProjectSummary | null>("get_current_project");
    if (nextSourceId) await selectSource(nextSourceId);
    else {
      selectedSourceId = "";
      verifiedSourceId = "";
    }
  }

  async function verifySource(sourceId: string, blockPreview: boolean, announceOffline: boolean) {
    if (!project || checkingSourceIds.has(sourceId)) return;
    checkingSourceIds.add(sourceId);
    if (blockPreview) {
      verifiedSourceId = "";
      previewChecking = true;
    }
    try {
      const checked = await invoke<SourceAsset>("check_source_status", { sourceId });
      sources = sources.map((source) => (source.id === checked.id ? checked : source));
      project = await invoke<ProjectSummary | null>("get_current_project");
      if (selectedSourceId === sourceId) {
        verifiedSourceId = sourceId;
        if (announceOffline && checked.status !== "online") {
          setMessage(checked.error ?? "源素材当前不可访问", "error");
        }
      }
    } catch (error) {
      if (selectedSourceId === sourceId) {
        verifiedSourceId = "";
        setMessage(errorText(error), "error");
      }
    } finally {
      checkingSourceIds.delete(sourceId);
      if (selectedSourceId === sourceId && blockPreview) previewChecking = false;
    }
  }

  async function selectSource(sourceId: string) {
    selectedSourceId = sourceId;
    await verifySource(sourceId, true, true);
    const source = sources.find((item) => item.id === sourceId);
    exportSourceScope = source?.kind === "image" ? "source_group" : "current";
    exportCandidateScope = "all";
    setExportContent(source?.kind === "video" ? "frames" : "tiles");
    if (source?.kind === "video" && source.status === "online") await loadVideoWorkspace(sourceId);
    else resetVideoWorkspace();
    await loadRoiWorkspace(sourceId);
  }

  async function selectSourceGroup(items: SourceAsset[]) {
    const source = items[0];
    if (!source) return;
    await selectSource(source.id);
    setExportSourceScope("source_group");
    activateSection("export");
  }

  async function activateProject(path: string) {
    busyMessage = "正在打开项目";
    try {
      project = await invoke<ProjectSummary>("open_project", { path });
      recentProjectPath = project.path;
      await loadSources(true);
      setMessage(`已打开项目“${project.name}”`);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      busyMessage = "";
      projectMenuOpen = false;
    }
  }

  async function chooseProject() {
    const path = await open({ title: "打开 Free-Train 项目", directory: true, recursive: true });
    if (typeof path === "string") await activateProject(path);
  }

  async function beginCreateProject() {
    projectMenuOpen = false;
    createDialogOpen = true;
    createName = "新项目";
    createParent = "";
  }

  async function chooseCreateParent() {
    const path = await open({ title: "选择项目保存位置", directory: true });
    if (typeof path === "string") createParent = path;
  }

  async function createProject() {
    if (!createParent || !createName.trim()) return;
    busyMessage = "正在创建项目";
    try {
      project = await invoke<ProjectSummary>("create_project", { parentDir: createParent, name: createName.trim() });
      recentProjectPath = project.path;
      sources = [];
      selectedSourceId = "";
      createDialogOpen = false;
      setMessage(`项目“${project.name}”已创建`);
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      busyMessage = "";
    }
  }

  async function importPaths(paths: string[]) {
    if (!project || paths.length === 0) return;
    busyMessage = `正在检查 ${paths.length} 个导入入口`;
    importMenuOpen = false;
    try {
      const result = await invoke<ImportResult>("import_sources", { paths });
      await loadSources(true);
      const failureText = result.failures.length ? `，${result.failures.length} 个失败` : "";
      setMessage(`导入 ${result.imported} 个，更新 ${result.updated} 个，忽略 ${result.unsupported} 个不支持文件${failureText}`, result.failures.length ? "error" : "info");
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      busyMessage = "";
    }
  }

  async function chooseFiles() {
    const paths = await open({ title: "导入视频或图片", multiple: true, filters: mediaFilters });
    if (Array.isArray(paths)) await importPaths(paths);
    else if (typeof paths === "string") await importPaths([paths]);
  }

  async function chooseFolder() {
    const path = await open({ title: "递归导入素材目录", directory: true, recursive: true });
    if (typeof path === "string") await importPaths([path]);
  }

  async function refreshStatuses() {
    if (!project) return;
    sourceContextMenu = null;
    busyMessage = "正在核对源素材";
    try {
      const changed = await invoke<number>("refresh_source_status");
      await loadSources();
      setMessage(changed ? `${changed} 个源素材状态发生变化` : "源素材状态已是最新");
    } catch (error) {
      setMessage(errorText(error), "error");
    } finally {
      busyMessage = "";
    }
  }

  async function relinkSelected() {
    if (!selectedSource) return;
    const path = await open({ title: "重新定位源素材", filters: mediaFilters });
    if (typeof path !== "string") return;
    try {
      const source = await invoke<SourceAsset>("relink_source", { sourceId: selectedSource.id, newPath: path });
      sources = sources.map((item) => (item.id === source.id ? source : item));
      verifiedSourceId = source.id;
      project = await invoke<ProjectSummary | null>("get_current_project");
      setMessage("源素材引用已更新");
    } catch (error) {
      setMessage(errorText(error), "error");
    }
  }

  function toggleTheme() {
    theme = theme === "light" ? "dark" : "light";
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("free-train-theme", theme);
  }

  function showSourceContextMenu(event: MouseEvent) {
    event.preventDefault();
    const width = 178;
    const height = 44;
    sourceContextMenu = {
      x: Math.min(event.clientX, window.innerWidth - width - 8),
      y: Math.min(event.clientY, window.innerHeight - height - 8),
    };
  }

  onMount(() => {
    const saved = localStorage.getItem("free-train-theme");
    const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    theme = saved === "dark" || (!saved && prefersDark) ? "dark" : "light";
    document.documentElement.dataset.theme = theme;

    const verifySelected = () => {
      if (selectedSourceId && project && !busyMessage) {
        void verifySource(selectedSourceId, false, false);
      }
    };
    const statusTimer = window.setInterval(verifySelected, 5_000);
    window.addEventListener("focus", verifySelected);
    const closeSourceContextMenu = () => (sourceContextMenu = null);
    sourcePanel.addEventListener("contextmenu", showSourceContextMenu);
    window.addEventListener("click", closeSourceContextMenu);
      const handleShortcut = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const editingText = target?.matches("input, textarea, select, [contenteditable='true']");
      const commandKey = event.ctrlKey || event.metaKey;
      if (sourceRemovalDialogOpen) {
        if (event.key === "Escape") {
          event.preventDefault();
          cancelRemoveCheckedSources();
        } else if (event.key === "Enter") {
          event.preventDefault();
          void confirmRemoveCheckedSources();
        }
        return;
      }
      if (changeChartExpanded && event.key === "Escape") {
        event.preventDefault();
        changeChartExpanded = false;
        return;
      }
      if (commandKey && event.key.toLowerCase() === "s") {
        event.preventDefault();
        if (inspectorTab === "export") void previewExportPlan();
        else if (inspectorTab === "roi" && selectedSource) void saveRoi();
        return;
      }
      if (commandKey && event.key.toLowerCase() === "o") {
        event.preventDefault();
        void chooseProject();
        return;
      }
      if (commandKey && event.key.toLowerCase() === "i") {
        event.preventDefault();
        if (project) void chooseFiles();
        return;
      }
      if (editingText) return;
      if (activeSection === "review") {
        const key = event.key.toLowerCase();
        if (key === "k") void applyReviewAction("keep");
        else if (key === "x") void applyReviewAction("exclude");
        else if (key === "r") void applyReviewAction("restore");
        else if (key === "l" && selectedReviewItem) void applyReviewAction(selectedReviewItem.locked ? "unlock" : "lock");
        else if (event.key === "Enter" && selectedReviewItem?.similarityGroupId) void applyReviewAction("make_representative", [selectedReviewItem.assetKey]);
        else return;
        event.preventDefault();
        return;
      }
      if (inspectorTab === "export") {
        if (event.key === "Enter" && exportPlan) {
          event.preventDefault();
          void executeExport();
        } else if (event.key === "Escape") {
          event.preventDefault();
          activateInspectorTab(selectedSource?.kind === "video" ? "sampling" : "roi");
        }
        return;
      }
      if (inspectorTab === "roi" && selectedSource) {
        const step = event.shiftKey ? 10 : 1;
        if (event.key.toLowerCase() === "r") {
          event.preventDefault();
          beginNewRoi();
        } else if (event.key.toLowerCase() === "p") {
          event.preventDefault();
          void saveRoi({ previewAfter: true });
        } else if (event.key === "Delete" && selectedRoiId) {
          event.preventDefault();
          void removeRoi();
        } else if (event.code === "ArrowLeft") {
          event.preventDefault();
          nudgeRoi(-step, 0);
        } else if (event.code === "ArrowRight") {
          event.preventDefault();
          nudgeRoi(step, 0);
        } else if (event.code === "ArrowUp") {
          event.preventDefault();
          nudgeRoi(0, -step);
        } else if (event.code === "ArrowDown") {
          event.preventDefault();
          nudgeRoi(0, step);
        } else if (event.key.toLowerCase() === "e") {
          event.preventDefault();
          activateSection("export");
        }
        return;
      }
      if (event.key.toLowerCase() === "e" && selectedSource) {
        event.preventDefault();
        activateSection("export");
      } else if (!isActiveVideo) {
        return;
      } else if (event.code === "Space") {
        event.preventDefault();
        void togglePlayback();
      } else if (event.code === "ArrowLeft") {
        event.preventDefault();
        stepFrame(-1);
      } else if (event.code === "ArrowRight") {
        event.preventDefault();
        stepFrame(1);
      } else if (event.key.toLowerCase() === "a" || event.code === "Numpad4" || event.key === "[") {
        event.preventDefault();
        navigateCandidate(-1);
      } else if (event.key.toLowerCase() === "d" || event.code === "Numpad6" || event.key === "]") {
        event.preventDefault();
        navigateCandidate(1);
      } else if (event.key.toLowerCase() === "c") {
        event.preventDefault();
        void captureCurrentFrame();
      } else if (event.key.toLowerCase() === "i") {
        event.preventDefault();
        setRangeIn();
      } else if (event.key.toLowerCase() === "o") {
        event.preventDefault();
        void setRangeOut();
      }
    };
    window.addEventListener("keydown", handleShortcut);

    let unlisten: (() => void) | undefined;
    let unlistenSourceRemovalProgress: (() => void) | undefined;
    if ("__TAURI_INTERNALS__" in window) {
      void listen<SourceDeletionProgress>("source-removal-progress", (event) => {
        sourceRemovalProgress = event.payload;
        busyMessage = `正在移除项目来源 ${event.payload.completed} / ${event.payload.total}`;
      }).then((stop) => (unlistenSourceRemovalProgress = stop));
      void getCurrentWebview().onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") dragActive = true;
        if (event.payload.type === "leave") dragActive = false;
        if (event.payload.type === "drop") {
          dragActive = false;
          if (project) void importPaths(event.payload.paths);
          else setMessage("请先创建或打开项目，再导入素材", "error");
        }
      }).then((stop) => (unlisten = stop));
    }

    void (async () => {
      try {
        project = await invoke<ProjectSummary | null>("get_current_project");
        recentProjectPath = (await invoke<string | null>("get_recent_project")) ?? "";
        if (project) await loadSources(true);
        else if (recentProjectPath) await activateProject(recentProjectPath);
      } catch (error) {
        setMessage(errorText(error), "error");
      }
    })();
    return () => {
      unlisten?.();
      unlistenSourceRemovalProgress?.();
      window.clearInterval(statusTimer);
      window.removeEventListener("focus", verifySelected);
      sourcePanel.removeEventListener("contextmenu", showSourceContextMenu);
      window.removeEventListener("click", closeSourceContextMenu);
      window.removeEventListener("keydown", handleShortcut);
      if (estimatePulseTimer !== undefined) window.clearTimeout(estimatePulseTimer);
      if (roiAutoSaveTimer !== undefined) window.clearTimeout(roiAutoSaveTimer);
      if (sourceRemovalCompletionTimer !== undefined) window.clearTimeout(sourceRemovalCompletionTimer);
    };
  });
</script>

<svelte:head><title>Free-Train</title></svelte:head>

<main class="app-shell" class:drag-active={dragActive}>
  <header class="topbar">
    <div class="brand-block">
      <div class="brand-mark" aria-hidden="true"><ScanLine size={18} strokeWidth={2.2} /></div>
      <div><strong>Free-Train</strong><span>M4 质量与相似审核</span></div>
    </div>
    <div class="menu-anchor">
      <button class="project-switcher" onclick={() => (projectMenuOpen = !projectMenuOpen)}>
        <span>{project?.name ?? "未打开项目"}</span><ChevronDown size={15} />
      </button>
      {#if projectMenuOpen}
        <div class="command-menu project-menu">
          <button onclick={beginCreateProject}><FolderPlus size={15} /><span>新建项目</span></button>
          <button onclick={chooseProject}><FolderOpen size={15} /><span>打开项目</span></button>
          {#if recentProjectPath && recentProjectPath !== project?.path}
            <button onclick={() => activateProject(recentProjectPath)}><Clock3 size={15} /><span>最近项目</span></button>
          {/if}
        </div>
      {/if}
    </div>
    <div class="toolbar-actions">
      <div class="menu-anchor">
        <button class="command" disabled={!project || !!busyMessage} onclick={() => (importMenuOpen = !importMenuOpen)}><FolderInput size={16} /><span>导入</span><ChevronDown size={13} /></button>
        {#if importMenuOpen}
          <div class="command-menu import-menu">
            <button onclick={chooseFiles}><FileImage size={15} /><span>导入文件</span></button>
            <button onclick={chooseFolder}><FolderTree size={15} /><span>递归导入目录</span></button>
          </div>
        {/if}
      </div>
      <button class="command" disabled={!isActiveVideo || !!videoBusy} onclick={estimateVideoSampling} title="估算当前视频抽帧数量"><CircleGauge size={16} /><span>预估</span></button>
      <button class="command primary" disabled={!project || !!reviewBusy || !!busyMessage} onclick={runReviewAnalysis} title="运行质量与相似分析并进入审核"><Play size={16} fill="currentColor" /><span>快速处理</span></button>
      <button class="command" disabled={!selectedSource} onclick={() => activateSection("export")}><Download size={16} /><span>导出</span></button>
      <button class="icon-button" onclick={toggleTheme} title="切换主题" aria-label="切换主题">{#if theme === "light"}<Moon size={17} />{:else}<Sun size={17} />{/if}</button>
      <button class="icon-button" disabled title="应用设置将在后续里程碑接入" aria-label="设置"><Settings2 size={17} /></button>
    </div>
  </header>

  <aside class="rail" aria-label="主导航">
    {#each sections as section}
      {@const Icon = section.icon}
      <button class:active={activeSection === section.id} class="rail-button" disabled={!enabledSections.has(section.id)} onclick={() => activateSection(section.id)} title={!enabledSections.has(section.id) ? `${section.label}将在后续里程碑接入` : section.label} aria-label={section.label}>
        <Icon size={19} strokeWidth={1.9} /><span>{section.label}</span>
      </button>
    {/each}
  </aside>

  <aside class="source-panel" bind:this={sourcePanel}>
    <div class="panel-heading">
      <div><span class="eyebrow">项目素材</span><h1>{project?.name ?? "尚未打开项目"}</h1></div>
      <button class="icon-button compact" disabled={!project || !!busyMessage} onclick={refreshStatuses} title="刷新素材状态" aria-label="刷新素材状态"><RefreshCw size={15} class={busyMessage.includes("核对") ? "spinning" : ""} /></button>
    </div>
    <label class="search-box"><Search size={15} /><input bind:value={search} placeholder="搜索文件或来源" disabled={!project || sources.length === 0} aria-label="搜索素材" /></label>
    <div class="source-tree">
      {#if !project}
        <button class="empty-tree-action" onclick={beginCreateProject}><FolderPlus size={16} /><span>创建第一个项目</span></button>
      {:else if sources.length === 0}
        <button class="empty-tree-action" onclick={chooseFolder}><FolderInput size={16} /><span>导入素材目录</span></button>
      {:else}
        {#each sourceGroups as [group, items]}
          <div class="tree-group-header" class:checked={sourceGroupChecked(group)}>
            <label class="source-checkbox group-checkbox" title={`选择文件夹 ${group} 内的全部来源`}>
              <input type="checkbox" checked={sourceGroupChecked(group)} onchange={(event) => toggleSourceGroupChecked(group, event.currentTarget.checked)} aria-label={`选择文件夹 ${group} 内的全部来源`} />
            </label>
            <button class="tree-label" class:collapsed={collapsedSourceGroups.has(group)} onclick={() => toggleSourceGroupCollapsed(group)} ondblclick={() => selectSourceGroup(sourceGroupItems(group))} title={`单击展开或收起；双击批量处理来源组 ${group}`}><ChevronDown size={14} /><FolderTree size={14} /><span>{group}</span><small>{sourceGroupItems(group).length}</small></button>
          </div>
          {#if !collapsedSourceGroups.has(group)}
            {#each items as source}
              <div class="source-item" class:checked={checkedSourceIds.has(source.id)}>
                <label class="source-checkbox" title="选择来源">
                  <input type="checkbox" checked={checkedSourceIds.has(source.id)} onchange={(event) => toggleSourceChecked(source.id, event.currentTarget.checked)} aria-label={`选择来源 ${source.fileName}`} />
                </label>
                <button class="source-row" class:selected={source.id === selectedSourceId} onclick={() => selectSource(source.id)} title={source.absolutePath}>
                  {#if source.kind === "video"}<FileVideo size={15} />{:else}<FileImage size={15} />{/if}
                  <span><strong>{source.fileName}</strong><small>{source.sourceIdentifier}</small></span>
                  {#if source.status !== "online"}<AlertCircle size={14} class="source-warning" />{/if}
                </button>
              </div>
            {/each}
          {/if}
        {/each}
        {#if visibleSources.length < filteredSources.length}
          <button class="load-more" onclick={() => (visibleLimit += pageSize)}>加载更多 <span>{filteredSources.length - visibleSources.length}</span></button>
        {/if}
      {/if}
    </div>
    <div class="source-management">
      <div><span>来源管理</span><strong class="mono">已选 {checkedSourceIds.size}</strong></div>
      <div><button onclick={toggleAllSources} disabled={sources.length === 0} title={allSourcesChecked ? "清除全部选择" : "选择项目中的全部来源"}><SquareCheckBig size={13} />{allSourcesChecked ? "清除全选" : "全选"}</button><button onclick={selectOfflineSources} disabled={!sources.some((source) => source.status !== "online")}><AlertCircle size={13} />选中离线</button><button class="danger" onclick={requestRemoveCheckedSources} disabled={!checkedSourceIds.size || !!busyMessage}><Trash2 size={13} />移除选中</button></div>
    </div>
    <div class="source-summary"><span>源素材 / 候选</span><strong>{project?.sourceCount ?? 0} / {project?.candidateCount ?? 0}</strong><span>离线/异常</span><strong class:danger={(project?.offlineCount ?? 0) > 0}>{project?.offlineCount ?? 0}</strong></div>
    {#if sourceContextMenu}
      <div class="command-menu source-context-menu" style:left={`${sourceContextMenu.x}px`} style:top={`${sourceContextMenu.y}px`}>
        <button disabled={!project || !!busyMessage} onclick={refreshStatuses}><RefreshCw size={15} /><span>刷新素材状态</span></button>
      </div>
    {/if}
  </aside>

  <section class="workspace" class:video-workspace={isActiveVideo && !selectedTilePreview && activeSection !== "review"} class:review-workspace={activeSection === "review"}>
    {#if activeSection === "review"}
      <div class="review-toolbar">
        <div class="review-title"><ListChecks size={17} /><div><strong>审核工作台</strong><span>{reviewWorkspace ? `${filteredReviewItems.length} / ${reviewWorkspace.summary.total} 张` : "尚未运行分析"}</span></div></div>
        <label class="review-select-all"><input type="checkbox" checked={visibleReviewItems.length > 0 && visibleReviewItems.every((item) => checkedReviewKeys.has(item.assetKey))} onchange={(event) => toggleAllVisibleReview(event.currentTarget.checked)} /><span>选择当前页</span></label>
        <select bind:value={reviewStatusFilter} aria-label="审核状态筛选">
          <option value="all">全部状态</option><option value="suggested">建议排除</option><option value="excluded">人工排除</option><option value="warning">质量警告</option><option value="keep">保留</option><option value="error">失败</option>
        </select>
        <select bind:value={reviewSourceFilter} aria-label="来源筛选"><option value="all">全部来源</option>{#each reviewSourceOptions as source}<option value={source}>{source}</option>{/each}</select>
        <select bind:value={reviewGroupFilter} aria-label="相似组筛选"><option value="all">全部相似组</option>{#each reviewGroupOptions as group}<option value={group}>{group.replace("sim-", "组 ")}</option>{/each}</select>
        <span class="review-toolbar-spacer"></span>
        <button onclick={() => applyReviewAction("keep")} disabled={!checkedReviewKeys.size && !selectedReviewKey}><Check size={14} />保留</button>
        <button onclick={() => applyReviewAction("exclude")} disabled={!checkedReviewKeys.size && !selectedReviewKey}><X size={14} />排除</button>
        <button onclick={() => applyReviewAction("restore")} disabled={!checkedReviewKeys.size && !selectedReviewKey}><RotateCcw size={14} />恢复</button>
      </div>
      <div class="review-summary-strip">
        <span><strong>{reviewWorkspace?.summary.keep ?? 0}</strong> 保留</span>
        <span class="suggested"><strong>{reviewWorkspace?.summary.suggestedExclude ?? 0}</strong> 建议排除</span>
        <span class="excluded"><strong>{reviewWorkspace?.summary.manuallyExcluded ?? 0}</strong> 人工排除</span>
        <span class="warning"><strong>{reviewWorkspace?.summary.warning ?? 0}</strong> 警告</span>
        <span><strong>{reviewWorkspace?.summary.locked ?? 0}</strong> 锁定</span>
        <span><strong>{reviewWorkspace?.summary.similarityGroups ?? 0}</strong> 相似组</span>
      </div>
      <div class="review-grid-shell">
        {#if reviewBusy}
          <div class="review-empty"><LoaderCircle size={28} class="spinning" /><strong>{reviewBusy}</strong><span>自动建议不会覆盖已有人工决定。</span></div>
        {:else if !reviewWorkspace}
          <div class="review-empty"><ScanLine size={30} /><strong>尚未生成审核结果</strong><span>运行质量与相似分析后在这里确认建议排除项。</span><button class="primary" onclick={runReviewAnalysis}><Play size={14} fill="currentColor" />开始分析</button></div>
        {:else if filteredReviewItems.length === 0}
          <div class="review-empty"><Search size={28} /><strong>当前筛选没有结果</strong><span>调整状态、来源或相似组筛选。</span></div>
        {:else}
          <div class="review-grid">
            {#each visibleReviewItems as item}
              {@const status = reviewEffectiveStatus(item)}
              <div class="review-card" class:selected={selectedReviewKey === item.assetKey} class:suggested={status === "suggested"} class:excluded={status === "excluded"} class:warning={status === "warning"} class:error={status === "error"} class:representative={item.representative}>
                <label class="review-checkbox"><input type="checkbox" checked={checkedReviewKeys.has(item.assetKey)} onchange={(event) => toggleReviewChecked(item.assetKey, event.currentTarget.checked)} aria-label={`选择 ${item.displayName}`} /></label>
                <button class="review-image" onclick={() => selectReviewItem(item)} ondblclick={() => item.similarityGroupId && (reviewGroupFilter = item.similarityGroupId)} title={`${item.displayName} · ${reviewStatusLabel(item)}`}>
                  <img src={reviewThumbnailUrl(item)} alt="" />
                  {#if status === "suggested"}<span class="review-wash">建议排除</span>{/if}
                  {#if status === "excluded"}<span class="review-wash manual">人工排除</span>{/if}
                </button>
                <div class="review-badges">
                  {#if item.locked}<span class="locked" title="锁定保留"><Lock size={11} /></span>{/if}
                  {#if item.representative}<span class="representative" title="相似组代表图"><Check size={11} /></span>{/if}
                  {#if item.automaticReasons.length}<span class="quality" title={item.automaticReasons.join("；")}><AlertCircle size={11} /></span>{/if}
                </div>
                <button class="review-caption" onclick={() => selectReviewItem(item)}><strong>{item.sourceIdentifier}</strong><span>{item.videoOffsetMs !== null ? formatTimestamp(item.videoOffsetMs) : item.displayName}</span><small>{reviewStatusLabel(item)}{item.similarityGroupId ? ` · ${item.similarityGroupId.replace("sim-", "组 ")}` : ""}</small></button>
              </div>
            {/each}
          </div>
          {#if visibleReviewItems.length < filteredReviewItems.length}<button class="review-load-more" onclick={() => (reviewVisibleLimit += 400)}>加载更多 {filteredReviewItems.length - visibleReviewItems.length}</button>{/if}
        {/if}
      </div>
    {:else}
    <div class="workspace-toolbar">
      <div class="view-tabs" role="tablist" aria-label="工作视图">
        <button class:active={!selectedTilePreview} role="tab" onclick={() => (selectedTilePreview = null)}><ImageIcon size={15} />源画面</button>
        <button class:active={!!selectedTilePreview} disabled={!selectedTilePreview} role="tab"><LayoutGrid size={15} />切片</button>
      </div>
      <div class="canvas-meta"><span>{selectedTilePreview ? selectedTilePreview.roiName + " · R" + (selectedTilePreview.placement.row + 1) + " C" + (selectedTilePreview.placement.column + 1) : selectedSource?.fileName ?? "无活动素材"}</span><span class="mono">{selectedTilePreview ? selectedTilePreview.placement.outputWidth : selectedSource?.width ?? "--"} × {selectedTilePreview ? selectedTilePreview.placement.outputHeight : selectedSource?.height ?? "--"}</span></div>
    </div>

    <div class="media-canvas" class:offline={selectedSource?.status !== "online" && !!selectedSource}>
      {#if selectedSource && (previewChecking || verifiedSourceId !== selectedSource.id)}
        <div class="empty-media"><div class="empty-media-icon"><LoaderCircle size={30} class="spinning" /></div><h2>正在验证源素材</h2><p>检查路径和内容指纹后再打开预览。</p></div>
      {:else if selectedTilePreview}
        <div class="preview-stage tile-detail-stage" style={"--source-ratio:" + selectedTilePreview.placement.outputWidth / Math.max(selectedTilePreview.placement.outputHeight, 1)}>
          <img class="source-preview" src={convertFileSrc(selectedTilePreview.previewPath)} alt={selectedTilePreview.roiName} />
          <span class="tile-detail-label">{selectedTilePreview.roiName} · R{selectedTilePreview.placement.row + 1} C{selectedTilePreview.placement.column + 1}</span>
        </div>
      {:else if selectedSource?.status === "online"}
        <div
          class="preview-stage"
          class:roi-editing={inspectorTab === "roi"}
          role="group"
          aria-label="ROI 编辑画布"
          style={"--source-ratio:" + Math.max(selectedSource.width ?? 1, 1) / Math.max(selectedSource.height ?? 1, 1)}
          onpointerdown={startRoiDrag}
          onpointermove={moveRoiDrag}
          onpointerup={endRoiDrag}
          onpointercancel={endRoiDrag}
        >
          {#if selectedSource.kind === "image"}
            <img class="source-preview" src={previewUrl} alt={selectedSource.fileName} />
          {:else}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video
              bind:this={videoElement}
              class="source-preview video-preview"
              src={previewUrl}
              preload="auto"
              ontimeupdate={(event) => { currentTimeMs = Math.round(event.currentTarget.currentTime * 1_000); jumpTime = formatTimestamp(currentTimeMs); }}
              onplay={() => (isPlaying = true)}
              onpause={() => (isPlaying = false)}
              onended={() => (isPlaying = false)}
              onloadedmetadata={() => applyPlaybackRate(playbackRate)}
              onclick={togglePlayback}
            ></video>
          {/if}
          {#if inspectorTab === "roi" || activeSection === "export"}
            <div class="roi-layer" aria-label="ROI 与切片坐标">
              {#each roiProfiles as profile}
                <button
                  class="roi-box"
                  class:selected={profile.id === selectedRoiId}
                  style={profile.id === selectedRoiId ? draftRoiStyle() : roiStyle(profile)}
                  onpointerdown={(event) => startRoiMove(event, profile)}
                  onclick={(event) => event.detail === 0 && editRoi(profile)}
                  title={profile.name + " · 拖动以移动 ROI"}
                ><span>{profile.name}</span></button>
              {/each}
              {#if !selectedRoiId}
                <span class="roi-box selected draft-roi" style={draftRoiStyle()}><span>{roiName}</span></span>
              {/if}
              {#each activeTilePreviews as preview}
                <span class="tile-box" style={tileStyle(preview)} title={"R" + (preview.placement.row + 1) + " C" + (preview.placement.column + 1)}></span>
              {/each}
            </div>
          {/if}
        </div>
      {:else}
        <div class="empty-media">
          <div class="empty-media-icon">{#if selectedSource}<AlertCircle size={30} strokeWidth={1.45} />{:else}<Video size={30} strokeWidth={1.45} />{/if}</div>
          <h2>{selectedSource ? "源素材当前不可预览" : project ? "等待源素材" : "从一个本地项目开始"}</h2>
          <p>{selectedSource?.error ?? (project ? "可导入视频、图片或整个目录。" : "项目仅保存引用、参数和缓存，不复制源素材。")}</p>
          {#if selectedSource}<button onclick={relinkSelected}><Link2 size={16} />重新定位</button>{:else if project}<button onclick={chooseFiles}><FolderInput size={16} />导入视频或图片</button>{:else}<button onclick={beginCreateProject}><FolderPlus size={16} />新建项目</button>{/if}
        </div>
      {/if}
      <div class="canvas-corner top-left"></div><div class="canvas-corner top-right"></div><div class="canvas-corner bottom-left"></div><div class="canvas-corner bottom-right"></div>
    </div>

    {#if isActiveVideo && !selectedTilePreview}
      <div class="video-timeline">
        <div class="timeline-ruler">
          <span class="timeline-playhead" style:left={`${currentTimeMs / Math.max(selectedSource?.durationMs ?? 1, 1) * 100}%`}></span>
          {#each videoSelections as selection}
            <button class="range-band" style:left={`${selection.startMs / Math.max(selectedSource?.durationMs ?? 1, 1) * 100}%`} style:width={`${(selection.endMs - selection.startMs) / Math.max(selectedSource?.durationMs ?? 1, 1) * 100}%`} onclick={() => seekTo(selection.startMs)} title={`${selection.label} ${formatTimestamp(selection.startMs)} - ${formatTimestamp(selection.endMs)}`}></button>
          {/each}
          {#each candidates as candidate}
            <button class="candidate-marker" class:pinned={candidate.pinned} style:left={`${candidate.videoOffsetMs / Math.max(selectedSource?.durationMs ?? 1, 1) * 100}%`} onclick={() => seekTo(candidate.videoOffsetMs)} title={`${formatTimestamp(candidate.videoOffsetMs)} ${candidate.selectionMethod}`}></button>
          {/each}
          {#if markInMs !== null}<span class="in-marker" style:left={`${markInMs / Math.max(selectedSource?.durationMs ?? 1, 1) * 100}%`}></span>{/if}
          <input type="range" min="0" max={selectedSource?.durationMs ?? 0} step="1" value={currentTimeMs} oninput={(event) => seekTo(Number(event.currentTarget.value))} aria-label="视频时间轴" />
        </div>
        <div class="transport-bar">
          <button onclick={() => stepFrame(-1)} disabled={!frameTimestamps.length || !!videoBusy} title="上一帧" aria-label="上一帧"><SkipBack size={15} /></button>
          <button class="play-control" onclick={togglePlayback} disabled={!!videoBusy} title={isPlaying ? "暂停" : "播放"} aria-label={isPlaying ? "暂停" : "播放"}>{#if isPlaying}<Pause size={15} fill="currentColor" />{:else}<Play size={15} fill="currentColor" />{/if}</button>
          <button onclick={() => stepFrame(1)} disabled={!frameTimestamps.length || !!videoBusy} title="下一帧" aria-label="下一帧"><SkipForward size={15} /></button>
          <strong class="mono transport-time">{formatTimestamp(currentTimeMs)}</strong>
          <div class="speed-control" aria-label="播放速度">{#each [0.25, 0.5, 1, 2] as rate}<button class:active={playbackRate === rate} onclick={() => applyPlaybackRate(rate)}>{rate}x</button>{/each}</div>
          <label class="jump-control"><input bind:value={jumpTime} onkeydown={(event) => event.key === "Enter" && jumpToInput()} aria-label="跳转时间" /><button onclick={jumpToInput} title="跳转"><Timer size={14} /></button></label>
          <span class="transport-spacer"></span>
          <button onclick={captureCurrentFrame} disabled={!!videoBusy} title="保存当前帧 (C)" aria-label="保存当前帧"><Camera size={15} /></button>
          <button class:active={markInMs !== null} onclick={setRangeIn} disabled={!!videoBusy} title="设置入点 (I)" aria-label="设置入点"><Flag size={15} /></button>
          <button onclick={setRangeOut} disabled={markInMs === null || !!videoBusy} title="设置出点 (O)" aria-label="设置出点"><Flag size={15} fill="currentColor" /></button>
        </div>
      </div>
    {:else}
      <div class="media-statusline">
        <span>{selectedTilePreview ? "切片预览" : selectedSource ? "静态图片" : "--"}</span><span class="timeline-spacer"></span><span class="mono">{selectedTilePreview ? selectedTilePreview.placement.outputWidth + " × " + selectedTilePreview.placement.outputHeight : selectedSource ? formatBytes(selectedSource.sizeBytes) : "--"}</span>
      </div>
    {/if}

    <div class="thumbnail-strip" class:candidate-strip={isActiveVideo || tilePreviews.length > 0} class:estimate-pulse={isActiveVideo && estimatePulse} aria-label={tilePreviews.length ? "切片预览" : isActiveVideo ? "候选图片，按 A/D 或小键盘 4/6 切换" : "源素材缩略图"}>
      {#if tilePreviews.length > 0 && (inspectorTab === "roi" || activeSection === "export")}
        {#each tilePreviews as preview}
          <div class="candidate-item tile-item" class:checked={checkedTileKeys.has(tilePreviewKey(preview))}>
            <label class="candidate-checkbox" title="选择切片">
              <input type="checkbox" checked={checkedTileKeys.has(tilePreviewKey(preview))} onchange={(event) => toggleTileChecked(preview, event.currentTarget.checked)} aria-label={`选择 ${preview.roiName} R${preview.placement.row + 1} C${preview.placement.column + 1}`} />
            </label>
            <button class="candidate-thumbnail tile-thumbnail" class:selected={preview.previewPath === selectedTilePreview?.previewPath} onclick={() => selectTilePreview(preview)} title={preview.roiName + " · 点击查看大图"}>
              <img src={convertFileSrc(preview.previewPath)} alt="" />
              <small>{preview.roiName} · R{preview.placement.row + 1} C{preview.placement.column + 1}</small>
            </button>
          </div>
        {/each}
      {:else if isActiveVideo}
        {#each candidates as candidate}
          <div class="candidate-item" class:checked={checkedCandidateIds.has(candidate.id)}>
            <label class="candidate-checkbox" title="选择候选帧">
              <input type="checkbox" checked={checkedCandidateIds.has(candidate.id)} onchange={(event) => toggleCandidateChecked(candidate.id, event.currentTarget.checked)} aria-label={`选择 ${formatTimestamp(candidate.videoOffsetMs)} 的候选帧`} />
            </label>
            <button class="candidate-thumbnail" class:selected={candidate.id === selectedCandidateId} data-candidate-id={candidate.id} onclick={() => selectCandidate(candidate)} title={`${formatTimestamp(candidate.videoOffsetMs)} · ${candidate.selectionMethod} · A/D 切换`}>
              <img src={candidateThumbnailUrl(candidate)} alt="" /><small class="mono">{formatTimestamp(candidate.videoOffsetMs)}</small>
              {#if candidate.pinned}<span class="pin-badge"><Pin size={11} fill="currentColor" /></span>{/if}
            </button>
          </div>
        {/each}
        {#if candidates.length === 0}<div class="candidate-empty"><Camera size={18} /><span>尚未保存候选帧</span></div>{/if}
      {:else}
        {#each sources.slice(0, 8) as source}
          <button class="source-thumbnail" class:selected={source.id === selectedSourceId} onclick={() => selectSource(source.id)} title={source.fileName}>
            {#if source.thumbnailPath}<img src={thumbnailUrl(source)} alt="" />{:else}<span>{source.kind === "video" ? "VIDEO" : "IMAGE"}</span>{/if}
            <small>{source.fileName}</small>
          </button>
        {/each}
        {#if sources.length === 0}{#each Array(6) as _, index}<div class="thumbnail-placeholder"><span>{String(index + 1).padStart(2, "0")}</span></div>{/each}{/if}
      {/if}
    </div>
    {/if}
  </section>

  <aside class="inspector">
    {#if activeSection === "review"}
      <section class="sampling-heading review-heading">
        <div><span class="eyebrow">质量与相似审核</span><h2>{reviewWorkspace ? `${reviewWorkspace.summary.total} 张审核资产` : "等待分析"}</h2></div>
        <ListChecks size={17} />
      </section>
      {#if reviewBusy}<div class="video-busy"><LoaderCircle size={14} class="spinning" /><span>{reviewBusy}</span></div>{/if}
      <div class="sampling-panel review-settings">
        <div class="subsection-heading"><span>质量阈值</span><Activity size={14} /></div>
        <div class="analysis-grid coordinate-grid">
          <label><span>最小宽度</span><input type="number" min="1" bind:value={reviewMinWidth} /></label>
          <label><span>最小高度</span><input type="number" min="1" bind:value={reviewMinHeight} /></label>
          <label><span>清晰度</span><input type="number" min="0" step="1" bind:value={reviewMinSharpness} /></label>
          <label><span>低信息量 %</span><input type="number" min="0" max="100" bind:value={reviewMaxLowInformation} /></label>
          <label><span>欠曝上限 %</span><input type="number" min="0" max="100" bind:value={reviewMaxUnderexposed} /></label>
          <label><span>过曝上限 %</span><input type="number" min="0" max="100" bind:value={reviewMaxOverexposed} /></label>
        </div>
        <div class="subsection-heading"><span>视觉相似</span><ScanLine size={14} /></div>
        <label class="field-row"><span>比较范围</span><select bind:value={reviewSimilarityScope}><option value="source">同一来源</option><option value="source_group">同一来源组</option><option value="project">整个项目</option></select></label>
        <div class="analysis-grid coordinate-grid">
          <label><span>pHash 距离</span><input type="number" min="0" max="64" bind:value={reviewPhashDistance} /></label>
          <label><span>SSIM %</span><input type="number" min="0" max="100" bind:value={reviewSsimThreshold} /></label>
          <label><span>视频窗口 s</span><input type="number" min="0" bind:value={reviewTimeWindowSeconds} /></label>
        </div>
        {#if reviewSimilarityScope !== "source"}<div class="review-risk"><AlertCircle size={13} /><span>{reviewSimilarityScope === "project" ? "全项目比较可能产生更多误判，并受安全比较上限限制。" : "跨来源比较会扩大近重复候选范围。"}</span></div>{/if}
        <button class="analysis-command" onclick={runReviewAnalysis} disabled={!project || !!reviewBusy}><Play size={14} fill="currentColor" />运行质量与相似分析</button>

        <div class="subsection-heading"><span>当前审核项</span>{#if selectedReviewItem}<strong>{reviewStatusLabel(selectedReviewItem)}</strong>{/if}</div>
        {#if selectedReviewItem}
          <div class="review-detail-preview"><img src={convertFileSrc(selectedReviewItem.imagePath)} alt="" /><span>{selectedReviewItem.displayName}</span></div>
          <div class="review-detail-meta">
            <div><span>尺寸</span><strong>{selectedReviewItem.metrics?.width ?? "--"} × {selectedReviewItem.metrics?.height ?? "--"}</strong></div>
            <div><span>清晰度</span><strong>{selectedReviewItem.metrics?.sharpness.toFixed(1) ?? "--"}</strong></div>
            <div><span>欠曝 / 过曝</span><strong>{selectedReviewItem.metrics ? `${(selectedReviewItem.metrics.underexposedRatio * 100).toFixed(1)}% / ${(selectedReviewItem.metrics.overexposedRatio * 100).toFixed(1)}%` : "--"}</strong></div>
            <div><span>低信息量</span><strong>{selectedReviewItem.metrics ? `${(selectedReviewItem.metrics.lowInformation * 100).toFixed(1)}%` : "--"}</strong></div>
            <div><span>相似分数</span><strong>{selectedReviewItem.similarityScore !== null ? selectedReviewItem.similarityScore.toFixed(4) : "--"}</strong></div>
          </div>
          {#if selectedReviewItem.automaticReasons.length}<div class="review-reasons">{#each selectedReviewItem.automaticReasons as reason}<span><AlertCircle size={11} />{reason}</span>{/each}</div>{/if}
          {#if selectedReviewItem.lockedConflict}<div class="review-risk"><AlertCircle size={13} /><span>该相似组包含多个锁定项，需要人工确认代表图。</span></div>{/if}
          <div class="review-detail-actions">
            <button onclick={() => applyReviewAction(selectedReviewItem.locked ? "unlock" : "lock", [selectedReviewItem.assetKey])}>{#if selectedReviewItem.locked}<Unlock size={13} />解锁{:else}<Lock size={13} />锁定{/if}</button>
            <button onclick={() => applyReviewAction("make_representative", [selectedReviewItem.assetKey])} disabled={!selectedReviewItem.similarityGroupId || selectedReviewItem.representative}><Check size={13} />设为代表图</button>
          </div>
        {:else}<span class="range-empty">从审核网格选择一个项目查看测量值</span>{/if}
      </div>
    {:else}
    <div class="inspector-tabs">
      <button class:active={inspectorTab === "info"} onclick={() => activateInspectorTab("info")}>素材信息</button>
      <button class:active={inspectorTab === "sampling"} disabled={selectedSource?.kind !== "video"} onclick={() => activateInspectorTab("sampling")}>抽帧</button>
      <button class:active={inspectorTab === "roi"} disabled={!selectedSource} onclick={() => activateInspectorTab("roi")}>ROI / 切图</button>
      <button class:active={inspectorTab === "export"} disabled={!selectedSource} onclick={() => activateInspectorTab("export")}>导出</button>
    </div>
    {#if inspectorTab === "export" && selectedSource}
      <section class="sampling-heading">
        <div><span class="eyebrow">基础导出</span><h2>{selectedSource.fileName}</h2></div>
        <Download size={17} />
      </section>
      {#if exportBusy}<div class="video-busy"><LoaderCircle size={14} class="spinning" /><span>{exportBusy}</span></div>{/if}
      <div class="sampling-panel export-panel">
        <div class="field-row export-mode-row"><span>导出内容</span><div class="segmented-control" aria-label="导出内容">
          <button class:active={exportContent === "frames"} onclick={() => setExportContent("frames")} title={selectedSource.kind === "video" ? "导出视频抽帧候选原图" : "导出当前图片原图"}><Film size={13} />{selectedSource.kind === "video" ? "抽帧" : "原图"}</button>
          <button class:active={exportContent === "tiles"} onclick={() => setExportContent("tiles")}><LayoutGrid size={13} />ROI 切片</button>
        </div></div>
        <div class="field-row export-mode-row"><span>导出范围</span><div class="segmented-control" aria-label="导出范围">
          <button class:active={exportSourceScope === "current"} onclick={() => setExportSourceScope("current")}><ImageIcon size={13} />当前素材</button>
          <button class:active={exportSourceScope === "source_group"} onclick={() => setExportSourceScope("source_group")}><FolderTree size={13} />来源组 {selectedSourceGroupCount}</button>
        </div></div>
        {#if selectedSource.kind === "video" && exportSourceScope === "current"}
          <div class="field-row export-mode-row"><span>候选范围</span><div class="segmented-control" aria-label="候选范围">
            <button class:active={exportCandidateScope === "all"} onclick={() => setExportCandidateScope("all")}><Images size={13} />全部 {candidates.length}</button>
            <button class:active={exportCandidateScope === "selected"} disabled={!selectedCandidateId} onclick={() => setExportCandidateScope("selected")}><ImageIcon size={13} />当前帧</button>
          </div></div>
        {/if}
        <label class="field-row"><span>导出目录</span><button class="inline-picker" onclick={chooseExportDirectory}><FolderOutput size={14} /><span>{exportDirectory || "选择目录"}</span></button></label>
        <label class="field-row"><span>命名模板</span><input bind:value={namingTemplate} oninput={() => (exportPlan = null)} /></label>
        <label class="field-row"><span>图片格式</span><select bind:value={exportFormat} onchange={() => (exportPlan = null)}>
          <option value="jpeg">JPEG</option><option value="png">PNG</option><option value="webp">WebP</option>
        </select></label>
        <label class="field-row"><span>重名策略</span><select bind:value={conflictStrategy} onchange={() => (exportPlan = null)}>
          <option value="append_sequence">追加序号</option><option value="append_hash">追加短哈希</option>
          <option value="skip">跳过</option><option value="fail">导出前失败</option>
        </select></label>
        <div class="template-fields"><span>字段</span><code>{"{source}"}</code><code>{"{source_group}"}</code><code>{"{timestamp_ms}"}</code>{#if exportContent === "tiles"}<code>{"{roi}"}</code><code>{"{row}"}</code><code>{"{col}"}</code>{/if}<code>{"{index}"}</code></div>
        {#if exportPlan}
          <div class="export-summary">
            <div><span>成品</span><strong>{exportPlan.items.length}</strong></div>
            <div><span>跳过</span><strong>{exportPlan.skipped}</strong></div>
            <div><span>估算</span><strong>{formatBytes(exportPlan.estimatedBytes)}</strong></div>
          </div>
          <div class="filename-preview">
            {#each exportPlan.items.slice(0, 8) as item}<span>{item.fileName}</span>{/each}
            {#if exportPlan.items.length > 8}<small>另有 {exportPlan.items.length - 8} 个文件名</small>{/if}
          </div>
        {/if}
      </div>
      <div class="sampling-actions"><button onclick={previewExportPlan} disabled={!exportDirectory || !!exportBusy}><CircleGauge size={14} />检查计划</button><button class="primary" onclick={executeExport} disabled={!exportPlan || !!exportBusy}><Download size={14} />确认并导出</button></div>
    {:else if inspectorTab === "sampling" && selectedSource?.kind === "video"}
      <section class="sampling-heading">
        <div><span class="eyebrow">视频人工筛查</span><h2>{selectedSource.fileName}</h2></div>
        <span class="mono">{frameTimestamps.length} 帧</span>
      </section>
      {#if videoBusy}<div class="video-busy"><LoaderCircle size={14} class="spinning" /><span>{videoBusy}</span></div>{/if}
      <div class="sampling-panel">
        <label class="field-row"><span>抽帧模式</span><select bind:value={samplingMode} onchange={() => (samplingEstimate = null)}>
          <option value="fixed_interval">固定时间间隔</option><option value="frame_interval">固定帧间隔</option>
          <option value="target_count">目标数量</option><option value="valid_ranges">有效片段</option>
          <option value="change_triggered">画面变化触发</option>
        </select></label>
        {#if samplingMode === "fixed_interval" || samplingMode === "valid_ranges"}
          <label class="field-row"><span>时间间隔</span><div class="number-unit"><input type="number" min="1" bind:value={intervalMs} /><small>ms</small></div></label>
        {:else if samplingMode === "frame_interval"}
          <label class="field-row"><span>帧间隔</span><div class="number-unit"><input type="number" min="1" bind:value={frameInterval} /><small>帧</small></div></label>
        {:else if samplingMode === "target_count"}
          <label class="field-row"><span>目标数量</span><div class="number-unit"><input type="number" min="1" max="100000" bind:value={targetCount} /><small>张</small></div></label>
        {/if}
        <label class="toggle-row"><input type="checkbox" bind:checked={pinBatchResults} /><span>锁定本次批量候选</span></label>
        <label class="toggle-row"><input type="checkbox" bind:checked={applySourceGroup} disabled={["valid_ranges", "change_triggered"].includes(samplingMode)} /><span>应用到来源组“{selectedSource.sourceGroup}”</span></label>

        <div class="candidate-management">
          <div><span>候选管理</span><strong class="mono">已选 {checkedCandidateCount} / {candidates.length}</strong></div>
          <div class="candidate-management-actions">
            <button onclick={() => clearCandidates("selected")} disabled={checkedCandidateCount === 0 || !!videoBusy}><ListX size={13} />清空选中</button>
            <button class="danger" onclick={() => clearCandidates("all")} disabled={candidates.length === 0 || !!videoBusy}><Trash2 size={13} />清空全部</button>
          </div>
        </div>

        <div class="subsection-heading"><span>有效片段</span><strong>{videoSelections.length}</strong></div>
        <label class="toggle-row compact-toggle"><input type="checkbox" bind:checked={protectNewRange} /><span>新片段生成的候选默认锁定</span></label>
        <div class="range-list">
          {#each videoSelections as selection}
            <div><button class="range-jump" onclick={() => seekTo(selection.startMs)}><Flag size={13} fill={selection.protected ? "currentColor" : "none"} /><span><strong>{selection.label}</strong><small class="mono">{formatTimestamp(selection.startMs)} - {formatTimestamp(selection.endMs)}</small></span></button><button onclick={() => removeSelection(selection.id)} title="删除片段" aria-label="删除片段"><Trash2 size={13} /></button></div>
          {/each}
          {#if videoSelections.length === 0}<span class="range-empty">使用时间轴入点和出点添加片段</span>{/if}
        </div>

        <div class="subsection-heading"><span>画面变化</span><div class="heading-tools"><Activity size={14} /><button onclick={() => (changeChartExpanded = true)} disabled={!changeAnalysis} title="放大变化曲线" aria-label="放大变化曲线"><Maximize2 size={13} /></button></div></div>
        <div class="analysis-grid">
          <label><span>分析 FPS</span><input type="number" min="0.1" max="30" step="0.1" bind:value={analysisFps} /></label>
          <label><span>阈值</span><input type="number" min="0" max="1" step="0.01" bind:value={changeThreshold} /></label>
          <label><span>最小间隔 ms</span><input type="number" min="1" bind:value={minChangeIntervalMs} /></label>
          <label><span>最大间隔 ms</span><input type="number" min="1" bind:value={maxChangeIntervalMs} /></label>
        </div>
        <button class="analysis-command" onclick={analyzeVideoChanges} disabled={!!videoBusy}><Activity size={14} />分析画面变化</button>
        {#if changeAnalysis}
          <div class="change-chart">
            <svg viewBox="0 0 320 104" role="img" aria-label="相邻分析帧的归一化画面变化分数，横轴为视频时间，纵轴为变化分数">
              <g class="chart-grid">
                <line x1="42" y1="10" x2="308" y2="10" /><line x1="42" y1="40" x2="308" y2="40" /><line x1="42" y1="70" x2="308" y2="70" />
              </g>
              <g class="chart-axes"><line x1="42" y1="10" x2="42" y2="70" /><line x1="42" y1="70" x2="308" y2="70" /></g>
              <line class="chart-threshold" x1="42" y1={changeThresholdY} x2="308" y2={changeThresholdY} />
              <text class="chart-threshold-label" x="304" y={Math.max(9, changeThresholdY - 3)} text-anchor="end">阈值 {changeThreshold.toFixed(2)}</text>
              <polyline points={changePolyline} />
              <g class="chart-labels">
                <text x="37" y="73" text-anchor="end">0</text><text x="37" y="43" text-anchor="end">{(changeChartMaxScore / 2).toFixed(2)}</text><text x="37" y="13" text-anchor="end">{changeChartMaxScore.toFixed(2)}</text>
                <text x="42" y="83" text-anchor="middle">0:00</text><text x="175" y="83" text-anchor="middle">{formatChartTime(changeChartMaxTimestamp / 2)}</text><text x="308" y="83" text-anchor="middle">{formatChartTime(changeChartMaxTimestamp)}</text>
                <text class="chart-axis-title" x="175" y="98" text-anchor="middle">视频时间</text><text class="chart-axis-title" x="9" y="40" text-anchor="middle" transform="rotate(-90 9 40)">变化分数</text>
              </g>
            </svg>
            <span>相邻分析帧的归一化视觉差异 · {changeAnalysis.suggestedTimestampsMs.length} 个建议时间点</span>
          </div>
        {/if}

        {#if samplingEstimate}
          <div class="estimate-result" class:estimate-pulse={estimatePulse}><span>{estimatedSourceCount} 个视频预计候选</span><strong>{samplingEstimate.estimatedCount} 张</strong><small>{samplingEstimate.timestampsMs.length ? samplingEstimate.timestampsMs.slice(0, 3).map(formatTimestamp).join(" · ") : `来源组 ${selectedSource.sourceGroup}`}</small></div>
        {/if}
      </div>
      <div class="sampling-actions"><button onclick={estimateVideoSampling} disabled={!!videoBusy}><CircleGauge size={14} />预估</button><button class="primary" onclick={runVideoSampling} disabled={!!videoBusy || (samplingMode === "change_triggered" && !changeAnalysis)}><Play size={14} fill="currentColor" />执行抽帧</button></div>
    {:else if inspectorTab === "roi" && selectedSource}
      <section class="sampling-heading">
        <div><span class="eyebrow">ROI 与固定尺寸切图</span><h2>{selectedSource.fileName}</h2></div>
        <button class="icon-button compact" onclick={beginNewRoi} title="新建 ROI" aria-label="新建 ROI"><Plus size={15} /></button>
      </section>
      {#if roiBusy}<div class="video-busy"><LoaderCircle size={14} class="spinning" /><span>{roiBusy}</span></div>{/if}
      <div class="sampling-panel roi-panel">
        <div class="roi-profile-list">
          {#each roiProfiles as profile}
            <button class:selected={profile.id === selectedRoiId} onclick={() => editRoi(profile)}>
              <Crop size={14} /><span><strong>{profile.name}</strong><small>{profile.inherited ? "来源组预设" : "来源覆盖"} · {profile.roi.width} × {profile.roi.height}</small></span>
            </button>
          {/each}
          {#if roiProfiles.length === 0}<span class="range-empty">尚未配置 ROI</span>{/if}
        </div>
        {#if tilePreviewTotal > 0}
          <div class="candidate-management tile-management">
            <div><span>切片预览管理</span><strong class="mono">已选 {checkedTileCount} · 可见 {tilePreviews.length} / 总计 {tilePreviewTotal}</strong></div>
            <div class="candidate-management-actions"><button onclick={() => clearTilePreviews("selected")} disabled={checkedTileCount === 0 || !!roiBusy}><ListX size={13} />清空选中</button><button class="danger" onclick={() => clearTilePreviews("all")} disabled={!tilePreviews.length || !!roiBusy}><Trash2 size={13} />清空全部</button></div>
            {#if excludedTiles.size}<small>已排除切片会从本次导出计划跳过，重新生成预览可恢复。</small>{/if}
          </div>
        {/if}
        <label class="field-row"><span>ROI 名称</span><input bind:value={roiName} /></label>
        <label class="field-row"><span>应用范围</span><select bind:value={roiScope}>
          <option value="source_group">来源组 {selectedSource.sourceGroup}</option><option value="source">仅此来源</option>
        </select></label>
        <div class="subsection-heading"><span>逻辑画面坐标</span><Crop size={14} /></div>
        <div class="analysis-grid coordinate-grid">
          <label><span>X</span><input type="number" min="0" bind:value={roiX} /></label>
          <label><span>Y</span><input type="number" min="0" bind:value={roiY} /></label>
          <label><span>宽</span><input type="number" min="1" bind:value={roiWidth} /></label>
          <label><span>高</span><input type="number" min="1" bind:value={roiHeight} /></label>
        </div>
        <div class="subsection-heading"><span>固定切片</span><LayoutGrid size={14} /></div>
        <div class="analysis-grid coordinate-grid">
          <label><span>切片宽</span><input type="number" min="1" bind:value={tileWidth} /></label>
          <label><span>切片高</span><input type="number" min="1" bind:value={tileHeight} /></label>
          <label><span>横向重叠 %</span><input type="number" min="0" max="99" bind:value={overlapXPercent} /></label>
          <label><span>纵向重叠 %</span><input type="number" min="0" max="99" bind:value={overlapYPercent} /></label>
        </div>
        <label class="field-row"><span>边缘策略</span><select bind:value={edgeStrategy}>
          <option value="shift_to_edge">贴边覆盖</option><option value="pad">保留并填充</option><option value="discard">丢弃不足切片</option>
        </select></label>
        <label class="field-row"><span>填充方式</span><select bind:value={paddingMode}>
          <option value="constant">常量颜色</option><option value="edge">边缘复制</option><option value="reflect">反射</option>
        </select></label>
        {#if paddingMode === "constant"}<label class="field-row"><span>填充颜色</span><input class="color-input" type="color" bind:value={fillColor} /></label>{/if}
        <label class="field-row"><span>缩放策略</span><select bind:value={resizeMode}>
          <option value="stretch">固定宽高</option><option value="fit">适配留边</option><option value="fill">填满裁切</option><option value="long_side">长边适配</option>
        </select></label>
      </div>
      <div class="roi-actions">
        <button onclick={removeRoi} disabled={!selectedRoiId || !!roiBusy} title="删除 ROI"><Trash2 size={14} /></button>
        <button onclick={() => saveRoi()} disabled={!roiName.trim() || !!roiBusy}><Save size={14} />保存</button>
        <button class="primary" onclick={() => saveRoi({ previewAfter: true })} disabled={!roiName.trim() || !!roiBusy}><LayoutGrid size={14} />保存并预览</button>
      </div>
    {:else if selectedSource}
      <section class="asset-heading">
        <div class="asset-kind">{#if selectedSource.kind === "video"}<Film size={18} />{:else}<ImageIcon size={18} />{/if}</div>
        <div><span class="eyebrow">{selectedSource.kind === "video" ? "视频源素材" : "图片源素材"}</span><h2>{selectedSource.fileName}</h2></div>
        <span class="status-badge" class:online={selectedSource.status === "online"} class:offline={selectedSource.status !== "online"}>{selectedSource.status === "online" ? "在线" : selectedSource.status === "offline" ? "离线" : "异常"}</span>
      </section>
      <div class="metadata-list">
        <div><span><FolderTree size={14} />来源组</span><strong>{selectedSource.sourceGroup}</strong></div>
        <div><span><Link2 size={14} />来源标识</span><strong>{selectedSource.sourceIdentifier}</strong></div>
        <div><span><ImageIcon size={14} />显示尺寸</span><strong class="mono">{selectedSource.width ?? "--"} × {selectedSource.height ?? "--"}</strong></div>
        {#if selectedSource.kind === "video"}
          <div><span><Clock3 size={14} />时长</span><strong class="mono">{formatDuration(selectedSource.durationMs)}</strong></div>
          <div><span><Film size={14} />编码 / 帧率</span><strong>{selectedSource.codec ?? "--"} · {selectedSource.frameRate ?? "--"}</strong></div>
        {:else}
          <div><span><RefreshCw size={14} />EXIF 方向</span><strong class="mono">{selectedSource.orientation ?? "--"}</strong></div>
        {/if}
        <div><span><HardDrive size={14} />文件大小</span><strong class="mono">{formatBytes(selectedSource.sizeBytes)}</strong></div>
        <div><span><Clock3 size={14} />拍摄时间</span><strong>{selectedSource.captureTime ?? "未提供"}</strong></div>
        <div><span><Hash size={14} />快速指纹</span><strong class="mono hash-value">{selectedSource.quickFingerprint.slice(0, 16)}</strong></div>
        <div><span><Hash size={14} />完整 SHA-256</span><strong class="mono hash-value">{selectedSource.sha256?.slice(0, 16) ?? "大文件待后台计算"}</strong></div>
      </div>
      <div class="path-block"><span>原地引用路径</span><code>{selectedSource.absolutePath}</code></div>
      {#if selectedSource.status !== "online"}<button class="inspector-command" onclick={relinkSelected}><Link2 size={15} />重新定位源素材</button>{/if}
    {:else}
      <div class="inspector-empty"><MoreHorizontal size={22} /><span>选择一个源素材查看元数据</span></div>
    {/if}
    {/if}
  </aside>

  <footer class="statusbar">
    <div class="status-left"><span class="status-dot" class:ok={!!project && !busyMessage && !videoBusy && !roiBusy && !exportBusy && !reviewBusy} class:error={messageKind === "error"}></span><span>{busyMessage || videoBusy || roiBusy || exportBusy || reviewBusy || message || (project ? "项目已就绪" : "未打开项目")}</span></div>
    <div class="shortcut-preview" aria-label="当前工作区快捷键">
      {#each shortcutHints as hint}<span><kbd>{hint[0]}</kbd>{hint[1]}</span>{/each}
    </div>
    <div class="status-right"><span>源素材 {project?.sourceCount ?? 0}</span><span class:estimate-status={estimatePulse}>{estimatePulse && samplingEstimate ? `预计候选 ${samplingEstimate.estimatedCount}` : `候选 ${project?.candidateCount ?? 0}`}</span><span>任务 0</span></div>
  </footer>

  {#if dragActive}<div class="drop-overlay"><FolderInput size={38} /><strong>{project ? "松开以导入源素材" : "请先创建或打开项目"}</strong><span>支持文件和递归目录</span></div>{/if}

  {#if sourceRemovalProgress}
    <section class="source-removal-progress" role="status" aria-live="polite">
      <header><LoaderCircle size={17} class="spinning" /><strong>正在移除项目来源</strong></header>
      <p>仅清理项目引用与缓存，原始图片和视频保持不变。</p>
      <div class="source-progress-track"><span style:width={`${sourceRemovalProgress.total ? sourceRemovalProgress.completed / sourceRemovalProgress.total * 100 : 0}%`}></span></div>
      <footer><span>{sourceRemovalProgress.completed} / {sourceRemovalProgress.total}</span><span>来源 {sourceRemovalProgress.deleted} · 候选 {sourceRemovalProgress.candidateDeleted}</span></footer>
    </section>
  {:else if sourceRemovalCompletion}
    <section class="source-removal-complete" role="status" aria-live="polite"><Check size={21} /><div><strong>项目来源已移除</strong><span>已移除 {sourceRemovalCompletion.deleted} 个来源和 {sourceRemovalCompletion.candidateDeleted} 个候选；原始文件未删除。</span></div></section>
  {/if}

  {#if sourceRemovalDialogOpen}
    <div class="modal-backdrop" role="presentation">
      <div class="project-dialog source-removal-dialog" role="dialog" aria-modal="true" aria-labelledby="remove-sources-title">
        <header>
          <div><span class="eyebrow">来源管理</span><h2 id="remove-sources-title">移除选中的 {pendingSourceRemovalIds.length} 个来源</h2></div>
          <button class="icon-button compact" onclick={cancelRemoveCheckedSources} title="取消" aria-label="取消移除"><X size={16} /></button>
        </header>
        <div class="source-removal-warning"><AlertCircle size={22} /><div><strong>只移除项目引用和项目缓存</strong><span>磁盘上的原始图片和视频不会被删除。移除后可以再次导入这些文件。</span></div></div>
        <p>关联的视频候选、来源级 ROI、质量与审核记录会一并从当前项目中清理。来源组 ROI 预设会保留。</p>
        <footer>
          <button class="dialog-secondary" onclick={cancelRemoveCheckedSources}>取消</button>
          <button class="dialog-danger" onclick={confirmRemoveCheckedSources}><Trash2 size={14} />确认移除</button>
        </footer>
      </div>
    </div>
  {/if}

  {#if createDialogOpen}
    <div class="modal-backdrop" role="presentation">
      <div class="project-dialog" role="dialog" aria-modal="true" aria-labelledby="create-project-title">
        <header><div><span class="eyebrow">本地项目</span><h2 id="create-project-title">新建 Free-Train 项目</h2></div><button class="icon-button compact" onclick={() => (createDialogOpen = false)} aria-label="关闭"><X size={16} /></button></header>
        <label><span>项目名称</span><input bind:value={createName} placeholder="例如：cam1 夏季素材" /></label>
        <label><span>保存位置</span><button class="path-picker" onclick={chooseCreateParent}><FolderOpen size={15} /><span>{createParent || "选择一个本地文件夹"}</span><ChevronRight size={14} /></button></label>
        <p>将创建一个 <code>.ftproj</code> 目录。源视频和图片仍保留在原位置。</p>
        <footer><button class="dialog-secondary" onclick={() => (createDialogOpen = false)}>取消</button><button class="dialog-primary" disabled={!createParent || !createName.trim() || !!busyMessage} onclick={createProject}>{#if busyMessage}<LoaderCircle size={15} class="spinning" />{:else}<Check size={15} />{/if}创建项目</button></footer>
      </div>
    </div>
  {/if}

  {#if changeChartExpanded && changeAnalysis}
    <div class="modal-backdrop chart-backdrop" role="presentation">
      <div class="chart-dialog" role="dialog" aria-modal="true" aria-labelledby="change-chart-title">
        <header><div><span class="eyebrow">视频变化分析</span><h2 id="change-chart-title">画面变化曲线</h2></div><button class="icon-button compact" onclick={() => (changeChartExpanded = false)} aria-label="关闭变化曲线"><X size={17} /></button></header>
        <div class="change-chart expanded-chart">
          <svg viewBox="0 0 320 104" role="img" aria-label="放大的画面变化分数曲线">
            <g class="chart-grid">
              <line x1="42" y1="10" x2="308" y2="10" /><line x1="42" y1="40" x2="308" y2="40" /><line x1="42" y1="70" x2="308" y2="70" />
            </g>
            <g class="chart-axes"><line x1="42" y1="10" x2="42" y2="70" /><line x1="42" y1="70" x2="308" y2="70" /></g>
            <line class="chart-threshold" x1="42" y1={changeThresholdY} x2="308" y2={changeThresholdY} />
            <text class="chart-threshold-label" x="304" y={Math.max(9, changeThresholdY - 3)} text-anchor="end">阈值 {changeThreshold.toFixed(2)}</text>
            <polyline points={changePolyline} />
            <g class="chart-labels">
              <text x="37" y="73" text-anchor="end">0</text><text x="37" y="43" text-anchor="end">{(changeChartMaxScore / 2).toFixed(2)}</text><text x="37" y="13" text-anchor="end">{changeChartMaxScore.toFixed(2)}</text>
              <text x="42" y="83" text-anchor="middle">0:00</text><text x="175" y="83" text-anchor="middle">{formatChartTime(changeChartMaxTimestamp / 2)}</text><text x="308" y="83" text-anchor="middle">{formatChartTime(changeChartMaxTimestamp)}</text>
              <text class="chart-axis-title" x="175" y="98" text-anchor="middle">视频时间</text><text class="chart-axis-title" x="9" y="40" text-anchor="middle" transform="rotate(-90 9 40)">变化分数</text>
            </g>
          </svg>
          <span>相邻分析帧的归一化视觉差异 · {changeAnalysis.suggestedTimestampsMs.length} 个建议时间点</span>
        </div>
        <footer><SquareCheckBig size={15} /><span>阈值 {changeThreshold.toFixed(2)}</span><span>建议时间点 {changeAnalysis.suggestedTimestampsMs.length}</span><span>分析点 {changeAnalysis.points.length}</span></footer>
      </div>
    </div>
  {/if}
</main>
