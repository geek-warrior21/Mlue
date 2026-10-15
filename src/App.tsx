import { useCallback, useEffect, useRef, useState } from "react";
import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type ServerState = "starting" | "running" | "error" | "stopped";

export interface RuntimeStatus {
  state: ServerState;
  endpoint: string;
  error: string | null;
}

export interface HealthResponse {
  status: string;
  service: string;
  version: string;
}

export type ProviderStatus = "checking" | "ready" | "no_models" | "unreachable" | "error";

export interface ModelCapabilities {
  chat: boolean | null;
  streaming: boolean | null;
  tools: boolean | null;
  vision: boolean | null;
}

export interface ModelInfo {
  id: string;
  provider_id: string;
  upstream_id: string;
  display_name: string;
  capabilities: ModelCapabilities;
  size_bytes: number | null;
  parameter_size: string | null;
  quantization_level: string | null;
  modified_at: string | null;
}

export interface DiscoveredModelsResponse {
  provider_id: string;
  status: ProviderStatus;
  models: ModelInfo[];
  last_checked_at: string | null;
  last_successful_at: string | null;
  error: string | null;
}

export interface UsageInfo {
  prompt_tokens?: number | null;
  completion_tokens?: number | null;
  total_tokens?: number | null;
}

export interface InferenceResult {
  model_id: string;
  content: string;
  finish_reason?: string | null;
  usage?: UsageInfo | null;
  duration_ms?: number | null;
}

export interface InferenceErrorState {
  code: string;
  message: string;
  technical_details?: string | null;
}

export type InferenceEventType =
  | { type: "started"; request_id: string; model_id: string }
  | { type: "delta"; request_id: string; content: string }
  | {
      type: "completed";
      request_id: string;
      model_id: string;
      usage?: UsageInfo | null;
      duration_ms: number;
      finish_reason?: string | null;
    }
  | { type: "cancelled"; request_id: string }
  | { type: "error"; request_id: string; code: string; message: string };

export type GenerationStatus = "idle" | "generating" | "completed" | "cancelled" | "error";

export interface Project {
  id: string;
  name: string;
  description: string | null;
  created_at: string;
  updated_at: string;
}

export type AccessPolicy =
  | { type: "local_only" }
  | { type: "selected_models"; models: string[] };

export interface ApiKeyMetadata {
  id: string;
  project_id: string;
  name: string;
  key_prefix: string;
  access_policy: AccessPolicy;
  enabled: boolean;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

export interface CreatedApiKey {
  metadata: ApiKeyMetadata;
  full_key: string;
}

function formatBytes(bytes: number | null): string | null {
  if (bytes == null || bytes === 0) return null;
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

function formatDuration(durationMs?: number | null): string {
  if (durationMs == null) return "0.0s";
  return `${(durationMs / 1000).toFixed(1)}s`;
}

export default function App() {
  // Runtime State (Brick 0)
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus | null>(null);
  const [runtimeLoading, setRuntimeLoading] = useState<boolean>(true);
  const [lastRefreshed, setLastRefreshed] = useState<Date | null>(null);
  const [healthData, setHealthData] = useState<HealthResponse | null>(null);
  const [healthLoading, setHealthLoading] = useState<boolean>(false);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);

  // Model Discovery State (Brick 1)
  const [discoveryData, setDiscoveryData] = useState<DiscoveredModelsResponse | null>(null);
  const [discoveryLoading, setDiscoveryLoading] = useState<boolean>(true);
  const [isRefreshingOllama, setIsRefreshingOllama] = useState<boolean>(false);

  // Playground State (Brick 2A & 2B)
  const [selectedModelId, setSelectedModelId] = useState<string>("");
  const [promptText, setPromptText] = useState<string>("");
  const [isGenerating, setIsGenerating] = useState<boolean>(false);
  const [generationStatus, setGenerationStatus] = useState<GenerationStatus>("idle");
  const [activeModelId, setActiveModelId] = useState<string>("");
  const [streamedContent, setStreamedContent] = useState<string>("");
  const [generationResult, setGenerationResult] = useState<InferenceResult | null>(null);
  const [generationError, setGenerationError] = useState<InferenceErrorState | null>(null);
  const [isStreamingMode, setIsStreamingMode] = useState<boolean>(true);

  // Projects & API Keys State (Brick 3A)
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [apiKeys, setApiKeys] = useState<ApiKeyMetadata[]>([]);
  const [projectsLoading, setProjectsLoading] = useState<boolean>(true);
  const [keysLoading, setKeysLoading] = useState<boolean>(false);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [keyError, setKeyError] = useState<string | null>(null);

  // Project Modals
  const [showNewProjectModal, setShowNewProjectModal] = useState<boolean>(false);
  const [newProjectName, setNewProjectName] = useState<string>("");
  const [newProjectDesc, setNewProjectDesc] = useState<string>("");

  const [editingProject, setEditingProject] = useState<Project | null>(null);
  const [editProjectName, setEditProjectName] = useState<string>("");
  const [editProjectDesc, setEditProjectDesc] = useState<string>("");
  const [deletingProjectId, setDeletingProjectId] = useState<string | null>(null);

  // API Key Modals & One-Time Display
  const [showNewKeyModal, setShowNewKeyModal] = useState<boolean>(false);
  const [newKeyName, setNewKeyName] = useState<string>("");
  const [newKeyPolicyType, setNewKeyPolicyType] = useState<"local_only" | "selected_models">("local_only");
  const [selectedPolicyModels, setSelectedPolicyModels] = useState<string[]>([]);
  const [createdKeyData, setCreatedKeyData] = useState<CreatedApiKey | null>(null);
  const [revokingKeyId, setRevokingKeyId] = useState<string | null>(null);

  const selectedProject = projects.find((p) => p.id === selectedProjectId) || null;

  const activeRequestIdRef = useRef<string | null>(null);
  const contentBufferRef = useRef<string>("");
  const batchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Fetch Runtime Status via IPC
  const fetchStatus = useCallback(async () => {
    setRuntimeLoading(true);
    try {
      if (isTauri()) {
        const status = await invoke<RuntimeStatus>("get_runtime_status");
        setRuntimeStatus(status);
      } else {
        setRuntimeStatus({
          state: "starting",
          endpoint: "http://127.0.0.1:4782",
          error: "Running outside native Tauri desktop window (web preview mode)",
        });
      }
      setLastRefreshed(new Date());
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setRuntimeStatus({
        state: "error",
        endpoint: "http://127.0.0.1:4782",
        error: `IPC call failed: ${msg}`,
      });
    } finally {
      setRuntimeLoading(false);
    }
  }, []);

  // Fetch Existing Discovered Models Snapshot (non-blocking)
  const fetchDiscoveredModels = useCallback(async () => {
    try {
      if (isTauri()) {
        const data = await invoke<DiscoveredModelsResponse>("get_discovered_models");
        setDiscoveryData(data);
      } else {
        setDiscoveryData({
          provider_id: "ollama",
          status: "unreachable",
          models: [],
          last_checked_at: null,
          last_successful_at: null,
          error: "Tauri IPC is unavailable in browser preview mode.",
        });
      }
    } catch (err) {
      console.error("Failed to fetch discovered models:", err);
    } finally {
      setDiscoveryLoading(false);
    }
  }, []);

  // Trigger Ollama Discovery Refresh via IPC
  const refreshOllama = useCallback(async () => {
    setIsRefreshingOllama(true);
    try {
      if (isTauri()) {
        const updated = await invoke<DiscoveredModelsResponse>("refresh_ollama");
        setDiscoveryData(updated);
      } else {
        setDiscoveryData({
          provider_id: "ollama",
          status: "unreachable",
          models: [],
          last_checked_at: new Date().toISOString(),
          last_successful_at: null,
          error: "Tauri IPC is unavailable in browser preview mode.",
        });
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setDiscoveryData((prev) => ({
        provider_id: "ollama",
        status: "error",
        models: [],
        last_checked_at: new Date().toISOString(),
        last_successful_at: prev?.last_successful_at || null,
        error: `Refresh failed: ${msg}`,
      }));
    } finally {
      setIsRefreshingOllama(false);
    }
  }, []);

  // Test /health endpoint
  const testHealth = useCallback(async (endpoint: string) => {
    setHealthLoading(true);
    try {
      const res = await fetch(`${endpoint}/health`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status} ${res.statusText}`);
      }
      const data: HealthResponse = await res.json();
      setHealthData(data);
    } catch (err) {
      setHealthData(null);
      console.error("Health check error:", err);
    } finally {
      setHealthLoading(false);
    }
  }, []);

  // Initial load & automatic discovery update
  useEffect(() => {
    fetchStatus();
    fetchDiscoveredModels();

    let unlisten: (() => void) | undefined;
    if (isTauri()) {
      listen<DiscoveredModelsResponse>("models-updated", (event) => {
        setDiscoveryData(event.payload);
        setDiscoveryLoading(false);
      }).then((unsub) => {
        unlisten = unsub;
      });
    }

    return () => {
      if (unlisten) unlisten();
    };
  }, [fetchStatus, fetchDiscoveredModels]);

  // Sync selected model with available models
  useEffect(() => {
    if (discoveryData && discoveryData.models.length > 0) {
      if (!selectedModelId || !discoveryData.models.some((m) => m.id === selectedModelId)) {
        setSelectedModelId(discoveryData.models[0].id);
      }
    } else if (discoveryData && discoveryData.models.length === 0) {
      setSelectedModelId("");
    }
  }, [discoveryData, selectedModelId]);

  // Terminal state auto-transition: If still checking, poll every 500ms until reaching terminal state
  useEffect(() => {
    if (discoveryData?.status !== "checking") return;

    const interval = setInterval(() => {
      fetchDiscoveredModels();
    }, 500);

    return () => clearInterval(interval);
  }, [discoveryData?.status, fetchDiscoveredModels]);

  // Fetch Projects via IPC
  const fetchProjects = useCallback(async () => {
    setProjectsLoading(true);
    setProjectError(null);
    try {
      if (isTauri()) {
        const list = await invoke<Project[]>("list_projects");
        setProjects(list);
        setSelectedProjectId((prev) => {
          if (prev && list.some((p) => p.id === prev)) return prev;
          return list.length > 0 ? list[0].id : null;
        });
      } else {
        const mock: Project[] = [
          {
            id: "proj_default_1",
            name: "My Study Bot",
            description: "Study assistant integration",
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          },
        ];
        setProjects(mock);
        setSelectedProjectId("proj_default_1");
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setProjectError(`Failed to load projects: ${msg}`);
    } finally {
      setProjectsLoading(false);
    }
  }, []);

  // Fetch API Keys for selected project
  const fetchKeys = useCallback(async (projectId: string) => {
    setKeysLoading(true);
    setKeyError(null);
    try {
      if (isTauri()) {
        const list = await invoke<ApiKeyMetadata[]>("list_api_keys", {
          project_id: projectId,
        });
        setApiKeys(list);
      } else {
        setApiKeys([]);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setKeyError(`Failed to load API keys: ${msg}`);
    } finally {
      setKeysLoading(false);
    }
  }, []);

  // Trigger project loading on mount
  useEffect(() => {
    fetchProjects();
  }, [fetchProjects]);

  // Sync keys whenever selected project changes
  useEffect(() => {
    if (selectedProjectId) {
      fetchKeys(selectedProjectId);
    } else {
      setApiKeys([]);
    }
  }, [selectedProjectId, fetchKeys]);

  const handleCreateProject = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newProjectName.trim()) return;
    try {
      if (isTauri()) {
        const created = await invoke<Project>("create_project", {
          name: newProjectName.trim(),
          description: newProjectDesc.trim() || null,
        });
        setProjects((prev) => [created, ...prev]);
        setSelectedProjectId(created.id);
      } else {
        const mock: Project = {
          id: `proj_${Date.now()}`,
          name: newProjectName.trim(),
          description: newProjectDesc.trim() || null,
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        };
        setProjects((prev) => [mock, ...prev]);
        setSelectedProjectId(mock.id);
      }
      setNewProjectName("");
      setNewProjectDesc("");
      setShowNewProjectModal(false);
      setProjectError(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setProjectError(msg);
    }
  };

  const handleUpdateProject = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!editingProject || !editProjectName.trim()) return;
    try {
      if (isTauri()) {
        const updated = await invoke<Project>("update_project", {
          id: editingProject.id,
          name: editProjectName.trim(),
          description: editProjectDesc.trim() || null,
        });
        setProjects((prev) =>
          prev.map((p) => (p.id === updated.id ? updated : p))
        );
      } else {
        setProjects((prev) =>
          prev.map((p) =>
            p.id === editingProject.id
              ? {
                  ...p,
                  name: editProjectName.trim(),
                  description: editProjectDesc.trim() || null,
                  updated_at: new Date().toISOString(),
                }
              : p
          )
        );
      }
      setEditingProject(null);
      setProjectError(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setProjectError(msg);
    }
  };

  const handleDeleteProject = async (id: string) => {
    try {
      if (isTauri()) {
        await invoke<boolean>("delete_project", { id });
      }
      setProjects((prev) => prev.filter((p) => p.id !== id));
      if (selectedProjectId === id) {
        const remaining = projects.filter((p) => p.id !== id);
        setSelectedProjectId(remaining.length > 0 ? remaining[0].id : null);
      }
      setDeletingProjectId(null);
      setProjectError(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setProjectError(msg);
    }
  };

  const handleCreateKey = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!selectedProjectId || !newKeyName.trim()) return;
    try {
      const policy: AccessPolicy =
        newKeyPolicyType === "local_only"
          ? { type: "local_only" }
          : { type: "selected_models", models: selectedPolicyModels };

      if (isTauri()) {
        const result = await invoke<CreatedApiKey>("create_api_key", {
          project_id: selectedProjectId,
          name: newKeyName.trim(),
          access_policy: policy,
        });
        setApiKeys((prev) => [result.metadata, ...prev]);
        setCreatedKeyData(result);
      } else {
        const mockCreated: CreatedApiKey = {
          metadata: {
            id: `key_${Date.now()}`,
            project_id: selectedProjectId,
            name: newKeyName.trim(),
            key_prefix: "mlue_sk_mock1234...",
            access_policy: policy,
            enabled: true,
            created_at: new Date().toISOString(),
            last_used_at: null,
            revoked_at: null,
          },
          full_key: "mlue_sk_mock_preview_key_not_for_real_use_1234567890",
        };
        setApiKeys((prev) => [mockCreated.metadata, ...prev]);
        setCreatedKeyData(mockCreated);
      }
      setNewKeyName("");
      setNewKeyPolicyType("local_only");
      setSelectedPolicyModels([]);
      setShowNewKeyModal(false);
      setKeyError(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setKeyError(msg);
    }
  };

  const handleRevokeKey = async (keyId: string) => {
    try {
      if (isTauri()) {
        await invoke<boolean>("revoke_api_key", { key_id: keyId });
      }
      setApiKeys((prev) =>
        prev.map((k) =>
          k.id === keyId
            ? {
                ...k,
                enabled: false,
                revoked_at: new Date().toISOString(),
              }
            : k
        )
      );
      setRevokingKeyId(null);
      setKeyError(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setKeyError(msg);
    }
  };

  const copyToClipboard = (text: string, key: string) => {
    navigator.clipboard.writeText(text);
    setCopiedKey(key);
    setTimeout(() => setCopiedKey(null), 2000);
  };

  // Cancel active inference operation
  const handleCancel = useCallback(async () => {
    const reqId = activeRequestIdRef.current;
    if (!reqId) return;

    try {
      if (isTauri()) {
        await invoke("cancel_test_completion", { request_id: reqId });
      }
    } catch (err) {
      console.error("Cancel failed:", err);
    }
  }, []);

  // Cleanup active request handling on unmount
  useEffect(() => {
    return () => {
      if (activeRequestIdRef.current && isTauri()) {
        invoke("cancel_test_completion", { request_id: activeRequestIdRef.current }).catch(() => {});
      }
      if (batchTimerRef.current) {
        clearTimeout(batchTimerRef.current);
      }
    };
  }, []);

  // Execute inference in AI Playground
  const handleGenerate = async (e?: React.FormEvent) => {
    if (e) e.preventDefault();
    if (isGenerating || !selectedModelId || !promptText.trim()) return;

    const targetModelId = selectedModelId;
    const userPrompt = promptText.trim();
    const requestId = `req_${Date.now()}_${Math.random().toString(36).substring(2, 9)}`;

    setIsGenerating(true);
    setGenerationStatus("generating");
    setActiveModelId(targetModelId);
    setStreamedContent("");
    setGenerationResult(null);
    setGenerationError(null);
    activeRequestIdRef.current = requestId;
    contentBufferRef.current = "";

    if (!isTauri()) {
      setGenerationStatus("error");
      setGenerationError({
        code: "TAURI_UNAVAILABLE",
        message: "Desktop inference requires running inside the Tauri native app.",
        technical_details: "Web preview mode cannot invoke native IPC.",
      });
      setIsGenerating(false);
      activeRequestIdRef.current = null;
      return;
    }

    if (isStreamingMode) {
      try {
        const channel = new Channel<InferenceEventType>((event) => {
          // Ignore events that do not match the active request ID
          if (event.request_id !== activeRequestIdRef.current) return;

          switch (event.type) {
            case "started":
              break;

            case "delta": {
              contentBufferRef.current += event.content;
              if (!batchTimerRef.current) {
                batchTimerRef.current = setTimeout(() => {
                  setStreamedContent(contentBufferRef.current);
                  batchTimerRef.current = null;
                }, 20);
              }
              break;
            }

            case "completed": {
              if (batchTimerRef.current) {
                clearTimeout(batchTimerRef.current);
                batchTimerRef.current = null;
              }
              const finalContent = contentBufferRef.current;
              setStreamedContent(finalContent);
              setGenerationResult({
                model_id: event.model_id,
                content: finalContent,
                finish_reason: event.finish_reason,
                usage: event.usage,
                duration_ms: event.duration_ms,
              });
              setGenerationStatus("completed");
              setIsGenerating(false);
              activeRequestIdRef.current = null;
              break;
            }

            case "cancelled": {
              if (batchTimerRef.current) {
                clearTimeout(batchTimerRef.current);
                batchTimerRef.current = null;
              }
              setStreamedContent(contentBufferRef.current);
              setGenerationStatus("cancelled");
              setIsGenerating(false);
              activeRequestIdRef.current = null;
              break;
            }

            case "error": {
              if (batchTimerRef.current) {
                clearTimeout(batchTimerRef.current);
                batchTimerRef.current = null;
              }
              setStreamedContent(contentBufferRef.current);
              setGenerationError({
                code: event.code,
                message: event.message,
              });
              setGenerationStatus("error");
              setIsGenerating(false);
              activeRequestIdRef.current = null;
              break;
            }
          }
        });

        // Register channel callback before invoking command
        await invoke("start_test_stream", {
          request_id: requestId,
          model_id: targetModelId,
          messages: [
            {
              role: "user",
              content: userPrompt,
            },
          ],
          on_event: channel,
        });
      } catch (err: unknown) {
        console.error("Stream start failed:", err);
        const payload = err as { code?: string; message?: string; technical_details?: string } | undefined;
        setGenerationError({
          code: payload?.code || "STREAM_START_FAILED",
          message: payload?.message || "Failed to start inference stream.",
          technical_details: payload?.technical_details || String(err),
        });
        setGenerationStatus("error");
        setIsGenerating(false);
        activeRequestIdRef.current = null;
      }
    } else {
      // Non-streaming fallback
      try {
        const response = await invoke<InferenceResult>("generate_test_completion", {
          request: {
            model_id: targetModelId,
            messages: [{ role: "user", content: userPrompt }],
          },
        });
        setStreamedContent(response.content);
        setGenerationResult(response);
        setGenerationStatus("completed");
      } catch (err: unknown) {
        console.error("Inference failed:", err);
        const payload = err as { code?: string; message?: string; technical_details?: string } | undefined;
        setGenerationError({
          code: payload?.code || "INFERENCE_ERROR",
          message: payload?.message || "Failed to generate completion.",
          technical_details: payload?.technical_details || null,
        });
        setGenerationStatus("error");
      } finally {
        setIsGenerating(false);
        activeRequestIdRef.current = null;
      }
    }
  };

  const endpoint = runtimeStatus?.endpoint || "http://127.0.0.1:4782";
  const healthUrl = `${endpoint}/health`;
  const currentState = runtimeStatus?.state || "starting";

  const getStatusBadge = () => {
    switch (currentState) {
      case "running":
        return {
          label: "Running",
          bg: "bg-emerald-500/10 text-emerald-400 border-emerald-500/20",
          dot: "bg-emerald-400",
        };
      case "starting":
        return {
          label: "Starting",
          bg: "bg-amber-500/10 text-amber-400 border-amber-500/20",
          dot: "bg-amber-400 animate-pulse",
        };
      case "error":
        return {
          label: "Error",
          bg: "bg-rose-500/10 text-rose-400 border-rose-500/20",
          dot: "bg-rose-400",
        };
      case "stopped":
        return {
          label: "Stopped",
          bg: "bg-zinc-500/10 text-zinc-400 border-zinc-500/20",
          dot: "bg-zinc-400",
        };
    }
  };

  const badge = getStatusBadge();
  const providerStatus = discoveryData?.status || "checking";
  const hasAvailableModels = Boolean(discoveryData?.models && discoveryData.models.length > 0);

  return (
    <div className="min-h-screen bg-zinc-950 text-zinc-100 font-sans flex flex-col items-center justify-center p-6 selection:bg-indigo-500/30 selection:text-indigo-200">
      <div className="w-full max-w-2xl bg-zinc-900/80 border border-zinc-800 rounded-2xl p-8 shadow-2xl backdrop-blur-xl transition-all space-y-6">
        {/* Header */}
        <header className="flex items-center justify-between border-b border-zinc-800/80 pb-6">
          <div>
            <div className="flex items-center gap-3">
              <span className="flex items-center justify-center w-8 h-8 rounded-lg bg-indigo-600/20 border border-indigo-500/30 text-indigo-400 font-bold text-lg">
                m
              </span>
              <h1 className="text-2xl font-bold tracking-tight text-white">mlue</h1>
            </div>
            <p className="text-sm text-zinc-400 mt-1 font-medium">Your AI. Your API.</p>
          </div>

          <div
            id="runtime-status-badge"
            className={`inline-flex items-center gap-2 px-3 py-1.5 rounded-full text-xs font-semibold border ${badge.bg}`}
          >
            <span className={`w-2 h-2 rounded-full ${badge.dot}`} />
            <span>{badge.label}</span>
          </div>
        </header>

        {/* Runtime Error Alert */}
        {runtimeStatus?.error && (
          <div
            id="runtime-error-alert"
            className="p-4 rounded-xl bg-rose-950/40 border border-rose-800/50 text-rose-200 text-sm flex flex-col gap-1"
          >
            <div className="font-semibold flex items-center gap-2 text-rose-300">
              <svg className="w-4 h-4 text-rose-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" />
              </svg>
              Backend Startup Failure
            </div>
            <p className="text-xs text-rose-300/80 break-words font-mono mt-0.5">{runtimeStatus.error}</p>
          </div>
        )}

        {/* SECTION 1: Embedded Runtime & Local API */}
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <h2 className="text-xs font-semibold uppercase tracking-wider text-zinc-400">
              Embedded Runtime
            </h2>
            <button
              id="refresh-runtime-btn"
              onClick={fetchStatus}
              disabled={runtimeLoading}
              className="text-[11px] text-zinc-400 hover:text-indigo-400 transition-colors inline-flex items-center gap-1 cursor-pointer disabled:opacity-50"
            >
              <span>{runtimeLoading ? "Checking..." : "Refresh Status"}</span>
            </button>
          </div>

          <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
            {/* Local API Endpoint */}
            <div className="p-3.5 rounded-xl bg-zinc-950/60 border border-zinc-800/80">
              <div className="flex items-center justify-between mb-1">
                <span className="text-xs text-zinc-400 font-medium">Local API</span>
                <button
                  id="copy-endpoint-btn"
                  onClick={() => copyToClipboard(endpoint, "endpoint")}
                  className="text-xs text-zinc-400 hover:text-indigo-400 transition-colors cursor-pointer"
                >
                  {copiedKey === "endpoint" ? <span className="text-emerald-400">Copied</span> : "Copy"}
                </button>
              </div>
              <div id="local-api-endpoint" className="font-mono text-xs text-indigo-300 select-all break-all">
                {endpoint}
              </div>
            </div>

            {/* Health Endpoint */}
            <div className="p-3.5 rounded-xl bg-zinc-950/60 border border-zinc-800/80">
              <div className="flex items-center justify-between mb-1">
                <span className="text-xs text-zinc-400 font-medium">Health</span>
                <div className="flex items-center gap-2">
                  <button
                    id="test-health-btn"
                    onClick={() => testHealth(endpoint)}
                    disabled={healthLoading}
                    className="text-xs text-zinc-400 hover:text-indigo-400 transition-colors cursor-pointer disabled:opacity-50"
                  >
                    {healthLoading ? "Pinging..." : "Test"}
                  </button>
                  <span className="text-zinc-600">·</span>
                  <button
                    id="copy-health-btn"
                    onClick={() => copyToClipboard(healthUrl, "health")}
                    className="text-xs text-zinc-400 hover:text-indigo-400 transition-colors cursor-pointer"
                  >
                    {copiedKey === "health" ? <span className="text-emerald-400">Copied</span> : "Copy"}
                  </button>
                </div>
              </div>
              <div id="health-endpoint" className="font-mono text-xs text-indigo-300 select-all break-all">
                {healthUrl}
              </div>

              {healthData && (
                <div
                  id="health-response-preview"
                  className="mt-2 pt-2 border-t border-zinc-800/60 text-[11px] font-mono text-emerald-400/90 flex items-center justify-between"
                >
                  <span className="truncate">{JSON.stringify(healthData)}</span>
                  <span className="text-zinc-500 text-[10px] ml-1">200 OK</span>
                </div>
              )}
            </div>
          </div>
        </div>

        {/* SECTION 2: Projects & API Keys (Brick 3A) */}
        <section aria-labelledby="projects-heading" className="border-t border-zinc-800/80 pt-6 space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <h2 id="projects-heading" className="text-xs font-semibold uppercase tracking-wider text-zinc-400">
                Projects & API Keys
              </h2>
              <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-indigo-500/10 text-indigo-400 border border-indigo-500/20">
                {projects.length} {projects.length === 1 ? "Project" : "Projects"}
              </span>
            </div>
            <button
              id="new-project-btn"
              onClick={() => {
                setNewProjectName("");
                setNewProjectDesc("");
                setShowNewProjectModal(true);
              }}
              className="text-xs text-indigo-400 hover:text-indigo-300 font-medium inline-flex items-center gap-1 cursor-pointer transition-colors"
            >
              + New Project
            </button>
          </div>

          {/* Project Error Notification */}
          {projectError && (
            <div className="p-3 rounded-xl bg-rose-950/40 border border-rose-800/50 text-rose-200 text-xs flex items-center justify-between">
              <span>{projectError}</span>
              <button onClick={() => setProjectError(null)} className="text-rose-400 hover:text-white text-xs cursor-pointer">Dismiss</button>
            </div>
          )}

          {/* Key Error Notification */}
          {keyError && (
            <div className="p-3 rounded-xl bg-rose-950/40 border border-rose-800/50 text-rose-200 text-xs flex items-center justify-between">
              <span>{keyError}</span>
              <button onClick={() => setKeyError(null)} className="text-rose-400 hover:text-white text-xs cursor-pointer">Dismiss</button>
            </div>
          )}

          {/* One-Time Plaintext Secret Box (Security requirement) */}
          {createdKeyData && (
            <div id="new-key-secret-box" className="p-4 rounded-xl bg-emerald-950/40 border border-emerald-500/40 space-y-3">
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold text-emerald-300 flex items-center gap-1.5">
                  <span className="w-2 h-2 rounded-full bg-emerald-400 animate-pulse" />
                  API Key Created: {createdKeyData.metadata.name}
                </span>
                <button
                  id="dismiss-new-key-btn"
                  onClick={() => setCreatedKeyData(null)}
                  className="text-xs text-zinc-400 hover:text-white cursor-pointer"
                >
                  Dismiss
                </button>
              </div>

              <div className="p-2.5 rounded-lg bg-amber-950/50 border border-amber-800/50 text-amber-200 text-xs font-medium flex items-center gap-2">
                <span>⚠️ Copy this key now. For security, mlue won't show it again.</span>
              </div>

              {/* Local API Endpoint */}
              <div className="space-y-1">
                <label className="text-[11px] font-semibold text-zinc-400 uppercase tracking-wider">Local API Endpoint</label>
                <div className="flex items-center gap-2">
                  <input
                    id="api-endpoint-input"
                    type="text"
                    readOnly
                    value="http://127.0.0.1:4782/v1"
                    className="w-full font-mono text-xs bg-zinc-950 border border-zinc-700/80 rounded-lg px-3 py-2 text-zinc-300 select-all"
                  />
                  <button
                    id="copy-endpoint-btn"
                    onClick={() => copyToClipboard("http://127.0.0.1:4782/v1", "api-endpoint")}
                    className="px-3 py-2 bg-zinc-800 hover:bg-zinc-700 text-zinc-200 rounded-lg text-xs font-medium cursor-pointer transition-colors whitespace-nowrap"
                  >
                    {copiedKey === "api-endpoint" ? "Copied!" : "Copy URL"}
                  </button>
                </div>
              </div>

              {/* One-Time Plaintext Key */}
              <div className="space-y-1">
                <label className="text-[11px] font-semibold text-zinc-400 uppercase tracking-wider">Personal API Key</label>
                <div className="flex items-center gap-2">
                  <input
                    id="new-key-plaintext-input"
                    type="text"
                    readOnly
                    value={createdKeyData.full_key}
                    className="w-full font-mono text-xs bg-zinc-950 border border-zinc-700/80 rounded-lg px-3 py-2 text-emerald-300 select-all"
                  />
                  <button
                    id="copy-new-key-btn"
                    onClick={() => copyToClipboard(createdKeyData.full_key, "new-key-secret")}
                    className="px-3 py-2 bg-emerald-600 hover:bg-emerald-500 text-white rounded-lg text-xs font-medium cursor-pointer transition-colors whitespace-nowrap"
                  >
                    {copiedKey === "new-key-secret" ? "Copied!" : "Copy Key"}
                  </button>
                </div>
              </div>

              {/* Quick Integration / Python Example Button */}
              <div className="pt-2 flex flex-col sm:flex-row sm:items-center justify-between gap-2 border-t border-emerald-900/40">
                <span className="text-[11px] text-zinc-400">
                  mlue must be running locally for requests to succeed.
                </span>
                <button
                  id="copy-python-example-btn"
                  onClick={() => {
                    const pythonSnippet = `from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:4782/v1",
    api_key="${createdKeyData.full_key}"
)

response = client.chat.completions.create(
    model="auto",
    messages=[
        {"role": "user", "content": "Hello from mlue."}
    ]
)

print(response.choices[0].message.content)`;
                    copyToClipboard(pythonSnippet, "python-example");
                  }}
                  className="px-3 py-1.5 bg-indigo-600/80 hover:bg-indigo-600 text-white rounded-lg text-xs font-medium cursor-pointer transition-colors whitespace-nowrap flex items-center gap-1.5"
                >
                  <span>🐍</span>
                  <span>{copiedKey === "python-example" ? "Copied Example!" : "Copy Python Example"}</span>
                </button>
              </div>

              <div className="p-2 rounded-lg bg-zinc-900/80 border border-zinc-800 text-[11px] text-zinc-400">
                <span className="text-zinc-300 font-medium">Notice:</span> HTTP streaming (SSE) and remote network access are not yet available. Send requests with <code className="text-zinc-200 bg-zinc-800 px-1 py-0.5 rounded font-mono">stream: false</code>.
              </div>
            </div>
          )}

          {/* Project List / Selector */}
          {projectsLoading ? (
            <p className="text-xs text-zinc-500 italic">Loading projects...</p>
          ) : projects.length === 0 ? (
            <div className="p-5 rounded-xl bg-zinc-950/60 border border-zinc-800/80 text-center space-y-2">
              <p className="text-xs text-zinc-400">No projects yet. Create a project to organize your applications and generate API keys.</p>
              <button
                onClick={() => setShowNewProjectModal(true)}
                className="px-3.5 py-1.5 bg-indigo-600 hover:bg-indigo-500 text-white text-xs font-medium rounded-lg transition-colors cursor-pointer"
              >
                Create First Project
              </button>
            </div>
          ) : (
            <div className="space-y-3">
              {/* Project Tabs */}
              <div id="project-tabs" className="flex items-center gap-2 overflow-x-auto pb-1">
                {projects.map((proj) => (
                  <button
                    key={proj.id}
                    onClick={() => setSelectedProjectId(proj.id)}
                    className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-all whitespace-nowrap cursor-pointer flex items-center gap-1.5 ${
                      selectedProjectId === proj.id
                        ? "bg-zinc-800 text-white border border-zinc-700 shadow-sm"
                        : "bg-zinc-950/60 text-zinc-400 hover:text-zinc-200 border border-zinc-800/60"
                    }`}
                  >
                    <span>{proj.name}</span>
                  </button>
                ))}
              </div>

              {/* Selected Project Card */}
              {selectedProject && (
                <div id="selected-project-card" className="p-4 rounded-xl bg-zinc-950/60 border border-zinc-800/80 space-y-4">
                  <div className="flex items-start justify-between">
                    <div>
                      <h3 className="text-sm font-semibold text-zinc-100 flex items-center gap-2">
                        {selectedProject.name}
                      </h3>
                      {selectedProject.description ? (
                        <p className="text-xs text-zinc-400 mt-0.5">{selectedProject.description}</p>
                      ) : (
                        <p className="text-[11px] text-zinc-500 italic mt-0.5">No description</p>
                      )}
                      <p className="text-[10px] text-zinc-500 font-mono mt-1">
                        ID: {selectedProject.id} · Created {new Date(selectedProject.created_at).toLocaleDateString()}
                      </p>
                    </div>

                    <div className="flex items-center gap-2">
                      <button
                        id="edit-project-btn"
                        onClick={() => {
                          setEditingProject(selectedProject);
                          setEditProjectName(selectedProject.name);
                          setEditProjectDesc(selectedProject.description || "");
                        }}
                        className="text-xs text-zinc-400 hover:text-zinc-200 px-2.5 py-1 rounded-lg border border-zinc-800 hover:border-zinc-700 cursor-pointer transition-colors"
                      >
                        Rename
                      </button>
                      <button
                        id="delete-project-btn"
                        onClick={() => setDeletingProjectId(selectedProject.id)}
                        className="text-xs text-rose-400 hover:text-rose-300 px-2.5 py-1 rounded-lg border border-rose-950 hover:border-rose-900 cursor-pointer transition-colors"
                      >
                        Delete
                      </button>
                    </div>
                  </div>

                  {/* API Keys List */}
                  <div className="pt-3 border-t border-zinc-800/80 space-y-3">
                    <div className="flex items-center justify-between">
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-semibold text-zinc-300">API Keys</span>
                        <span className="text-[10px] text-zinc-500 font-mono">({apiKeys.length})</span>
                      </div>
                      <button
                        id="generate-key-btn"
                        onClick={() => {
                          setNewKeyName("");
                          setNewKeyPolicyType("local_only");
                          setSelectedPolicyModels([]);
                          setShowNewKeyModal(true);
                        }}
                        className="px-2.5 py-1 bg-indigo-600/20 hover:bg-indigo-600/30 text-indigo-300 border border-indigo-500/30 rounded-lg text-xs font-medium cursor-pointer transition-colors"
                      >
                        + Generate Key
                      </button>
                    </div>

                    {keysLoading ? (
                      <p className="text-xs text-zinc-500 italic">Loading API keys...</p>
                    ) : apiKeys.length === 0 ? (
                      <p className="text-xs text-zinc-500 italic">No API keys generated yet for this project.</p>
                    ) : (
                      <div id="api-keys-list" className="space-y-2">
                        {apiKeys.map((key) => {
                          const isActive = key.enabled && !key.revoked_at;
                          return (
                            <div
                              key={key.id}
                              className="p-3 rounded-lg bg-zinc-900/60 border border-zinc-800 flex items-center justify-between text-xs"
                            >
                              <div className="space-y-0.5">
                                <div className="flex items-center gap-2">
                                  <span className="font-medium text-zinc-200">{key.name}</span>
                                  <span
                                    className={`px-1.5 py-0.2 rounded text-[10px] font-semibold border ${
                                      isActive
                                        ? "bg-emerald-500/10 text-emerald-400 border-emerald-500/20"
                                        : "bg-rose-500/10 text-rose-400 border-rose-500/20"
                                    }`}
                                  >
                                    {isActive ? "Active" : "Revoked"}
                                  </span>
                                  <span className="text-[10px] font-mono text-zinc-500 bg-zinc-950 px-1.5 py-0.5 rounded">
                                    {key.access_policy.type === "local_only" ? "local_only" : "selected_models"}
                                  </span>
                                </div>
                                <div className="font-mono text-[11px] text-zinc-400">
                                  Prefix: <span className="text-indigo-300">{key.key_prefix}</span>
                                </div>
                                <div className="text-[10px] text-zinc-500">
                                  Created: {new Date(key.created_at).toLocaleDateString()}
                                  {key.last_used_at && ` · Last used: ${new Date(key.last_used_at).toLocaleTimeString()}`}
                                </div>
                              </div>

                              <div>
                                {isActive && (
                                  <button
                                    id={`revoke-key-${key.id}`}
                                    onClick={() => setRevokingKeyId(key.id)}
                                    className="text-xs text-rose-400/80 hover:text-rose-300 px-2.5 py-1 rounded-lg border border-rose-950 hover:border-rose-900 cursor-pointer transition-colors"
                                  >
                                    Revoke
                                  </button>
                                )}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          )}

          {/* Modal: Create Project */}
          {showNewProjectModal && (
            <div className="fixed inset-0 bg-black/70 backdrop-blur-sm z-50 flex items-center justify-center p-4">
              <div className="bg-zinc-900 border border-zinc-800 rounded-2xl p-6 max-w-md w-full space-y-4 shadow-2xl">
                <h3 className="text-sm font-bold text-white">Create New Project</h3>
                <form onSubmit={handleCreateProject} className="space-y-3">
                  <div>
                    <label className="block text-xs font-medium text-zinc-300 mb-1">Project Name *</label>
                    <input
                      id="new-project-name-input"
                      type="text"
                      required
                      maxLength={100}
                      placeholder="e.g. My Study Bot"
                      value={newProjectName}
                      onChange={(e) => setNewProjectName(e.target.value)}
                      className="w-full bg-zinc-950 border border-zinc-800 rounded-lg px-3 py-2 text-xs text-zinc-100 focus:outline-none focus:border-indigo-500"
                    />
                  </div>
                  <div>
                    <label className="block text-xs font-medium text-zinc-300 mb-1">Description (Optional)</label>
                    <textarea
                      id="new-project-desc-input"
                      maxLength={500}
                      placeholder="Integration details or notes..."
                      value={newProjectDesc}
                      onChange={(e) => setNewProjectDesc(e.target.value)}
                      className="w-full bg-zinc-950 border border-zinc-800 rounded-lg px-3 py-2 text-xs text-zinc-100 focus:outline-none focus:border-indigo-500 resize-none h-20"
                    />
                  </div>
                  <div className="flex items-center justify-end gap-2 pt-2">
                    <button
                      type="button"
                      onClick={() => setShowNewProjectModal(false)}
                      className="px-3 py-1.5 rounded-lg text-xs text-zinc-400 hover:text-white cursor-pointer"
                    >
                      Cancel
                    </button>
                    <button
                      id="create-project-submit-btn"
                      type="submit"
                      disabled={!newProjectName.trim()}
                      className="px-4 py-1.5 bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white rounded-lg text-xs font-semibold cursor-pointer"
                    >
                      Create Project
                    </button>
                  </div>
                </form>
              </div>
            </div>
          )}

          {/* Modal: Edit Project */}
          {editingProject && (
            <div className="fixed inset-0 bg-black/70 backdrop-blur-sm z-50 flex items-center justify-center p-4">
              <div className="bg-zinc-900 border border-zinc-800 rounded-2xl p-6 max-w-md w-full space-y-4 shadow-2xl">
                <h3 className="text-sm font-bold text-white">Rename Project</h3>
                <form onSubmit={handleUpdateProject} className="space-y-3">
                  <div>
                    <label className="block text-xs font-medium text-zinc-300 mb-1">Project Name *</label>
                    <input
                      id="edit-project-name-input"
                      type="text"
                      required
                      maxLength={100}
                      value={editProjectName}
                      onChange={(e) => setEditProjectName(e.target.value)}
                      className="w-full bg-zinc-950 border border-zinc-800 rounded-lg px-3 py-2 text-xs text-zinc-100 focus:outline-none focus:border-indigo-500"
                    />
                  </div>
                  <div>
                    <label className="block text-xs font-medium text-zinc-300 mb-1">Description (Optional)</label>
                    <textarea
                      id="edit-project-desc-input"
                      maxLength={500}
                      value={editProjectDesc}
                      onChange={(e) => setEditProjectDesc(e.target.value)}
                      className="w-full bg-zinc-950 border border-zinc-800 rounded-lg px-3 py-2 text-xs text-zinc-100 focus:outline-none focus:border-indigo-500 resize-none h-20"
                    />
                  </div>
                  <div className="flex items-center justify-end gap-2 pt-2">
                    <button
                      type="button"
                      onClick={() => setEditingProject(null)}
                      className="px-3 py-1.5 rounded-lg text-xs text-zinc-400 hover:text-white cursor-pointer"
                    >
                      Cancel
                    </button>
                    <button
                      id="save-project-btn"
                      type="submit"
                      disabled={!editProjectName.trim()}
                      className="px-4 py-1.5 bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white rounded-lg text-xs font-semibold cursor-pointer"
                    >
                      Save Changes
                    </button>
                  </div>
                </form>
              </div>
            </div>
          )}

          {/* Modal: Delete Project Confirmation */}
          {deletingProjectId && (
            <div className="fixed inset-0 bg-black/70 backdrop-blur-sm z-50 flex items-center justify-center p-4">
              <div className="bg-zinc-900 border border-zinc-800 rounded-2xl p-6 max-w-md w-full space-y-4 shadow-2xl">
                <h3 className="text-sm font-bold text-rose-300">Delete Project?</h3>
                <p className="text-xs text-zinc-300 leading-relaxed">
                  Are you sure you want to delete this project? All associated API keys will also be permanently revoked and deleted. This action cannot be undone.
                </p>
                <div className="flex items-center justify-end gap-2 pt-2">
                  <button
                    type="button"
                    onClick={() => setDeletingProjectId(null)}
                    className="px-3 py-1.5 rounded-lg text-xs text-zinc-400 hover:text-white cursor-pointer"
                  >
                    Cancel
                  </button>
                  <button
                    id="confirm-delete-project-btn"
                    onClick={() => handleDeleteProject(deletingProjectId)}
                    className="px-4 py-1.5 bg-rose-600 hover:bg-rose-500 text-white rounded-lg text-xs font-semibold cursor-pointer"
                  >
                    Confirm Delete
                  </button>
                </div>
              </div>
            </div>
          )}

          {/* Modal: Create API Key */}
          {showNewKeyModal && (
            <div className="fixed inset-0 bg-black/70 backdrop-blur-sm z-50 flex items-center justify-center p-4">
              <div className="bg-zinc-900 border border-zinc-800 rounded-2xl p-6 max-w-md w-full space-y-4 shadow-2xl">
                <h3 className="text-sm font-bold text-white">Generate Secure API Key</h3>
                <form onSubmit={handleCreateKey} className="space-y-3">
                  <div>
                    <label className="block text-xs font-medium text-zinc-300 mb-1">Key Name *</label>
                    <input
                      id="new-key-name-input"
                      type="text"
                      required
                      maxLength={100}
                      placeholder="e.g. Next.js Web App"
                      value={newKeyName}
                      onChange={(e) => setNewKeyName(e.target.value)}
                      className="w-full bg-zinc-950 border border-zinc-800 rounded-lg px-3 py-2 text-xs text-zinc-100 focus:outline-none focus:border-indigo-500"
                    />
                  </div>

                  <div>
                    <label className="block text-xs font-medium text-zinc-300 mb-1">Access Policy</label>
                    <select
                      id="new-key-policy-select"
                      value={newKeyPolicyType}
                      onChange={(e) => setNewKeyPolicyType(e.target.value as "local_only" | "selected_models")}
                      className="w-full bg-zinc-950 border border-zinc-800 rounded-lg px-3 py-2 text-xs text-zinc-100 focus:outline-none focus:border-indigo-500"
                    >
                      <option value="local_only">Local Only (Default) — Access all detected local models</option>
                      <option value="selected_models">Selected Models — Restrict to specific models</option>
                    </select>
                  </div>

                  {newKeyPolicyType === "selected_models" && (
                    <div className="space-y-1.5 max-h-32 overflow-y-auto p-2 rounded-lg bg-zinc-950 border border-zinc-800">
                      <p className="text-[11px] text-zinc-400 mb-1">Select authorized models:</p>
                      {discoveryData && discoveryData.models.length > 0 ? (
                        discoveryData.models.map((m) => (
                          <label key={m.id} className="flex items-center gap-2 text-xs text-zinc-300 cursor-pointer">
                            <input
                              type="checkbox"
                              checked={selectedPolicyModels.includes(m.id)}
                              onChange={(e) => {
                                if (e.target.checked) {
                                  setSelectedPolicyModels((prev) => [...prev, m.id]);
                                } else {
                                  setSelectedPolicyModels((prev) => prev.filter((id) => id !== m.id));
                                }
                              }}
                              className="rounded border-zinc-700 bg-zinc-900"
                            />
                            <span>{m.display_name}</span>
                          </label>
                        ))
                      ) : (
                        <p className="text-xs text-zinc-500 italic">No models currently discovered.</p>
                      )}
                    </div>
                  )}

                  <div className="flex items-center justify-end gap-2 pt-2">
                    <button
                      type="button"
                      onClick={() => setShowNewKeyModal(false)}
                      className="px-3 py-1.5 rounded-lg text-xs text-zinc-400 hover:text-white cursor-pointer"
                    >
                      Cancel
                    </button>
                    <button
                      id="create-key-submit-btn"
                      type="submit"
                      disabled={!newKeyName.trim()}
                      className="px-4 py-1.5 bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-white rounded-lg text-xs font-semibold cursor-pointer"
                    >
                      Generate Key
                    </button>
                  </div>
                </form>
              </div>
            </div>
          )}

          {/* Modal: Revoke Key Confirmation */}
          {revokingKeyId && (
            <div className="fixed inset-0 bg-black/70 backdrop-blur-sm z-50 flex items-center justify-center p-4">
              <div className="bg-zinc-900 border border-zinc-800 rounded-2xl p-6 max-w-md w-full space-y-4 shadow-2xl">
                <h3 className="text-sm font-bold text-rose-300">Revoke API Key?</h3>
                <p className="text-xs text-zinc-300 leading-relaxed">
                  Are you sure you want to revoke this API key? Applications using this credential will immediately be rejected and cannot authenticate. This action cannot be reversed.
                </p>
                <div className="flex items-center justify-end gap-2 pt-2">
                  <button
                    type="button"
                    onClick={() => setRevokingKeyId(null)}
                    className="px-3 py-1.5 rounded-lg text-xs text-zinc-400 hover:text-white cursor-pointer"
                  >
                    Cancel
                  </button>
                  <button
                    id="confirm-revoke-key-btn"
                    onClick={() => handleRevokeKey(revokingKeyId)}
                    className="px-4 py-1.5 bg-rose-600 hover:bg-rose-500 text-white rounded-lg text-xs font-semibold cursor-pointer"
                  >
                    Revoke Key
                  </button>
                </div>
              </div>
            </div>
          )}
        </section>

        {/* SECTION 3: AI Playground (Brick 2A & 2B) */}
        <section aria-labelledby="playground-heading" className="border-t border-zinc-800/80 pt-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <h2 id="playground-heading" className="text-xs font-semibold uppercase tracking-wider text-zinc-400">
                AI Playground
              </h2>
              <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-indigo-500/10 text-indigo-400 border border-indigo-500/20">
                Test Inference
              </span>
            </div>
            {isGenerating && (
              <span className="inline-flex items-center gap-1.5 text-xs text-indigo-400 animate-pulse font-medium">
                <span className="w-2 h-2 rounded-full bg-indigo-400" />
                Generating response...
              </span>
            )}
          </div>

          <form onSubmit={handleGenerate} className="space-y-4">
            {/* Model Selector */}
            <div>
              <label htmlFor="model-selector" className="block text-xs font-medium text-zinc-300 mb-1.5">
                Model:
              </label>
              <div className="relative">
                <select
                  id="model-selector"
                  value={selectedModelId}
                  onChange={(e) => setSelectedModelId(e.target.value)}
                  disabled={!hasAvailableModels || isGenerating}
                  className="w-full bg-zinc-950/80 border border-zinc-800 rounded-xl px-3.5 py-2.5 text-sm text-zinc-100 focus:outline-none focus:border-indigo-500 focus:ring-1 focus:ring-indigo-500 disabled:opacity-50 disabled:cursor-not-allowed appearance-none cursor-pointer font-mono"
                >
                  {hasAvailableModels ? (
                    discoveryData?.models.map((model) => (
                      <option key={model.id} value={model.id}>
                        {model.display_name} ({model.provider_id})
                      </option>
                    ))
                  ) : (
                    <option value="">No models available — connect Ollama</option>
                  )}
                </select>
                <div className="pointer-events-none absolute inset-y-0 right-0 flex items-center px-3.5 text-zinc-500">
                  <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 9l-7 7-7-7" />
                  </svg>
                </div>
              </div>
            </div>

            {/* Prompt Input */}
            <div>
              <label htmlFor="prompt-input" className="block text-xs font-medium text-zinc-300 mb-1.5">
                Prompt:
              </label>
              <textarea
                id="prompt-input"
                rows={3}
                value={promptText}
                onChange={(e) => setPromptText(e.target.value)}
                placeholder={
                  hasAvailableModels
                    ? "Explain machine learning in one sentence."
                    : "Connect Ollama with a downloaded model to enter prompts."
                }
                disabled={!hasAvailableModels || isGenerating}
                className="w-full bg-zinc-950/80 border border-zinc-800 rounded-xl px-3.5 py-2.5 text-sm text-zinc-100 placeholder-zinc-500 focus:outline-none focus:border-indigo-500 focus:ring-1 focus:ring-indigo-500 disabled:opacity-50 disabled:cursor-not-allowed resize-none transition-colors"
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    handleGenerate();
                  }
                }}
              />
              <div className="flex items-center justify-between mt-1 text-[11px] text-zinc-500">
                <span>Press Cmd+Enter or Ctrl+Enter to submit</span>
                <span>{promptText.length} characters</span>
              </div>
            </div>

            {/* Action Buttons: Generate & Stop Generation */}
            <div className="flex items-center justify-between gap-3">
              <label className="flex items-center gap-2 cursor-pointer text-[11px] text-zinc-400 select-none">
                <input
                  type="checkbox"
                  checked={isStreamingMode}
                  onChange={(e) => setIsStreamingMode(e.target.checked)}
                  disabled={isGenerating}
                  className="rounded border-zinc-700 bg-zinc-900 text-indigo-600 focus:ring-0 cursor-pointer"
                />
                <span>Stream response in real time</span>
              </label>

              <div className="flex items-center gap-2">
                {isGenerating && (
                  <button
                    type="button"
                    id="stop-generation-btn"
                    onClick={handleCancel}
                    className="inline-flex items-center gap-1.5 px-4 py-2 rounded-xl bg-rose-600 hover:bg-rose-500 active:bg-rose-700 text-white text-xs font-semibold transition-all cursor-pointer shadow-md shadow-rose-900/20"
                  >
                    <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
                    </svg>
                    <span>Stop Generation</span>
                  </button>
                )}

                <button
                  type="submit"
                  id="generate-response-btn"
                  disabled={!hasAvailableModels || isGenerating || !promptText.trim()}
                  className="inline-flex items-center gap-2 px-5 py-2 rounded-xl bg-indigo-600 hover:bg-indigo-500 active:bg-indigo-700 disabled:bg-zinc-800 disabled:text-zinc-500 text-white text-xs font-semibold transition-all cursor-pointer disabled:cursor-not-allowed shadow-md shadow-indigo-900/20"
                >
                  {isGenerating ? (
                    <>
                      <svg className="w-3.5 h-3.5 animate-spin text-white" fill="none" viewBox="0 0 24 24">
                        <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                        <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" />
                      </svg>
                      <span>Generating...</span>
                    </>
                  ) : (
                    <>
                      <svg className="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 10V3L4 14h7v7l9-11h-7z" />
                      </svg>
                      <span>Generate Response</span>
                    </>
                  )}
                </button>
              </div>
            </div>
          </form>

          {/* Playground Error Display */}
          {generationError && (
            <div
              id="playground-error-display"
              className="mt-4 p-4 rounded-xl bg-rose-950/40 border border-rose-800/50 text-rose-200 text-xs flex flex-col gap-1 transition-all"
            >
              <div className="font-semibold flex items-center gap-2 text-rose-300">
                <svg className="w-4 h-4 text-rose-400 flex-shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
                </svg>
                <span>{generationError.message}</span>
              </div>
              {generationError.technical_details && (
                <p className="text-[11px] text-rose-300/70 font-mono mt-0.5 break-words">
                  {generationError.technical_details}
                </p>
              )}
            </div>
          )}

          {/* Playground Response Display */}
          {(streamedContent || generationResult || isGenerating || generationStatus === "cancelled") && (
            <div
              id="playground-response-display"
              className="mt-4 p-4 rounded-xl bg-zinc-950/90 border border-zinc-800/90 transition-all space-y-3"
            >
              <div className="flex items-center justify-between border-b border-zinc-800/80 pb-2.5">
                <div className="flex items-center gap-2">
                  <span className="text-xs font-semibold text-zinc-300">Response:</span>
                  {generationStatus === "generating" && (
                    <span className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-[10px] font-semibold bg-indigo-500/10 text-indigo-400 border border-indigo-500/20 animate-pulse">
                      <span className="w-1.5 h-1.5 rounded-full bg-indigo-400 animate-ping" />
                      Streaming...
                    </span>
                  )}
                  {generationStatus === "completed" && (
                    <span className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-[10px] font-semibold bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
                      <span className="w-1.5 h-1.5 rounded-full bg-emerald-400" />
                      Completed
                    </span>
                  )}
                  {generationStatus === "cancelled" && (
                    <span className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-[10px] font-semibold bg-amber-500/10 text-amber-400 border border-amber-500/20">
                      <span className="w-1.5 h-1.5 rounded-full bg-amber-400" />
                      Generation stopped
                    </span>
                  )}
                  {generationStatus === "error" && (
                    <span className="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-[10px] font-semibold bg-rose-500/10 text-rose-400 border border-rose-500/20">
                      <span className="w-1.5 h-1.5 rounded-full bg-rose-400" />
                      Inference error
                    </span>
                  )}
                </div>

                <button
                  type="button"
                  id="copy-response-btn"
                  disabled={isGenerating || !streamedContent}
                  onClick={() => copyToClipboard(streamedContent, "response")}
                  className="text-xs text-zinc-400 hover:text-indigo-400 disabled:opacity-40 disabled:hover:text-zinc-400 transition-colors cursor-pointer"
                >
                  {copiedKey === "response" ? <span className="text-emerald-400">Copied</span> : "Copy"}
                </button>
              </div>

              <div
                id="playground-response-content"
                className="text-xs leading-relaxed text-zinc-200 whitespace-pre-wrap font-sans min-h-[2.5rem]"
              >
                {streamedContent || (isGenerating ? (
                  <span className="text-zinc-500 italic">Waiting for initial token...</span>
                ) : (
                  <span className="text-zinc-500 italic">No output</span>
                ))}
                {isGenerating && (
                  <span className="inline-block w-1.5 h-3.5 bg-indigo-400 ml-0.5 align-middle animate-pulse" />
                )}
              </div>

              <div className="pt-2 border-t border-zinc-800/60 flex flex-wrap items-center justify-between gap-2 text-[11px] font-mono text-zinc-400">
                <div className="flex items-center gap-3">
                  <span>
                    Model:{" "}
                    <strong className="text-indigo-300 font-semibold">
                      {(activeModelId || selectedModelId).replace(/^ollama\//, "")}
                    </strong>
                  </span>
                  {generationResult?.duration_ms != null && (
                    <>
                      <span>·</span>
                      <span>
                        Duration:{" "}
                        <strong className="text-zinc-200">{formatDuration(generationResult.duration_ms)}</strong>
                      </span>
                    </>
                  )}
                  {generationStatus === "cancelled" && (
                    <>
                      <span>·</span>
                      <span className="text-amber-400/90 italic">Stopped by user</span>
                    </>
                  )}
                </div>

                {generationResult?.usage && (
                  <div className="text-[10px] text-zinc-500">
                    {generationResult.usage.prompt_tokens != null && (
                      <span>Prompt: {generationResult.usage.prompt_tokens} tokens </span>
                    )}
                    {generationResult.usage.completion_tokens != null && (
                      <span>· Eval: {generationResult.usage.completion_tokens} tokens</span>
                    )}
                  </div>
                )}
              </div>
            </div>
          )}
        </section>

        {/* SECTION 3: Local AI & Models (Brick 1) */}
        <div className="border-t border-zinc-800/80 pt-6">
          <div className="flex items-center justify-between mb-3">
            <div className="flex items-center gap-2">
              <h2 className="text-xs font-semibold uppercase tracking-wider text-zinc-400">
                Discovered Models
              </h2>
              {providerStatus === "ready" && (
                <span className="px-2 py-0.5 rounded-full text-[10px] font-semibold bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
                  {discoveryData?.models.length} ready
                </span>
              )}
            </div>

            <button
              id="refresh-ollama-btn"
              onClick={refreshOllama}
              disabled={isRefreshingOllama || discoveryLoading}
              className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-zinc-800 hover:bg-zinc-700 active:bg-zinc-600 text-zinc-200 text-xs font-medium transition-colors cursor-pointer disabled:opacity-50"
            >
              <svg
                className={`w-3 h-3 ${isRefreshingOllama ? "animate-spin" : ""}`}
                fill="none"
                viewBox="0 0 24 24"
                stroke="currentColor"
              >
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
              </svg>
              <span>{isRefreshingOllama ? "Refreshing..." : "Refresh Models"}</span>
            </button>
          </div>

          {/* Model Status Views */}
          {providerStatus === "checking" && (
            <div
              id="ollama-status-checking"
              className="p-4 rounded-xl bg-zinc-950/60 border border-zinc-800/80 flex items-center gap-3.5"
            >
              <div className="w-4 h-4 border-2 border-indigo-400 border-t-transparent rounded-full animate-spin flex-shrink-0" />
              <div>
                <p className="text-xs font-medium text-zinc-200">Looking for local AI models...</p>
                <p className="text-[11px] text-zinc-400 mt-0.5">Connecting to Ollama at http://127.0.0.1:11434</p>
              </div>
            </div>
          )}

          {providerStatus === "ready" && discoveryData && (
            <div id="ollama-status-ready" className="space-y-2">
              <div className="flex items-center gap-2 px-1 mb-1">
                <span className="w-2 h-2 rounded-full bg-emerald-400" />
                <span className="text-xs font-medium text-emerald-400">Ollama connected</span>
              </div>

              <div id="discovered-models-list" className="space-y-1.5 max-h-48 overflow-y-auto pr-1">
                {discoveryData.models.map((model) => (
                  <div
                    key={model.id}
                    className="p-3 rounded-xl bg-zinc-950/60 border border-zinc-800/80 hover:border-zinc-700/80 transition-colors flex items-center justify-between"
                  >
                    <div>
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-semibold text-zinc-100 font-mono">
                          {model.display_name}
                        </span>
                        <span className="px-1.5 py-0.5 rounded text-[9px] uppercase font-mono font-bold bg-zinc-800 text-zinc-400">
                          {model.provider_id}
                        </span>
                      </div>
                      <div className="text-[10px] text-zinc-400 mt-0.5 flex items-center gap-2">
                        {model.parameter_size && <span>Parameters: {model.parameter_size}</span>}
                        {model.quantization_level && (
                          <>
                            <span className="text-zinc-600">·</span>
                            <span>Quant: {model.quantization_level}</span>
                          </>
                        )}
                        {model.size_bytes && (
                          <>
                            <span className="text-zinc-600">·</span>
                            <span>Size: {formatBytes(model.size_bytes)}</span>
                          </>
                        )}
                      </div>
                    </div>

                    <div className="text-right">
                      <span className="inline-block px-2 py-0.5 rounded text-[10px] font-medium bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
                        Available
                      </span>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {providerStatus === "no_models" && (
            <div
              id="ollama-status-no-models"
              className="p-4 rounded-xl bg-zinc-950/60 border border-amber-900/40 text-amber-200/90"
            >
              <div className="flex items-center gap-2 font-medium text-xs text-amber-300">
                <span className="w-2 h-2 rounded-full bg-amber-400" />
                <span>Ollama is running, but no models were found.</span>
              </div>
              <p className="text-[11px] text-zinc-400 mt-1.5">
                Download a model in your terminal using: <code className="text-indigo-300 font-mono bg-zinc-900 px-1 py-0.5 rounded">ollama pull llama3.2</code>, then click Refresh Models.
              </p>
            </div>
          )}

          {providerStatus === "unreachable" && (
            <div
              id="ollama-status-unreachable"
              className="p-4 rounded-xl bg-zinc-950/60 border border-zinc-800/80"
            >
              <div className="flex items-center gap-2 font-medium text-xs text-zinc-300">
                <span className="w-2 h-2 rounded-full bg-zinc-500" />
                <span>Ollama couldn't be reached.</span>
              </div>
              <p className="text-[11px] text-zinc-400 mt-1 leading-relaxed">
                Ollama does not appear to be running on port 11434. If Ollama is installed, launch it in the background and click <strong className="text-zinc-300">Refresh Models</strong>.
              </p>
            </div>
          )}

          {providerStatus === "error" && (
            <div
              id="ollama-status-error"
              className="p-4 rounded-xl bg-rose-950/30 border border-rose-900/40 text-rose-200"
            >
              <div className="flex items-center gap-2 font-medium text-xs text-rose-300">
                <span className="w-2 h-2 rounded-full bg-rose-400" />
                <span>Ollama discovery encountered an issue</span>
              </div>
              <p className="text-[11px] text-rose-300/80 mt-1">
                {discoveryData?.error || "An error occurred while communicating with Ollama."}
              </p>
            </div>
          )}
        </div>

        {/* Footer info */}
        <footer className="pt-4 border-t border-zinc-800/60 flex items-center justify-between text-[11px] text-zinc-500 font-mono">
          <span>mlue v0.1.0</span>
          <span>{lastRefreshed ? `Checked ${lastRefreshed.toLocaleTimeString()}` : ""}</span>
        </footer>
      </div>
    </div>
  );
}
