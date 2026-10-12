import { useCallback, useEffect, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";

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

export default function App() {
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [lastRefreshed, setLastRefreshed] = useState<Date | null>(null);
  const [healthData, setHealthData] = useState<HealthResponse | null>(null);
  const [healthLoading, setHealthLoading] = useState<boolean>(false);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);

  const fetchStatus = useCallback(async () => {
    setLoading(true);
    try {
      if (isTauri()) {
        const status = await invoke<RuntimeStatus>("get_runtime_status");
        setRuntimeStatus(status);
      } else {
        // Fallback for non-Tauri preview environments
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
      setLoading(false);
    }
  }, []);

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

  useEffect(() => {
    fetchStatus();
  }, [fetchStatus]);

  // When status becomes running, perform an initial health check
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

  return (
    <div className="min-h-screen bg-zinc-950 text-zinc-100 font-sans flex flex-col items-center justify-center p-6 selection:bg-indigo-500/30 selection:text-indigo-200">
      <div className="w-full max-w-lg bg-zinc-900/80 border border-zinc-800 rounded-2xl p-8 shadow-2xl backdrop-blur-xl transition-all">
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

        {/* Error Alert Box */}
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

        {/* Runtime Information Cards */}
        <div className="space-y-4">
          {/* Status Row */}
          <div className="p-4 rounded-xl bg-zinc-950/60 border border-zinc-800/80 flex items-center justify-between">
            <span className="text-xs font-medium uppercase tracking-wider text-zinc-400">
              Runtime Status
            </span>
            <span
              id="runtime-status-value"
              className="text-sm font-semibold capitalize text-zinc-200"
            >
              {currentState}
            </span>
          </div>

          {/* Local API Endpoint */}
          <div className="p-4 rounded-xl bg-zinc-950/60 border border-zinc-800/80">
            <div className="flex items-center justify-between mb-1.5">
              <span className="text-xs font-medium uppercase tracking-wider text-zinc-400">
                Local API
              </span>
              <button
                id="copy-endpoint-btn"
                onClick={() => copyToClipboard(endpoint, "endpoint")}
                className="text-xs text-zinc-400 hover:text-indigo-400 transition-colors inline-flex items-center gap-1 cursor-pointer"
                title="Copy endpoint"
              >
                {copiedKey === "endpoint" ? (
                  <span className="text-emerald-400">Copied</span>
                ) : (
                  <span>Copy</span>
                )}
              </button>
            </div>
            <div
              id="local-api-endpoint"
              className="font-mono text-sm text-indigo-300 select-all break-all"
            >
              {endpoint}
            </div>
          </div>

          {/* Health Endpoint */}
          <div className="p-4 rounded-xl bg-zinc-950/60 border border-zinc-800/80">
            <div className="flex items-center justify-between mb-1.5">
              <span className="text-xs font-medium uppercase tracking-wider text-zinc-400">
                Health
              </span>
              <div className="flex items-center gap-2">
                <button
                  id="test-health-btn"
                  onClick={() => testHealth(endpoint)}
                  disabled={healthLoading}
                  className="text-xs text-zinc-400 hover:text-indigo-400 transition-colors inline-flex items-center gap-1 cursor-pointer disabled:opacity-50"
                  title="Test /health"
                >
                  {healthLoading ? "Pinging..." : "Test"}
                </button>
                <span className="text-zinc-600">·</span>
                <button
                  id="copy-health-btn"
                  onClick={() => copyToClipboard(healthUrl, "health")}
                  className="text-xs text-zinc-400 hover:text-indigo-400 transition-colors inline-flex items-center gap-1 cursor-pointer"
                  title="Copy health URL"
                >
                  {copiedKey === "health" ? (
                    <span className="text-emerald-400">Copied</span>
                  ) : (
                    <span>Copy</span>
                  )}
                </button>
              </div>
            </div>
            <div
              id="health-endpoint"
              className="font-mono text-sm text-indigo-300 select-all break-all"
            >
              {healthUrl}
            </div>

            {/* Health payload preview */}
            {healthData && (
              <div
                id="health-response-preview"
                className="mt-3 pt-3 border-t border-zinc-800/60 text-xs font-mono text-emerald-400/90 flex items-center justify-between"
              >
                <span>Response: {JSON.stringify(healthData)}</span>
                <span className="text-zinc-500 text-[10px]">HTTP 200</span>
              </div>
            )}
          </div>
        </div>

        {/* Action Controls */}
        <div className="mt-6 pt-4 border-t border-zinc-800/60 flex items-center justify-between">
          <div className="text-[11px] text-zinc-400 font-mono">
            {lastRefreshed ? `Checked ${lastRefreshed.toLocaleTimeString()}` : "Not checked yet"}
          </div>

          <button
            id="refresh-status-btn"
            onClick={fetchStatus}
            disabled={loading}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-xl bg-zinc-800 hover:bg-zinc-700 active:bg-zinc-600 text-zinc-200 text-xs font-semibold transition-colors cursor-pointer disabled:opacity-50"
          >
            <svg
              className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`}
              fill="none"
              viewBox="0 0 24 24"
              stroke="currentColor"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
              />
            </svg>
            <span>{loading ? "Refreshing..." : "Refresh Status"}</span>
          </button>
        </div>
      </div>
    </div>
  );
}
