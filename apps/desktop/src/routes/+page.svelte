<script lang="ts">
  import { convertFileSrc, invoke } from "@tauri-apps/api/core";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { open } from "@tauri-apps/plugin-dialog";
  import {
    Activity,
    AlertCircle,
    Camera,
    Check,
    ChevronDown,
    ChevronRight,
    CircleGauge,
    Clock3,
    Download,
    FileImage,
    FileVideo,
    Film,
    Flag,
    FolderInput,
    FolderOpen,
    FolderPlus,
    FolderTree,
    HardDrive,
    Hash,
    Image as ImageIcon,
    Images,
    LayoutGrid,
    Link2,
    ListChecks,
    LoaderCircle,
    Moon,
    MoreHorizontal,
    Pause,
    Pin,
    Play,
    RefreshCw,
    ScanLine,
    Search,
    Settings2,
    SkipBack,
    SkipForward,
    SlidersHorizontal,
    Sun,
    Timer,
    Trash2,
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
  type ChangePoint = { timestampMs: number; score: number };
  type ChangeAnalysis = { points: ChangePoint[]; suggestedTimestampsMs: number[] };

  const sections = [
    { id: "sources", label: "素材", icon: Images },
    { id: "process", label: "处理", icon: SlidersHorizontal },
    { id: "review", label: "审核", icon: ListChecks },
    { id: "export", label: "导出", icon: Download },
    { id: "jobs", label: "任务", icon: CircleGauge },
  ] as const;

  const mediaFilters = [{ name: "视频与图片", extensions: ["jpg", "jpeg", "png", "bmp", "tif", "tiff", "webp", "mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "m2ts"] }];
  const pageSize = 500;
  const enabledSections = new Set(["sources", "process"]);

  let activeSection = $state("sources");
  let theme = $state<"light" | "dark">("light");
  let project = $state<ProjectSummary | null>(null);
  let recentProjectPath = $state("");
  let sources = $state<SourceAsset[]>([]);
  let selectedSourceId = $state("");
  let search = $state("");
  let busyMessage = $state("");
  let message = $state("");
  let messageKind = $state<"info" | "error">("info");
  let projectMenuOpen = $state(false);
  let importMenuOpen = $state(false);
  let createDialogOpen = $state(false);
  let createParent = $state("");
  let createName = $state("");
  let dragActive = $state(false);
  let visibleLimit = $state(pageSize);
  let verifiedSourceId = $state("");
  let previewChecking = $state(false);
  let sourceContextMenu = $state<{ x: number; y: number } | null>(null);
  let sourcePanel: HTMLElement;
  let videoElement = $state<HTMLVideoElement>();
  const checkingSourceIds = new Set<string>();
  let inspectorTab = $state<"info" | "sampling">("info");
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
  let analysisFps = $state(2);
  let changeThreshold = $state(0.08);
  let minChangeIntervalMs = $state(500);
  let maxChangeIntervalMs = $state(5_000);
  let estimatePulse = $state(false);
  let estimatePulseTimer: number | undefined;

  const selectedSource = $derived(sources.find((source) => source.id === selectedSourceId) ?? null);
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

  function setMessage(text: string, kind: "info" | "error" = "info") {
    message = text;
    messageKind = kind;
  }

  function errorText(error: unknown) {
    return error instanceof Error ? error.message : String(error);
  }

  function thumbnailUrl(source: SourceAsset) {
    return source.thumbnailPath ? convertFileSrc(source.thumbnailPath) : "";
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
    seekTo(candidate.videoOffsetMs);
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(`[data-candidate-id="${candidate.id}"]`)?.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "center" });
    });
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
    samplingEstimate = null;
    changeAnalysis = null;
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
      project = await invoke<ProjectSummary | null>("get_current_project");
      setMessage(`新增 ${result.created} 个，已有 ${result.existing} 个${result.failures.length ? `，失败 ${result.failures.length} 个` : ""}`, result.failures.length ? "error" : "info");
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
    if (sectionId === "process" && selectedSource?.kind === "video") inspectorTab = "sampling";
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
    if (source?.kind === "video" && source.status === "online") await loadVideoWorkspace(sourceId);
    else resetVideoWorkspace();
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
    const handleVideoShortcut = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (!isActiveVideo || target?.matches("input, textarea, select, [contenteditable='true']")) return;
      if (event.code === "Space") {
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
    window.addEventListener("keydown", handleVideoShortcut);

    let unlisten: (() => void) | undefined;
    void getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "enter" || event.payload.type === "over") dragActive = true;
      if (event.payload.type === "leave") dragActive = false;
      if (event.payload.type === "drop") {
        dragActive = false;
        if (project) void importPaths(event.payload.paths);
        else setMessage("请先创建或打开项目，再导入素材", "error");
      }
    }).then((stop) => (unlisten = stop));

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
      window.clearInterval(statusTimer);
      window.removeEventListener("focus", verifySelected);
      sourcePanel.removeEventListener("contextmenu", showSourceContextMenu);
      window.removeEventListener("click", closeSourceContextMenu);
      window.removeEventListener("keydown", handleVideoShortcut);
      if (estimatePulseTimer !== undefined) window.clearTimeout(estimatePulseTimer);
    };
  });
</script>

<svelte:head><title>Free-Train</title></svelte:head>

<main class="app-shell" class:drag-active={dragActive}>
  <header class="topbar">
    <div class="brand-block">
      <div class="brand-mark" aria-hidden="true"><ScanLine size={18} strokeWidth={2.2} /></div>
      <div><strong>Free-Train</strong><span>M2 视频筛查工作区</span></div>
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
      <button class="command primary" disabled title="快速处理将在后续里程碑接入"><Play size={16} fill="currentColor" /><span>快速处理</span></button>
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
          <div class="tree-label"><ChevronDown size={14} /><FolderTree size={14} /><span>{group}</span><small>{items.length}</small></div>
          {#each items as source}
            <button class="source-row" class:selected={source.id === selectedSourceId} onclick={() => selectSource(source.id)} title={source.absolutePath}>
              {#if source.kind === "video"}<FileVideo size={15} />{:else}<FileImage size={15} />{/if}
              <span><strong>{source.fileName}</strong><small>{source.sourceIdentifier}</small></span>
              {#if source.status !== "online"}<AlertCircle size={14} class="source-warning" />{/if}
            </button>
          {/each}
        {/each}
        {#if visibleSources.length < filteredSources.length}
          <button class="load-more" onclick={() => (visibleLimit += pageSize)}>加载更多 <span>{filteredSources.length - visibleSources.length}</span></button>
        {/if}
      {/if}
    </div>
    <div class="source-summary"><span>源素材 / 候选</span><strong>{project?.sourceCount ?? 0} / {project?.candidateCount ?? 0}</strong><span>离线/异常</span><strong class:danger={(project?.offlineCount ?? 0) > 0}>{project?.offlineCount ?? 0}</strong></div>
    {#if sourceContextMenu}
      <div class="command-menu source-context-menu" style:left={`${sourceContextMenu.x}px`} style:top={`${sourceContextMenu.y}px`}>
        <button disabled={!project || !!busyMessage} onclick={refreshStatuses}><RefreshCw size={15} /><span>刷新素材状态</span></button>
      </div>
    {/if}
  </aside>

  <section class="workspace" class:video-workspace={isActiveVideo}>
    <div class="workspace-toolbar">
      <div class="view-tabs" role="tablist" aria-label="工作视图"><button class="active" role="tab"><ImageIcon size={15} />预览</button><button disabled role="tab"><LayoutGrid size={15} />缩略图</button></div>
      <div class="canvas-meta"><span>{selectedSource?.fileName ?? "无活动素材"}</span><span class="mono">{selectedSource?.width ?? "--"} × {selectedSource?.height ?? "--"}</span></div>
    </div>

    <div class="media-canvas" class:offline={selectedSource?.status !== "online" && !!selectedSource}>
      {#if selectedSource && (previewChecking || verifiedSourceId !== selectedSource.id)}
        <div class="empty-media"><div class="empty-media-icon"><LoaderCircle size={30} class="spinning" /></div><h2>正在验证源素材</h2><p>检查路径和内容指纹后再打开预览。</p></div>
      {:else if selectedSource?.status === "online" && selectedSource.kind === "image"}
        <img class="source-preview" src={previewUrl} alt={selectedSource.fileName} />
      {:else if selectedSource?.status === "online" && selectedSource.kind === "video"}
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

    {#if isActiveVideo}
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
        <span>{selectedSource ? "静态图片" : "--"}</span><span class="timeline-spacer"></span><span class="mono">{selectedSource ? formatBytes(selectedSource.sizeBytes) : "--"}</span>
      </div>
    {/if}

    <div class="thumbnail-strip" class:candidate-strip={isActiveVideo} class:estimate-pulse={isActiveVideo && estimatePulse} aria-label={isActiveVideo ? "候选图片，按 A/D 或小键盘 4/6 切换" : "源素材缩略图"}>
      {#if isActiveVideo}
        {#each candidates as candidate}
          <button class="candidate-thumbnail" class:selected={candidate.id === selectedCandidateId} data-candidate-id={candidate.id} onclick={() => selectCandidate(candidate)} title={`${formatTimestamp(candidate.videoOffsetMs)} · ${candidate.selectionMethod} · A/D 切换`}>
            <img src={candidateThumbnailUrl(candidate)} alt="" /><small class="mono">{formatTimestamp(candidate.videoOffsetMs)}</small>
            {#if candidate.pinned}<span class="pin-badge"><Pin size={11} fill="currentColor" /></span>{/if}
          </button>
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
  </section>

  <aside class="inspector">
    <div class="inspector-tabs">
      <button class:active={inspectorTab === "info"} onclick={() => (inspectorTab = "info")}>素材信息</button>
      <button class:active={inspectorTab === "sampling"} disabled={selectedSource?.kind !== "video"} onclick={() => (inspectorTab = "sampling")}>抽帧</button>
    </div>
    {#if inspectorTab === "sampling" && selectedSource?.kind === "video"}
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

        <div class="subsection-heading"><span>有效片段</span><strong>{videoSelections.length}</strong></div>
        <label class="toggle-row compact-toggle"><input type="checkbox" bind:checked={protectNewRange} /><span>新片段生成的候选默认锁定</span></label>
        <div class="range-list">
          {#each videoSelections as selection}
            <div><button class="range-jump" onclick={() => seekTo(selection.startMs)}><Flag size={13} fill={selection.protected ? "currentColor" : "none"} /><span><strong>{selection.label}</strong><small class="mono">{formatTimestamp(selection.startMs)} - {formatTimestamp(selection.endMs)}</small></span></button><button onclick={() => removeSelection(selection.id)} title="删除片段" aria-label="删除片段"><Trash2 size={13} /></button></div>
          {/each}
          {#if videoSelections.length === 0}<span class="range-empty">使用时间轴入点和出点添加片段</span>{/if}
        </div>

        <div class="subsection-heading"><span>画面变化</span><Activity size={14} /></div>
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
  </aside>

  <footer class="statusbar">
    <div class="status-left"><span class="status-dot" class:ok={!!project && !busyMessage && !videoBusy} class:error={messageKind === "error"}></span><span>{busyMessage || videoBusy || message || (project ? "项目已就绪" : "未打开项目")}</span></div>
    <div class="status-right"><span>源素材 {project?.sourceCount ?? 0}</span><span class:estimate-status={estimatePulse}>{estimatePulse && samplingEstimate ? `预计候选 ${samplingEstimate.estimatedCount}` : `候选 ${project?.candidateCount ?? 0}`}</span><span>任务 0</span></div>
  </footer>

  {#if dragActive}<div class="drop-overlay"><FolderInput size={38} /><strong>{project ? "松开以导入源素材" : "请先创建或打开项目"}</strong><span>支持文件和递归目录</span></div>{/if}

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
</main>
