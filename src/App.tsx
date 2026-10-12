import { useCallback, useEffect, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
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

function formatBytes(bytes: number | null): string | null {
  if (bytes == null || bytes === 0) return null;
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
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

  // Terminal state auto-transition: If still checking, poll every 500ms until reaching terminal state
  useEffect(() => {
    if (discoveryData?.status !== "checking") return;

    const interval = setInterval(() => {
      fetchDiscoveredModels();
    }, 500);

    return () => clearInterval(interval);
  }, [discoveryData?.status, fetchDiscoveredModels]);

  // When runtime is running, verify health
  useEffect(() => {
    if (runtimeStatus?.state === "running") {
      testHealth(runtimeStatus.endpoint);
    }
  }, [runtimeStatus?.state, runtimeStatus?.endpoint, testHealth]);

  const copyToClipboard = (text: string, key: string) => {
    navigator.clipboard.writeText(text);
    setCopiedKey(key);
    setTimeout(() => setCopiedKey(null), 2000);
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

  return (
    <div className="min-h-screen bg-zinc-950 text-zinc-100 font-sans flex flex-col items-center justify-center p-6 selection:bg-indigo-500/30 selection:text-indigo-200">
      <div className="w-full max-w-xl bg-zinc-900/80 border border-zinc-800 rounded-2xl p-8 shadow-2xl backdrop-blur-xl transition-all">
        {/* Header */}
        <header className="flex items-center justify-between border-b border-zinc-800/80 pb-6 mb-6">
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
            className="mb-6 p-4 rounded-xl bg-rose-950/40 border border-rose-800/50 text-rose-200 text-sm flex flex-col gap-1"
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
        <div className="space-y-3 mb-6">
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
                className="mt-2.5 pt-2.5 border-t border-zinc-800/60 text-[11px] font-mono text-emerald-400/90 flex items-center justify-between"
              >
                <span>{JSON.stringify(healthData)}</span>
                <span className="text-zinc-500 text-[10px]">HTTP 200</span>
              </div>
            )}
          </div>
        </div>

        {/* SECTION 2: Local AI & Models (Brick 1) */}
        <div className="border-t border-zinc-800/80 pt-6">
          <div className="flex items-center justify-between mb-3">
            <div className="flex items-center gap-2">
              <h2 className="text-xs font-semibold uppercase tracking-wider text-zinc-400">
                Local AI Models
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
              className="p-5 rounded-xl bg-zinc-950/60 border border-zinc-800/80 flex items-center gap-3.5"
            >
              <div className="w-4 h-4 border-2 border-indigo-400 border-t-transparent rounded-full animate-spin flex-shrink-0" />
              <div>
                <p className="text-sm font-medium text-zinc-200">Looking for local AI models...</p>
                <p className="text-xs text-zinc-400 mt-0.5">Connecting to Ollama at http://127.0.0.1:11434</p>
              </div>
            </div>
          )}

          {providerStatus === "ready" && discoveryData && (
            <div id="ollama-status-ready" className="space-y-3">
              <div className="flex items-center gap-2 px-1">
                <span className="w-2 h-2 rounded-full bg-emerald-400" />
                <span className="text-xs font-medium text-emerald-400">Ollama connected</span>
              </div>

              <div id="discovered-models-list" className="space-y-2">
                {discoveryData.models.map((model) => (
                  <div
                    key={model.id}
                    className="p-3.5 rounded-xl bg-zinc-950/60 border border-zinc-800/80 hover:border-zinc-700/80 transition-colors flex items-center justify-between"
                  >
                    <div>
                      <div className="flex items-center gap-2">
                        <span className="text-sm font-semibold text-zinc-100 font-mono">
                          {model.display_name}
                        </span>
                        <span className="px-1.5 py-0.5 rounded text-[10px] uppercase font-mono font-bold bg-zinc-800 text-zinc-400">
                          {model.provider_id}
                        </span>
                      </div>
                      <div className="text-[11px] text-zinc-400 mt-1 flex items-center gap-2">
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
                      <span className="inline-block px-2 py-0.5 rounded text-[11px] font-medium bg-indigo-500/10 text-indigo-300 border border-indigo-500/20">
                        Ready
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
              className="p-5 rounded-xl bg-zinc-950/60 border border-amber-900/40 text-amber-200/90"
            >
              <div className="flex items-center gap-2 font-medium text-sm text-amber-300">
                <span className="w-2 h-2 rounded-full bg-amber-400" />
                <span>Ollama is running, but no models were found.</span>
              </div>
              <p className="text-xs text-zinc-400 mt-2">
                Download a model in your terminal using: <code className="text-indigo-300 font-mono bg-zinc-900 px-1.5 py-0.5 rounded">ollama pull llama3.2</code>, then click Refresh.
              </p>
            </div>
          )}

          {providerStatus === "unreachable" && (
            <div
              id="ollama-status-unreachable"
              className="p-5 rounded-xl bg-zinc-950/60 border border-zinc-800/80"
            >
              <div className="flex items-center gap-2 font-medium text-sm text-zinc-300">
                <span className="w-2 h-2 rounded-full bg-zinc-500" />
                <span>Ollama couldn't be reached.</span>
              </div>
              <p className="text-xs text-zinc-400 mt-1.5 leading-relaxed">
                Ollama does not appear to be running on port 11434. If Ollama is installed, launch it in the background and click <strong className="text-zinc-300">Refresh Models</strong>.
              </p>
            </div>
          )}

          {providerStatus === "error" && (
            <div
              id="ollama-status-error"
              className="p-5 rounded-xl bg-rose-950/30 border border-rose-900/40 text-rose-200"
            >
              <div className="flex items-center gap-2 font-medium text-sm text-rose-300">
                <span className="w-2 h-2 rounded-full bg-rose-400" />
                <span>Ollama discovery encountered an issue</span>
              </div>
              <p className="text-xs text-rose-300/80 mt-1.5">
                {discoveryData?.error || "An error occurred while communicating with Ollama."}
              </p>
            </div>
          )}
        </div>

        {/* Footer info */}
        <footer className="mt-6 pt-4 border-t border-zinc-800/60 flex items-center justify-between text-[11px] text-zinc-500 font-mono">
          <span>mlue v0.1.0</span>
          <span>{lastRefreshed ? `Checked ${lastRefreshed.toLocaleTimeString()}` : ""}</span>
        </footer>
      </div>
    </div>
  );
}
